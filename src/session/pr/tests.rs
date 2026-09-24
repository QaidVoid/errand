use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::{
    Api, ApiCall, ApiReply, PullRequestError, Ran, Repo, Request, Run, Sleep, current_branch,
    find_repository, open_pull_request, parse_remote, pull_request_body, upstream,
};
use crate::config::schema::GithubConfig;
use crate::session::github::SessionLinks;

type Answer = (u16, serde_json::Value);

fn github() -> GithubConfig {
    GithubConfig {
        token: "ghp-value".to_owned(),
        user_name: "errand-bot".to_owned(),
        user_email: "bot@example.com".to_owned(),
    }
}

fn ok(stdout: &str) -> Ran {
    Ran {
        code: 0,
        stdout: stdout.to_owned(),
        stderr: String::new(),
    }
}

const KNOWN_GIT: [&str; 4] = ["rev-parse", "remote", "push", "log"];

/// A git that answers by subcommand, and records everything it was asked.
struct FakeGit {
    answers: BTreeMap<String, Ran>,
    calls: Mutex<Vec<RecordedCall>>,
}

struct RecordedCall {
    args: Vec<String>,
    env: Option<BTreeMap<String, String>>,
    cwd: Option<String>,
}

impl Clone for RecordedCall {
    fn clone(&self) -> Self {
        Self {
            args: self.args.clone(),
            env: self.env.clone(),
            cwd: self.cwd.clone(),
        }
    }
}

impl FakeGit {
    /// The git dir a real repository resolves to is inside it. A test that is
    /// proving a redirect out of the tree answers this itself.
    fn answer(&self, command: &[String], cwd: Option<&str>) -> Ran {
        if command.iter().any(|word| word == "--absolute-git-dir")
            && !self.answers.contains_key("--absolute-git-dir")
        {
            return ok(&format!("{}/.git\n", cwd.unwrap_or_default()));
        }
        let subcommand = command
            .iter()
            .find(|word| self.answers.contains_key(*word) || KNOWN_GIT.contains(&word.as_str()))
            .map(String::as_str)
            .unwrap_or_default();
        self.answers
            .get(subcommand)
            .cloned()
            .unwrap_or_else(|| ok(""))
    }

    fn call_pushing(&self) -> Option<RecordedCall> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .find(|call| call.args.iter().any(|word| word == "push"))
            .cloned()
    }
}

fn fake_git(answers: BTreeMap<String, Ran>) -> (Arc<FakeGit>, Run) {
    let fake = Arc::new(FakeGit {
        answers,
        calls: Mutex::new(Vec::new()),
    });
    let runner = Arc::clone(&fake);
    let run: Run = Arc::new(move |command, cwd, env| {
        let fake = Arc::clone(&runner);
        Box::pin(async move {
            fake.calls.lock().unwrap().push(RecordedCall {
                args: command.clone(),
                env: Some(env),
                cwd: cwd.clone(),
            });
            fake.answer(&command, cwd.as_deref())
        })
    });
    (fake, run)
}

fn plain_git() -> (Arc<FakeGit>, Run) {
    fake_git(BTreeMap::new())
}

struct FakeApi {
    answers: BTreeMap<String, Answer>,
    paths: Mutex<Vec<String>>,
    /// The JSON bodies sent, so a test can assert on what was asked for.
    bodies: Mutex<Vec<serde_json::Value>>,
}

fn fake_api(answers: BTreeMap<String, Answer>) -> (Arc<FakeApi>, Api) {
    let fake = Arc::new(FakeApi {
        answers,
        paths: Mutex::new(Vec::new()),
        bodies: Mutex::new(Vec::new()),
    });
    let caller = Arc::clone(&fake);
    let api: Api = Arc::new(move |path, init| {
        let fake = Arc::clone(&caller);
        Box::pin(async move {
            fake.paths
                .lock()
                .unwrap()
                .push(format!("{} {}", init.method, path));
            if let Some(body) = &init.body {
                fake.bodies.lock().unwrap().push(body.clone());
            }
            let (status, body) = fake
                .answers
                .get(&format!("{} {}", init.method, path))
                .cloned()
                .unwrap_or((200, serde_json::json!({})));
            ApiReply { status, body }
        })
    });
    (fake, api)
}

fn forked() -> Answer {
    (
        202,
        serde_json::json!({"name": "project-1", "owner": {"login": "errand-bot-login"}}),
    )
}

fn working_api() -> (Arc<FakeApi>, Api) {
    fake_api(BTreeMap::from([
        ("POST /repos/upstream/project/forks".to_owned(), forked()),
        (
            "GET /repos/errand-bot-login/project-1".to_owned(),
            (200, serde_json::json!({})),
        ),
        (
            "GET /repos/upstream/project".to_owned(),
            (200, serde_json::json!({"default_branch": "trunk"})),
        ),
        (
            "POST /repos/upstream/project/pulls".to_owned(),
            (
                201,
                serde_json::json!({"html_url": "https://github.com/upstream/project/pull/7"}),
            ),
        ),
    ]))
}

fn repo_git() -> (Arc<FakeGit>, Run) {
    fake_git(BTreeMap::from([
        ("rev-parse".to_owned(), ok("feature/thing\n")),
        (
            "remote".to_owned(),
            ok("https://github.com/upstream/project.git\n"),
        ),
        ("log".to_owned(), ok("what changed and why\n")),
    ]))
}

/// A session directory holding one repository, `project`.
struct WithRepo {
    /// Held for its lifetime: dropping it takes the directory with it.
    root: tempfile::TempDir,
    project: String,
    repo: String,
}

fn with_repo() -> WithRepo {
    let root = tempfile::tempdir().expect("a temp directory");
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join(".git")).expect("the repository is made");
    WithRepo {
        root,
        project: project.parent().unwrap().display().to_string(),
        repo: project.display().to_string(),
    }
}

fn immediate_sleep() -> Sleep {
    Arc::new(|ms: u64| {
        Box::pin(async move {
            let _ = ms;
        }) as Pin<Box<dyn Future<Output = ()> + Send>>
    })
}

fn request(project: &str, title: &str, asked: &str) -> Request {
    Request {
        github: github(),
        project_path: project.to_owned(),
        repository: None,
        title: title.to_owned(),
        requested_by: asked.to_owned(),
        links: SessionLinks::default(),
    }
}

#[test]
fn a_remote_is_read_in_either_form_it_was_cloned_in() {
    assert_eq!(
        parse_remote("https://github.com/owner/repo.git"),
        Some(Repo {
            owner: "owner".to_owned(),
            name: "repo".to_owned()
        })
    );
    assert_eq!(
        parse_remote("git@github.com:owner/repo.git"),
        Some(Repo {
            owner: "owner".to_owned(),
            name: "repo".to_owned()
        })
    );
    assert_eq!(
        parse_remote("https://x-access-token:tok@github.com/owner/repo"),
        Some(Repo {
            owner: "owner".to_owned(),
            name: "repo".to_owned()
        })
    );
}

#[test]
fn a_remote_that_is_not_github_is_not_a_repository_this_can_open() {
    assert_eq!(parse_remote("https://gitlab.com/owner/repo.git"), None);
    assert_eq!(parse_remote("/srv/git/local.git"), None);
    assert_eq!(parse_remote(""), None);
}

/// The session directory is not the repository: whatever was asked for is
/// cloned into it. Looking only at the top reported that there was nothing to
/// open when there plainly was.
#[test]
fn the_repository_is_found_one_level_down_from_the_session() {
    let kept = with_repo();
    assert_eq!(find_repository(&kept.project, None).unwrap(), kept.repo);
}

#[test]
fn a_session_that_is_itself_a_repository_is_the_repository() {
    let kept = with_repo();
    std::fs::create_dir_all(kept.root.path().join(".git")).unwrap();
    let project = kept.root.path().display().to_string();
    assert_eq!(find_repository(&project, None).unwrap(), project);
}

#[test]
fn several_repositories_are_named_so_somebody_can_say_which() {
    let kept = with_repo();
    std::fs::create_dir_all(kept.root.path().join("other").join(".git")).unwrap();

    let error = find_repository(&kept.project, None).unwrap_err();

    assert!(error.to_string().contains("other, project"));
    assert_eq!(
        find_repository(&kept.project, Some("other")).unwrap(),
        kept.root.path().join("other").display().to_string()
    );
}

#[test]
fn a_session_holding_no_repository_says_so_plainly() {
    let kept = with_repo();
    std::fs::remove_dir_all(&kept.repo).unwrap();

    let error = find_repository(&kept.project, None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("nothing in this session is a git repository")
    );
}

/// A `.git` file names a repository elsewhere, which is how a session makes
/// the daemon operate on a tree outside its own. Only a real `.git` directory,
/// which is what a clone makes, is a repository the daemon will open.
#[test]
fn a_directory_whose_git_is_a_redirect_file_is_not_a_repository() {
    let kept = with_repo();
    let linked = kept.root.path().join("linked");
    std::fs::create_dir(&linked).unwrap();
    std::fs::write(linked.join(".git"), "gitdir: /elsewhere\n").unwrap();

    assert!(find_repository(&kept.project, Some("linked")).is_err());
}

/// An agent that clones the way the URL reads puts the repository three
/// levels down, and it is found there.
#[test]
fn a_repository_placed_the_way_its_url_reads_is_found() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("github.com/pkgforge-dev/polyfill-glibc");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(root.path().join(".cache/deep/.git")).unwrap();
    let project = root.path().display().to_string();

    assert_eq!(
        find_repository(&project, None).unwrap(),
        repo.display().to_string()
    );
}

/// Several nested clones are named by their paths, and one is picked by its
/// path, written as the agent sees it or relative to the workspace.
#[test]
fn nested_repositories_are_named_and_picked_by_path() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("github.com/owner/one/.git")).unwrap();
    std::fs::create_dir_all(root.path().join("github.com/owner/two/.git")).unwrap();
    // Vendored inside one, so not a second repository to choose.
    std::fs::create_dir_all(root.path().join("github.com/owner/one/vendor/lib/.git")).unwrap();
    let project = root.path().display().to_string();

    let error = find_repository(&project, None).unwrap_err().to_string();
    assert!(
        error.contains("(github.com/owner/one, github.com/owner/two)"),
        "{error}"
    );

    let two = root
        .path()
        .join("github.com/owner/two")
        .display()
        .to_string();
    assert_eq!(
        find_repository(&project, Some("github.com/owner/two")).unwrap(),
        two
    );
    assert_eq!(
        find_repository(&project, Some("/workspace/github.com/owner/two/")).unwrap(),
        two
    );
    assert!(find_repository(&project, Some("github.com/owner")).is_err());
}

/// A symlink along the way could name a repository anywhere on the host, so
/// it is neither followed in the search nor accepted in a name.
#[test]
fn a_symlink_never_leads_to_a_repository() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(outside.path().join("repo/.git")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("hop")).unwrap();
    let project = root.path().display().to_string();

    assert!(find_repository(&project, None).is_err());
    assert!(find_repository(&project, Some("hop/repo")).is_err());
}

#[test]
fn a_repository_deeper_than_the_search_is_not_found() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("a/b/c/d/e/.git")).unwrap();
    let project = root.path().display().to_string();

    let error = find_repository(&project, None).unwrap_err().to_string();
    assert!(error.contains("levels down"), "{error}");
    assert!(find_repository(&project, Some("a/b/c/d/e")).is_ok());
}

#[test]
fn a_name_that_is_a_path_is_not_the_name_of_a_repository() {
    let kept = with_repo();

    for name in ["../escape", ".", ".."] {
        assert!(find_repository(&kept.project, Some(name)).is_err());
    }
}

#[tokio::test]
async fn a_detached_head_has_no_branch_to_open_a_request_for() {
    let (_fake, run) = fake_git(BTreeMap::from([("rev-parse".to_owned(), ok("HEAD\n"))]));

    let error = current_branch(&run, "/p").await.unwrap_err();
    assert!(error.to_string().contains("no branch checked out"));
}

#[tokio::test]
async fn a_project_with_no_origin_cannot_say_what_to_open_against() {
    let (_fake, run) = fake_git(BTreeMap::from([(
        "remote".to_owned(),
        Ran {
            code: 1,
            stdout: String::new(),
            stderr: "no such remote".to_owned(),
        },
    )]));

    let error = upstream(&run, "/p").await.unwrap_err();
    assert!(error.to_string().contains("no origin remote"));
}

#[test]
fn the_body_carries_the_summary_and_ends_with_the_attribution() {
    let body = pull_request_body(
        "  it does the thing  ",
        "amelia",
        &SessionLinks {
            thread: Some("https://t/1".to_owned()),
            transcript: None,
        },
    );

    assert!(body.contains("it does the thing"));
    assert!(body.contains("---"));
    assert!(body.contains("Requested by amelia via errand."));
    assert!(!body.contains('@'));
}

#[tokio::test]
async fn a_pull_request_is_opened_against_the_upstream_from_the_bots_fork() {
    let kept = with_repo();
    let (_fake, run) = repo_git();
    let (api_fake, api) = working_api();

    let url = open_pull_request(
        &request(&kept.project, "Do the thing", "amelia"),
        &run,
        &api,
        &immediate_sleep(),
    )
    .await
    .unwrap();

    assert_eq!(url, "https://github.com/upstream/project/pull/7");
    let paths = api_fake.paths.lock().unwrap().join("\n");
    assert!(paths.contains("POST /repos/upstream/project/forks"));
    assert!(paths.contains("POST /repos/upstream/project/pulls"));
}

/// The fork's own name and owner are read from the answer. An account that
/// already holds a repository of that name gets the fork under another, and
/// the owner is a login, which the configured author name need not be.
#[tokio::test]
async fn the_push_goes_to_the_fork_github_actually_made() {
    let kept = with_repo();
    let (fake, run) = repo_git();
    let (_api_fake, api) = working_api();

    open_pull_request(
        &request(&kept.project, "t", "amelia"),
        &run,
        &api,
        &immediate_sleep(),
    )
    .await
    .unwrap();

    let push = fake.call_pushing().expect("a push happened");
    let joined = push.args.join(" ");
    assert!(joined.contains("https://github.com/errand-bot-login/project-1.git"));
    assert!(joined.contains("refs/heads/feature/thing:refs/heads/feature/thing"));
}

/// In `ps` for anyone on the host, and in git's own error output.
#[tokio::test]
async fn the_token_is_passed_in_the_environment_never_on_a_command_line() {
    let kept = with_repo();
    let (fake, run) = repo_git();
    let (_api_fake, api) = working_api();

    open_pull_request(
        &request(&kept.project, "t", "amelia"),
        &run,
        &api,
        &immediate_sleep(),
    )
    .await
    .unwrap();

    let push = fake.call_pushing().expect("a push happened");
    let joined = push.args.join(" ");
    assert!(!joined.contains("ghp-value"));
    assert_eq!(
        push.env.as_ref().unwrap().get("ERRAND_GH_TOKEN"),
        Some(&"ghp-value".to_owned())
    );
    assert!(joined.contains("$ERRAND_GH_TOKEN"));
}

#[tokio::test]
async fn the_request_is_opened_against_whatever_the_upstream_merges_into() {
    let kept = with_repo();
    let (_fake, run) = repo_git();
    let (_api_fake, api) = fake_api(BTreeMap::from([
        ("POST /repos/upstream/project/forks".to_owned(), forked()),
        (
            "GET /repos/errand-bot-login/project-1".to_owned(),
            (200, serde_json::json!({})),
        ),
        (
            "GET /repos/upstream/project".to_owned(),
            (200, serde_json::json!({"default_branch": "develop"})),
        ),
        (
            "POST /repos/upstream/project/pulls".to_owned(),
            (201, serde_json::json!({"html_url": "https://x/1"})),
        ),
    ]));
    let opened = Arc::new(Mutex::new(serde_json::Value::Null));
    let watcher = Arc::clone(&opened);
    let watching: Api = Arc::new(move |path: String, init: ApiCall| {
        let watcher = Arc::clone(&watcher);
        let base = Arc::clone(&api);
        Box::pin(async move {
            if path.ends_with("/pulls") {
                *watcher.lock().unwrap() = init.body.clone().unwrap_or(serde_json::Value::Null);
            }
            base(path, init).await
        })
    });

    open_pull_request(
        &request(&kept.project, "t", "amelia"),
        &run,
        &watching,
        &immediate_sleep(),
    )
    .await
    .unwrap();

    let opened = opened.lock().unwrap().clone();
    assert_eq!(opened.get("base"), Some(&serde_json::json!("develop")));
    assert_eq!(
        opened.get("head"),
        Some(&serde_json::json!("errand-bot-login:feature/thing"))
    );
}

/// Forking is asynchronous and answered before it has finished. Pushing inside
/// that window fails as though the repository did not exist, which is what
/// this poll is for.
#[tokio::test]
async fn a_fork_that_is_not_there_yet_is_waited_for() {
    let kept = with_repo();
    let (_fake, run) = repo_git();
    let (_api_fake, api) = working_api();
    let looks = Arc::new(AtomicUsize::new(0));
    let waited = Arc::new(AtomicUsize::new(0));

    let watcher = Arc::clone(&looks);
    let looking: Api = Arc::new(move |path: String, init: ApiCall| {
        let base = Arc::clone(&api);
        let watcher = Arc::clone(&watcher);
        Box::pin(async move {
            if path == "/repos/errand-bot-login/project-1" && init.method == "GET" {
                let seen = watcher.fetch_add(1, Ordering::SeqCst) + 1;
                let status = if seen < 3 { 404 } else { 200 };
                return ApiReply {
                    status,
                    body: serde_json::json!({}),
                };
            }
            base(path, init).await
        })
    });
    let ticks = Arc::clone(&waited);
    let counting_sleep: Sleep = Arc::new(move |ms: u64| {
        let ticks = Arc::clone(&ticks);
        Box::pin(async move {
            ticks.fetch_add(1, Ordering::SeqCst);
            let _ = ms;
        }) as Pin<Box<dyn Future<Output = ()> + Send>>
    });

    open_pull_request(
        &request(&kept.project, "t", "amelia"),
        &run,
        &looking,
        &counting_sleep,
    )
    .await
    .unwrap();

    assert_eq!(looks.load(Ordering::SeqCst), 3);
    assert_eq!(waited.load(Ordering::SeqCst), 2);
}

/// A repository the token cannot see is reported as missing, not forbidden.
#[tokio::test]
async fn a_fork_that_is_refused_says_the_token_may_not_reach_the_repository() {
    let kept = with_repo();
    let (_fake, run) = repo_git();
    let (_api_fake, api) = fake_api(BTreeMap::from([(
        "POST /repos/upstream/project/forks".to_owned(),
        (404, serde_json::json!({"message": "Not Found"})),
    )]));

    let error = open_pull_request(
        &request(&kept.project, "t", "a"),
        &run,
        &api,
        &immediate_sleep(),
    )
    .await
    .unwrap_err();

    assert!(error.to_string().contains("may not have access"));
}

#[tokio::test]
async fn a_push_that_fails_says_why_in_one_line_worth_posting() {
    let kept = with_repo();
    let (_fake, run) = fake_git(BTreeMap::from([
        ("rev-parse".to_owned(), ok("feature/thing\n")),
        (
            "remote".to_owned(),
            ok("https://github.com/upstream/project.git\n"),
        ),
        (
            "push".to_owned(),
            Ran {
                code: 1,
                stdout: String::new(),
                stderr: "! [rejected] stale info\nhint: read this\n".to_owned(),
            },
        ),
    ]));
    let (_api_fake, api) = working_api();

    let error = open_pull_request(
        &request(&kept.project, "t", "a"),
        &run,
        &api,
        &immediate_sleep(),
    )
    .await
    .unwrap_err();

    assert!(error.to_string().contains("rejected"));
    assert!(!error.to_string().contains("hint:"));
}

#[tokio::test]
async fn a_request_github_refuses_is_reported_with_what_it_said() {
    let kept = with_repo();
    let (_fake, run) = repo_git();
    let (_api_fake, api) = fake_api(BTreeMap::from([
        ("POST /repos/upstream/project/forks".to_owned(), forked()),
        (
            "GET /repos/errand-bot-login/project-1".to_owned(),
            (200, serde_json::json!({})),
        ),
        (
            "POST /repos/upstream/project/pulls".to_owned(),
            (
                422,
                serde_json::json!({"message": "A pull request already exists"}),
            ),
        ),
    ]));

    let error = open_pull_request(
        &request(&kept.project, "t", "a"),
        &run,
        &api,
        &immediate_sleep(),
    )
    .await
    .unwrap_err();

    assert!(error.to_string().contains("already exists"));
}

/// The daemon's git runs on the host, outside the sandbox, against a tree the
/// session can write. A repository is code as much as data, so none of what it
/// says about commands to run may be honoured.
#[tokio::test]
async fn the_sessions_repository_cannot_make_the_daemon_run_anything() {
    let kept = with_repo();
    let (fake, run) = repo_git();
    let (_api_fake, api) = working_api();

    open_pull_request(
        &request(&kept.project, "t", "amelia"),
        &run,
        &api,
        &immediate_sleep(),
    )
    .await
    .unwrap();

    let calls = fake.calls.lock().unwrap();
    for call in calls.iter() {
        let line = call.args.join(" ");
        // Hooks are the direct route: a pre-push in the session's own tree.
        assert!(line.contains("core.hooksPath=/dev/null"));
        // A helper is a command too, and the repository's list is reset before
        // the daemon's own is added.
        assert!(line.contains("credential.helper="));
        // The host's own files are not consulted either.
        assert_eq!(
            call.env.as_ref().unwrap().get("GIT_CONFIG_NOSYSTEM"),
            Some(&"1".to_owned())
        );
        assert_eq!(
            call.env.as_ref().unwrap().get("GIT_CONFIG_GLOBAL"),
            Some(&"/dev/null".to_owned())
        );
    }

    // A helper named for one URL, and an insteadOf, cannot be reset from the
    // command line, so the push does not happen in the session's repository.
    let push = calls
        .iter()
        .find(|call| call.args.iter().any(|word| word == "push"))
        .expect("a push happened");
    assert!(!push.args.contains(&kept.repo));
    let everything = calls
        .iter()
        .map(|call| call.args.join(" "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(everything.contains("clone --shared --bare"));
}

/// A crafted commit and a `gpg.program` in a repository's own config ran on
/// the host when git verified the commit's signature. The commit is read from
/// the clone instead, whose configuration is git's own rather than the
/// session's.
#[tokio::test]
async fn the_commit_is_never_read_in_the_tree_the_session_can_write() {
    let kept = with_repo();
    let (fake, run) = repo_git();
    let (_api_fake, api) = working_api();

    open_pull_request(
        &request(&kept.project, "t", "amelia"),
        &run,
        &api,
        &immediate_sleep(),
    )
    .await
    .unwrap();

    let calls = fake.calls.lock().unwrap();
    // The clone happens in a temp dir, and the commit body is read there.
    let clone = calls
        .iter()
        .find(|call| call.args.iter().any(|word| word == "clone"))
        .expect("a clone happened");
    let staging_root = clone.cwd.as_ref().expect("a staging root");

    let log = calls
        .iter()
        .find(|call| call.args.iter().any(|word| word == "log"))
        .expect("a log happened");
    // Read under the staging root, and specifically not in the session's repo.
    assert!(log.cwd.as_ref().expect("a cwd").starts_with(staging_root));
    assert_ne!(log.cwd.as_deref(), Some(kept.repo.as_str()));
    assert_ne!(log.cwd.as_deref(), Some(kept.project.as_str()));

    // And every host-side git call refuses to verify a signature, which is
    // what invoked the program a repository could name.
    for call in calls.iter() {
        assert!(call.args.join(" ").contains("log.showSignature=false"));
    }
}

/// A `.git` that points out of the tree is how a session makes the daemon read
/// and push a repository elsewhere on the host. Only a real `.git` directory
/// is a repository the daemon will open.
#[tokio::test]
async fn a_project_whose_git_directory_is_a_redirect_file_is_not_opened() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    // The attacker's redirect: `.git` is a file naming a repo outside the tree.
    std::fs::write(project.join(".git"), "gitdir: /home/someone/private/.git\n").unwrap();

    let (_fake, run) = plain_git();
    let (api_fake, api) = working_api();

    let outcome = open_pull_request(
        &request(project.display().to_string().as_str(), "t", "amelia"),
        &run,
        &api,
        &immediate_sleep(),
    )
    .await;

    assert!(matches!(outcome, Err(PullRequestError(_))));
    // The daemon never forked or pushed anything.
    assert!(
        !api_fake
            .paths
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.contains("forks"))
    );
}

/// The backstop, for a git directory git itself would follow out of the tree.
#[tokio::test]
async fn a_git_directory_resolved_outside_the_repository_is_refused() {
    let kept = with_repo();
    let (_api_fake, api) = working_api();
    // git reports an absolute git dir that is not inside the repository.
    let (_fake, run) = fake_git(BTreeMap::from([
        (
            "--absolute-git-dir".to_owned(),
            ok("/etc/somewhere-else/.git\n"),
        ),
        (
            "remote".to_owned(),
            ok("https://github.com/upstream/project.git\n"),
        ),
    ]));

    let outcome = open_pull_request(
        &request(&kept.project, "t", "amelia"),
        &run,
        &api,
        &immediate_sleep(),
    )
    .await;

    assert!(matches!(outcome, Err(PullRequestError(_))));
    assert!(outcome.unwrap_err().to_string().contains("outside it"));
}

/// GitHub refuses a request with no `User-Agent`, and the refusal is a 403
/// that reads as a repository the bot cannot see. Every call has to carry one.
#[tokio::test]
async fn every_call_to_github_names_the_daemon() {
    let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&seen);
    let app = axum::Router::new().fallback(move |headers: axum::http::HeaderMap| {
        let captured = Arc::clone(&captured);
        async move {
            *captured.lock().unwrap() = headers
                .get(axum::http::header::USER_AGENT)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            "{}"
        }
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("a loopback port");
    let port = listener.local_addr().expect("a local address").port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("the server serves");
    });

    let sent = super::github_client()
        .get(format!("http://127.0.0.1:{port}/repos/someone/theirs"))
        .send()
        .await;
    assert!(sent.is_ok(), "the request reached the server");

    let named = seen.lock().unwrap().clone();
    assert_eq!(named.as_deref(), Some(super::USER_AGENT));
}

/// GitHub refuses to fork a repository into the account that owns it, so a
/// session working in one of the bot's own repositories could never open a
/// pull request. When the bot can already push, there is nothing to fork.
#[tokio::test]
async fn its_own_repository_is_pushed_to_directly_and_never_forked() {
    let kept = with_repo();
    let (git, run) = repo_git();
    let (api_fake, api) = fake_api(BTreeMap::from([
        (
            "GET /repos/upstream/project".to_owned(),
            (
                200,
                serde_json::json!({
                    "default_branch": "trunk",
                    "permissions": { "push": true },
                }),
            ),
        ),
        (
            "POST /repos/upstream/project/pulls".to_owned(),
            (
                201,
                serde_json::json!({"html_url": "https://github.com/upstream/project/pull/9"}),
            ),
        ),
    ]));

    let url = open_pull_request(
        &request(&kept.project, "Do the thing", "amelia"),
        &run,
        &api,
        &immediate_sleep(),
    )
    .await
    .expect("a pull request on the bot's own repository");

    assert_eq!(url, "https://github.com/upstream/project/pull/9");
    let paths = api_fake.paths.lock().unwrap().join("\n");
    assert!(!paths.contains("forks"), "nothing was forked: {paths}");

    // The branch went to the upstream itself.
    let pushed = git
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| call.args.join(" "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        pushed.contains("https://github.com/upstream/project.git"),
        "{pushed}"
    );

    // A same-repository request names the branch alone: GitHub refuses the
    // owner-qualified form when head and base are the same repository.
    let sent = api_fake.bodies.lock().unwrap().clone();
    let head = sent
        .iter()
        .find_map(|body| body.get("head").and_then(|head| head.as_str()))
        .expect("a head");
    assert!(!head.contains(':'), "head was qualified: {head}");
}

/// A repository the bot cannot push to is still forked first, which is the
/// whole reason the fork-first flow exists.
#[tokio::test]
async fn somebody_elses_repository_is_still_forked_first() {
    let kept = with_repo();
    let (_git, run) = repo_git();
    let (api_fake, api) = working_api();

    open_pull_request(
        &request(&kept.project, "Do the thing", "amelia"),
        &run,
        &api,
        &immediate_sleep(),
    )
    .await
    .expect("a pull request through the fork");

    let paths = api_fake.paths.lock().unwrap().join("\n");
    assert!(
        paths.contains("POST /repos/upstream/project/forks"),
        "{paths}"
    );
    let sent = api_fake.bodies.lock().unwrap().clone();
    let head = sent
        .iter()
        .find_map(|body| body.get("head").and_then(|head| head.as_str()))
        .expect("a head");
    assert!(
        head.contains(':'),
        "a cross-repository head is qualified: {head}"
    );
}
