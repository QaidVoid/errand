//! Opening a pull request for a session's work.
//!
//! Done by the daemon rather than by the agent, so what a pull request says
//! about where it came from is not left to a model's discretion. An
//! instruction in a prompt is advice: one was seen keeping the two lines that
//! read as useful and dropping the third as redundant. This composes the body
//! itself.
//!
//! The agent does hold a token, because reading issues and checking builds
//! needs one, so this is a route rather than a wall. What it buys is that the
//! ordinary path produces a correct description every time, and that the
//! daemon is the one that reports where the pull request went.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::config::schema::GithubConfig;
use crate::sandbox::backend::WORKSPACE_PATH;
use crate::sandbox::paths;
use crate::session::github::{SessionLinks, attribution_footer};

/// A repository on GitHub, as the API addresses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The account or organisation the repository sits under.
    pub owner: String,
    /// The repository's own name.
    pub name: String,
}

/// Reads the owner and repository out of a remote URL.
///
/// Both forms are accepted because a project may have been cloned either way,
/// and the daemon has no say in which.
///
/// Returns None for anything that is not GitHub, since there is nothing
/// useful to do with it here.
pub fn parse_remote(url: &str) -> Option<Repo> {
    let trimmed = url.trim();
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);

    if let Some(rest) = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
    {
        // An optional userinfo stands before the first slash, as in a token
        // embedded by a service that cloned on the agent's behalf.
        let rest = match rest.find('@') {
            Some(at) if rest[..at].find('/').is_none() => &rest[at + 1..],
            _ => rest,
        };
        let (host, path) = rest.split_once('/')?;
        if host != "github.com" {
            return None;
        }
        return owner_name(path);
    }

    let rest = trimmed.strip_prefix("ssh://").unwrap_or(trimmed);
    let rest = rest.strip_prefix("git@github.com")?;
    let rest = rest.strip_prefix(':').or_else(|| rest.strip_prefix('/'))?;
    owner_name(rest)
}

fn owner_name(path: &str) -> Option<Repo> {
    let (owner, name) = path.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(Repo {
        owner: owner.to_owned(),
        name: name.to_owned(),
    })
}

/// What a command said.
#[derive(Debug, Clone, PartialEq)]
pub struct Ran {
    /// The command's exit status.
    pub code: i32,
    /// What it wrote to standard output.
    pub stdout: String,
    /// What it wrote to standard error.
    pub stderr: String,
}

/// Runs a command. Injected so tests need no repository and no network.
pub type Run = Arc<
    dyn Fn(
            Vec<String>,
            Option<String>,
            BTreeMap<String, String>,
        ) -> Pin<Box<dyn Future<Output = Ran> + Send>>
        + Send
        + Sync,
>;

/// Waits. Injected so a test does not sit through a fork appearing.
pub type Sleep = Arc<dyn Fn(u64) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// One call to the GitHub API.
pub struct ApiCall {
    /// The HTTP method, as the API expects it.
    pub method: String,
    /// The credential the call is made with.
    pub token: String,
    /// The JSON body, for a call that carries one.
    pub body: Option<Value>,
}

/// What the API said.
pub struct ApiReply {
    /// The HTTP status.
    pub status: u16,
    /// The parsed JSON body, or an empty object when it was not JSON.
    pub body: Value,
}

/// Calls the GitHub API. Injected for the same reason.
pub type Api =
    Arc<dyn Fn(String, ApiCall) -> Pin<Box<dyn Future<Output = ApiReply> + Send>> + Send + Sync>;

/// What went wrong, in words worth posting into a thread.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PullRequestError(pub String);

/// What a session produced, and where it should go.
pub struct Request {
    /// How this host reaches GitHub, and who it pushes as.
    pub github: GithubConfig,
    /// The session's directory, which holds the repository rather than being
    /// one.
    pub project_path: String,
    /// Which repository in it, by its path within it. Only needed when it
    /// holds several.
    pub repository: Option<String>,
    /// Title for the pull request.
    pub title: String,
    /// Who asked, and where the conversation is.
    pub requested_by: String,
    /// Where the conversation is, for the body to point back at.
    pub links: SessionLinks,
}

/// The body the daemon writes, which is the whole point of doing this here.
pub fn pull_request_body(summary: &str, requested_by: &str, links: &SessionLinks) -> String {
    format!(
        "{}\n\n---\n\n{}\n",
        summary.trim(),
        attribution_footer(requested_by, links)
    )
}

/// Pushes without the token ever reaching a command line.
///
/// A URL carrying it would appear in `ps` for anyone on the host, and in git's
/// own error output. A credential helper reads it from the environment
/// instead.
const TOKEN_VARIABLE: &str = "ERRAND_GH_TOKEN";
const CREDENTIAL_HELPER: &str =
    "!f() { echo \"username=x-access-token\"; echo \"password=$ERRAND_GH_TOKEN\"; }; f";

/// Settings a repository must not be allowed to supply.
///
/// These commands run on the host, outside the sandbox, against a tree the
/// session can write. Git treats a repository as a source of code as much as
/// of data: a hook, a credential helper, a filesystem monitor are all commands
/// it will run on the daemon's behalf. Naming each one on the command line
/// beats whatever the repository says, since a `-c` is read last.
///
/// Overriding named settings is a denylist, and a denylist against git is a
/// losing game: signature verification runs `gpg.program`, a textconv filter
/// runs its command, and the list grows with git. So this is only the first
/// layer. Everything the daemon reads is read from its own copy of the git
/// directory, checked to borrow nothing, and the push happens in a fresh
/// clone of that copy, whose own configuration is git's default. See
/// [`push_work`].
const SAFE_CONFIG: &[&str] = &[
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.pager=cat",
    "-c",
    "credential.helper=",
    "-c",
    "http.proxy=",
    "-c",
    "http.sslVerify=true",
    "-c",
    "protocol.ext.allow=never",
    // A signed commit is verified by running the configured "gpg", so a
    // repository that says what that program is runs it. Nothing here needs a
    // signature checked, so none is.
    "-c",
    "log.showSignature=false",
    "-c",
    "merge.verifySignatures=false",
];

/// The environment git is given, so the host's own configuration cannot join
/// in.
///
/// The system and global files are the caller's rather than the repository's,
/// but neither is wanted here: this runs one known operation and should behave
/// the same on every host. A prompt would hang a daemon nobody is watching.
const SAFE_ENV: &[(&str, &str)] = &[
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_TERMINAL_PROMPT", "0"),
];

fn git(
    run: &Run,
    cwd: &str,
    args: &[&str],
    token: Option<&str>,
) -> Pin<Box<dyn Future<Output = Ran> + Send>> {
    let mut env: BTreeMap<String, String> = SAFE_ENV
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    if let Some(token) = token {
        env.insert(TOKEN_VARIABLE.to_owned(), token.to_owned());
    }
    let mut command = vec!["git".to_owned()];
    command.extend(SAFE_CONFIG.iter().map(|setting| (*setting).to_owned()));
    command.extend(args.iter().map(|argument| (*argument).to_owned()));
    run(command, Some(cwd.to_owned()), env)
}

/// Refuses a repository whose git directory is not inside it.
///
/// git will follow a `gitdir:` pointer, a symlink, or an alternate out of the
/// tree, and the daemon then reads and pushes whatever is at the other end. So
/// git is asked where it would actually look, and an answer outside the
/// repository is refused rather than operated on. `--absolute-git-dir`
/// resolves a path and runs nothing the repository could name.
async fn assert_contained(run: &Run, repo: &str) -> Result<(), PullRequestError> {
    let dir = git(run, repo, &["rev-parse", "--absolute-git-dir"], None).await;
    if dir.code != 0 {
        return Err(PullRequestError(format!(
            "there is no git repository at {repo}"
        )));
    }
    let git_dir = dir.stdout.trim();
    let inside = git_dir == format!("{repo}/.git")
        || git_dir.starts_with(&format!("{repo}/.git/"))
        || git_dir == repo
        || git_dir.starts_with(&format!("{repo}/"));
    if !inside {
        return Err(PullRequestError(
            "this project's git directory is outside it, so it is not one the daemon will open"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Copies a repository's git directory into the daemon's staging directory,
/// and returns the copy's work tree path.
///
/// Walked from the session's project root without following a link, since
/// the session can rearrange its tree while the copy is made; see
/// [`paths::copy_tree_beneath`]. A link inside `.git` is left out of the copy,
/// so nothing it points at is read.
async fn snapshot_git_directory(
    session_root: &str,
    project_path: &str,
    staging: &std::path::Path,
) -> Result<String, PullRequestError> {
    let relative = project_path
        .strip_prefix(session_root)
        .unwrap_or(project_path)
        .trim_start_matches('/');
    let git_directory = if relative.is_empty() {
        ".git".to_owned()
    } else {
        format!("{relative}/.git")
    };
    let snapshot = staging.join("repository");
    std::fs::create_dir(&snapshot)
        .map_err(|error| PullRequestError(format!("could not prepare the push: {error}")))?;
    let session_root = session_root.to_owned();
    let destination = snapshot.join(".git");
    let copied = tokio::task::spawn_blocking(move || {
        paths::copy_tree_beneath(
            &session_root,
            &git_directory,
            &destination,
            GIT_DIRECTORY_DEPTH,
            MAX_GIT_DIRECTORY_BYTES,
        )
    })
    .await
    .map_err(|error| PullRequestError(format!("could not prepare the push: {error}")))?;
    copied.map_err(|error| {
        PullRequestError(format!(
            "could not copy the repository at {project_path} to push it: {error}"
        ))
    })?;
    Ok(snapshot.display().to_string())
}

/// How deep a git directory is copied before the copy is refused.
const GIT_DIRECTORY_DEPTH: usize = 16;

/// The most of a git directory copied for a push.
///
/// The copy sits in the host's temp directory, often memory, and the
/// session decides how big its `.git` is.
const MAX_GIT_DIRECTORY_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Refuses a repository that borrows objects or configuration from
/// somewhere else.
///
/// The session writes its own `.git`, so each of these can make a push carry
/// another repository's commits under the daemon's token, or make the daemon
/// read refs or configuration from elsewhere: an alternates file, a
/// `commondir`, a configuration include, a ref store named by path, or a
/// partial clone's promisor remote. Checked on the daemon's copy, which the
/// session cannot change after, and the configuration is listed by git
/// itself, so what is checked is what git would act on.
async fn refuse_borrowed_store(
    run: &Run,
    snapshot: &str,
    project_path: &str,
) -> Result<(), PullRequestError> {
    let git_directory = std::path::Path::new(snapshot).join(".git");
    let refused = |why: &str| {
        PullRequestError(format!(
            "the repository at {project_path} is not one the daemon will push: {why}"
        ))
    };
    if !git_directory.is_dir() {
        return Err(PullRequestError(format!(
            "could not prepare the push: the copy of {project_path} holds no git directory"
        )));
    }
    for name in ["alternates", "http-alternates"] {
        if std::fs::symlink_metadata(git_directory.join("objects/info").join(name)).is_ok() {
            return Err(refused(&format!(
                "it borrows objects from outside itself through .git/objects/info/{name}. \
                 A clone made with --shared or --reference keeps its own objects after \
                 `git repack -a -d` and deleting that file"
            )));
        }
    }
    if std::fs::symlink_metadata(git_directory.join("commondir")).is_ok() {
        return Err(refused(
            "its .git/commondir points its git directory somewhere else",
        ));
    }
    // Run from the staging root, which is not a repository, so git reads
    // only the file it is pointed at and nothing it would discover itself.
    let staging = std::path::Path::new(snapshot)
        .parent()
        .map_or_else(String::new, |parent| parent.display().to_string());
    for name in ["config", "config.worktree"] {
        let file = git_directory.join(name);
        if std::fs::symlink_metadata(&file).is_err() {
            continue;
        }
        let file = file.display().to_string();
        let listed = git(
            run,
            &staging,
            &["config", "--file", &file, "--no-includes", "--list"],
            None,
        )
        .await;
        if listed.code != 0 {
            return Err(refused(&format!(
                ".git/{name} is not configuration git can read: {}",
                first_line(&listed.stderr)
            )));
        }
        if let Some(setting) = listed.stdout.lines().find_map(borrowing_setting) {
            return Err(refused(&format!(".git/{name} {setting}")));
        }
    }
    Ok(())
}

/// What a configuration line, as git lists it, borrows from elsewhere, if
/// anything.
fn borrowing_setting(line: &str) -> Option<String> {
    let (key, value) = line.split_once('=').unwrap_or((line, ""));
    let key = key.to_ascii_lowercase();
    if key.starts_with("include.") || key.starts_with("includeif.") {
        return Some(format!(
            "includes {value}, configuration from outside the repository"
        ));
    }
    if key == "extensions.partialclone"
        || (key.starts_with("remote.")
            && (key.ends_with(".promisor") || key.ends_with(".partialclonefilter")))
    {
        return Some(
            "makes it a partial clone, which fetches what it lacks from another \
             repository. Fetch the whole history first, or clone without --filter"
                .to_owned(),
        );
    }
    if key == "extensions.refstorage" && value.contains("://") {
        return Some(format!("keeps its refs at {value}, outside the repository"));
    }
    None
}

/// The branch the work is on, refusing a detached head.
pub async fn current_branch(run: &Run, project_path: &str) -> Result<String, PullRequestError> {
    let head = git(
        run,
        project_path,
        &["rev-parse", "--abbrev-ref", "HEAD"],
        None,
    )
    .await;
    if head.code != 0 {
        return Err(PullRequestError(format!(
            "git could not read a branch in {project_path}"
        )));
    }
    let branch = head.stdout.trim();
    if branch == "HEAD" {
        return Err(PullRequestError(
            "this project has no branch checked out".to_owned(),
        ));
    }
    Ok(branch.to_owned())
}

/// The repository the work was cloned from, which origin names.
///
/// Not always where a pull request from it belongs: a session that cloned its
/// fork of something still means the work for that something, and the opening
/// reads the fork relationship off GitHub rather than assuming it.
pub async fn origin_remote(run: &Run, project_path: &str) -> Result<Repo, PullRequestError> {
    let remote = git(run, project_path, &["remote", "get-url", "origin"], None).await;
    if remote.code != 0 {
        return Err(PullRequestError(
            "this project has no origin remote, so there is nowhere to open a pull request from"
                .to_owned(),
        ));
    }
    parse_remote(&remote.stdout).ok_or_else(|| {
        PullRequestError(format!(
            "origin is not a GitHub remote: {}",
            remote.stdout.trim()
        ))
    })
}

/// Whether a directory is a working tree the daemon will operate on.
///
/// A real `.git` directory, and nothing else. A `.git` that is a file holds a
/// `gitdir:` line pointing elsewhere, and one that is a symlink points
/// elsewhere too; both are how a session makes the daemon read and push a
/// repository outside its own tree, which is somewhere on the host the session
/// cannot otherwise reach. A checkout errand made is an ordinary clone, whose
/// `.git` is a directory, so nothing legitimate is turned away.
fn is_work_tree(path: &Path) -> bool {
    std::fs::symlink_metadata(path.join(".git")).is_ok_and(|meta| meta.is_dir())
}

/// How many levels below the session's directory a repository is looked for.
///
/// Deep enough for a clone placed the way its URL reads, as
/// `github.com/owner/repo`, and shallow enough that a session holding a large
/// tree is not walked whole.
const SEARCH_DEPTH: usize = 4;

/// Every working tree under the session's directory, by its path within it.
///
/// A tree found is not searched further, so a repository vendored inside
/// another is not offered as a second one. A symlink is never followed, for
/// the reason a `.git` file is refused, and a hidden directory is skipped,
/// since that is where tool caches live.
fn repositories_in(project_path: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut unread = vec![(PathBuf::from(project_path), String::new(), 0)];
    while let Some((directory, within, depth)) = unread.pop() {
        if depth == SEARCH_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let path = entry.path();
            let within = if within.is_empty() {
                name
            } else {
                format!("{within}/{name}")
            };
            if is_work_tree(&path) {
                found.push(within);
            } else {
                unread.push((path, within, depth + 1));
            }
        }
    }
    found.sort();
    found
}

/// The directory a repository named by its path within the session is in.
///
/// Every step must be a real directory beneath the session's own, so a
/// symlink or a `..` cannot name a repository elsewhere on the host. The path
/// may be written as the agent sees it, under the workspace.
fn named_repository(project_path: &str, named: &str) -> Result<PathBuf, PullRequestError> {
    let within = named
        .strip_prefix(&format!("{WORKSPACE_PATH}/"))
        .unwrap_or(named)
        .trim_end_matches('/');
    let mut path = PathBuf::from(project_path);
    for part in within.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(PullRequestError(format!(
                "`{named}` is not a path within this session; name a repository as \
                 `github.com/owner/repo`, relative to the workspace"
            )));
        }
        path.push(part);
        if !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
            return Err(PullRequestError(format!(
                "there is no repository at `{named}` in this session"
            )));
        }
    }
    if is_work_tree(&path) {
        Ok(path)
    } else {
        Err(PullRequestError(format!(
            "there is no repository at `{named}` in this session"
        )))
    }
}

/// Finds the working tree the pull request is for.
///
/// A session's directory is not itself a repository. Whatever the agent was
/// asked to work on is cloned into it, often one level down and sometimes
/// placed the way its URL reads, so it is looked for a few levels down, and
/// looking only at the top would report that there is nothing to open when
/// there plainly is.
///
/// `named` picks between them by path within the session, which a session
/// reused under the same name for a while needs.
pub fn find_repository(
    project_path: &str,
    named: Option<&str>,
) -> Result<String, PullRequestError> {
    if let Some(named) = named.map(str::trim).filter(|name| !name.is_empty()) {
        return named_repository(project_path, named)
            .map(|path| path.display().to_string())
            .map_err(|error| {
                let found = repositories_in(project_path);
                if found.is_empty() {
                    error
                } else {
                    PullRequestError(format!(
                        "{error}; name one by its directory under {WORKSPACE_PATH}: {}",
                        found.join(", ")
                    ))
                }
            });
    }

    if is_work_tree(Path::new(project_path)) {
        return Ok(project_path.to_owned());
    }

    let found = repositories_in(project_path);
    match found.as_slice() {
        [one] => Ok(Path::new(project_path).join(one).display().to_string()),
        [] => Err(PullRequestError(format!(
            "nothing in this session is a git repository yet, looking {SEARCH_DEPTH} levels \
             down, so there is nothing to open"
        ))),
        many => Err(PullRequestError(format!(
            "this session holds several repositories ({}), so say which one to open",
            many.join(", ")
        ))),
    }
}

/// Longest wait for a new fork to appear, and how often it is looked for.
pub const FORK_WAIT_MS: u64 = 30_000;
const FORK_POLL_MS: u64 = 1_000;

/// What one look at the cloned repository says about opening the work.
struct Looked {
    /// Whether the account this token belongs to can push to it.
    can_push: bool,
    /// The repository this one was forked from, when it is a fork. Used only
    /// when the bot can push to the fork itself: a fork the bot owns exists
    /// to carry work upstream, so the parent, and not the fork, is where the
    /// work is meant to land.
    parent: Option<Repo>,
}

/// Looks at the repository the session cloned, before anything is pushed.
///
/// One answer carries both facts the opening turns on: whether the bot can
/// push to the clone itself, and, when the clone is of a fork, where the work
/// is meant for. Whether a fork is possible is read here too, since GitHub
/// refuses to fork a repository into the account that owns it, and a session
/// working in one of the bot's own repositories would otherwise only learn
/// that from a 403 after being told the opening failed.
async fn look_at(api: &Api, token: &str, repo: &Repo) -> Looked {
    let answer = api(
        format!("/repos/{}/{}", repo.owner, repo.name),
        ApiCall {
            method: "GET".to_owned(),
            token: token.to_owned(),
            body: None,
        },
    )
    .await;
    Looked {
        can_push: answer.status == 200
            && answer
                .body
                .get("permissions")
                .and_then(|permissions| permissions.get("push"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        parent: answer.body.get("parent").and_then(named),
    }
}

/// Reads a repository out of an API answer that describes one.
fn named(body: &Value) -> Option<Repo> {
    let owner = body.get("owner")?.get("login")?.as_str()?;
    Some(Repo {
        owner: owner.to_owned(),
        name: body.get("name")?.as_str()?.to_owned(),
    })
}

fn detail(body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .map_or_else(|| "GitHub refused it".to_owned(), str::to_owned)
}

fn first_line(text: &str) -> String {
    text.trim()
        .split_once('\n')
        .map_or(text.trim(), |(first, _)| first)
        .to_owned()
}

/// The bot's fork of the repository the work is for, once it is there to
/// push to.
///
/// Where it lands is read from GitHub rather than assumed. The name is not
/// always the upstream's, since an account already holding a repository of
/// that name gets the fork under a different one, and the owner is a login,
/// which is not what the configured author name has to be.
///
/// Waiting is the other half. Forking is asynchronous and answered before it
/// has finished, and pushing inside that window fails as though the repository
/// did not exist.
async fn fork_of(
    api: &Api,
    token: &str,
    target: &Repo,
    sleep: &Sleep,
) -> Result<Repo, PullRequestError> {
    let made = api(
        format!("/repos/{}/{}/forks", target.owner, target.name),
        ApiCall {
            method: "POST".to_owned(),
            token: token.to_owned(),
            body: None,
        },
    )
    .await;
    if made.status >= 400 {
        // A repository the token cannot see is reported as missing rather than
        // as forbidden, so the two are worth naming together.
        let reach = if made.status == 403 || made.status == 404 {
            ", which the bot's token may not have access to"
        } else {
            ""
        };
        return Err(PullRequestError(format!(
            "could not fork {}/{}{}: {}",
            target.owner,
            target.name,
            reach,
            detail(&made.body)
        )));
    }

    let fork = named(&made.body).ok_or_else(|| {
        PullRequestError("GitHub accepted the fork but did not say where it put it".to_owned())
    })?;

    let deadline = Instant::now() + Duration::from_millis(FORK_WAIT_MS);
    loop {
        let there = api(
            format!("/repos/{}/{}", fork.owner, fork.name),
            ApiCall {
                method: "GET".to_owned(),
                token: token.to_owned(),
                body: None,
            },
        )
        .await;
        if there.status == 200 {
            return Ok(fork);
        }
        if Instant::now() >= deadline {
            return Err(PullRequestError(format!(
                "the fork {}/{} did not become available to push to",
                fork.owner, fork.name
            )));
        }
        sleep(FORK_POLL_MS).await;
    }
}

/// What the base repository merges into, which is not always `main`.
async fn default_branch(api: &Api, token: &str, repo: &Repo) -> String {
    let answer = api(
        format!("/repos/{}/{}", repo.owner, repo.name),
        ApiCall {
            method: "GET".to_owned(),
            token: token.to_owned(),
            body: None,
        },
    )
    .await;
    answer
        .body
        .get("default_branch")
        .and_then(Value::as_str)
        .unwrap_or("main")
        .to_owned()
}

/// Pushes the session's commit from a repository it never had a chance to
/// write.
///
/// The push is the one step that carries the token and opens a connection, and
/// it is the step git hangs the most on a repository's own configuration: a
/// `pre-push` hook, a credential helper named for a single URL, an `insteadOf`
/// that sends the whole thing somewhere else. A session owns its working tree,
/// so against that tree none of those can be trusted, and the last two cannot
/// be overridden from the command line at all.
///
/// So the commit is pushed from a bare clone made here of the daemon's own
/// copy of the git directory, which borrows that copy's objects. The clone's
/// configuration is git's default, not the repository's. The session never
/// learns of either directory and cannot write to them.
async fn push_work(
    run: &Run,
    staging: &std::path::Path,
    snapshot: &str,
    branch: &str,
    url: &str,
    token: &str,
) -> Result<String, PullRequestError> {
    let repository = staging.join("repository.git");

    // A clone takes the refs and leaves the configuration: the copy gets a
    // fresh one, and hooks are never carried over. `--shared` borrows the
    // objects of the daemon's own snapshot rather than copying them again,
    // which the session cannot reach.
    let cloned = git(
        run,
        staging.to_str().unwrap_or_default(),
        &[
            "clone",
            "--shared",
            "--bare",
            "--quiet",
            snapshot,
            &repository.display().to_string(),
        ],
        None,
    )
    .await;
    if cloned.code != 0 {
        return Err(PullRequestError(format!(
            "could not prepare the push: {}",
            first_line(&cloned.stderr)
        )));
    }

    let pushed = git(
        run,
        &repository.display().to_string(),
        &[
            "-c",
            &format!("credential.helper={CREDENTIAL_HELPER}"),
            "push",
            "--force-with-lease",
            url,
            &format!("refs/heads/{branch}:refs/heads/{branch}"),
        ],
        Some(token),
    )
    .await;
    if pushed.code != 0 {
        return Err(PullRequestError(format!(
            "could not push {branch}: {}",
            first_line(&pushed.stderr)
        )));
    }

    // Read here, where the configuration is the clone's own rather than the
    // session's. Reading it in the session's tree is what let a crafted commit
    // and a `gpg.program` in its config run on the host: `git log` verifies a
    // signature by running that program. The clone carries neither.
    let summary = git(
        run,
        &repository.display().to_string(),
        &["log", "-1", "--format=%b"],
        None,
    )
    .await;
    Ok(summary.stdout)
}

/// Opens the pull request, and returns where it is.
///
/// The work is meant for the repository the session cloned. Where the bot
/// cannot push to it, that repository is forked, the branch lives on the
/// fork, and the request is opened against the original. A repository the
/// bot can push to that is itself a fork defers to its parent instead: a
/// fork the bot owns exists to carry work upstream, so a session that
/// cloned its own fork still lands the work where the fork came from.
pub async fn open_pull_request(
    request: &Request,
    run: &Run,
    api: &Api,
    sleep: &Sleep,
) -> Result<String, PullRequestError> {
    let project_path = find_repository(&request.project_path, request.repository.as_deref())?;
    // The session can write its tree while this runs, so nothing below reads
    // it after this copy: what is checked is what is pushed.
    let staging = tempfile::tempdir()
        .map_err(|error| PullRequestError(format!("could not prepare the push: {error}")))?;
    let staging_root = std::fs::canonicalize(staging.path())
        .map_err(|error| PullRequestError(format!("could not prepare the push: {error}")))?;
    let snapshot =
        snapshot_git_directory(&request.project_path, &project_path, &staging_root).await?;
    refuse_borrowed_store(run, &snapshot, &project_path).await?;
    assert_contained(run, &snapshot).await?;
    let branch = current_branch(run, &snapshot).await?;
    let origin = origin_remote(run, &snapshot).await?;

    let looked = look_at(api, &request.github.token, &origin).await;
    let (base, pushing_to) = if looked.can_push {
        (
            looked.parent.unwrap_or_else(|| origin.clone()),
            origin.clone(),
        )
    } else {
        let fork = fork_of(api, &request.github.token, &origin, sleep).await?;
        (origin.clone(), fork)
    };

    let summary = push_work(
        run,
        &staging_root,
        &snapshot,
        &branch,
        &format!(
            "https://github.com/{}/{}.git",
            pushing_to.owner, pushing_to.name
        ),
        &request.github.token,
    )
    .await?;

    let created = api(
        format!("/repos/{}/{}/pulls", base.owner, base.name),
        ApiCall {
            method: "POST".to_owned(),
            token: request.github.token.clone(),
            body: Some(make_pull(
                &request.title,
                (pushing_to != base).then_some(pushing_to.owner.as_str()),
                &branch,
                &default_branch(api, &request.github.token, &base).await,
                &pull_request_body(&summary, &request.requested_by, &request.links),
            )),
        },
    )
    .await;
    if created.status >= 400 {
        return Err(PullRequestError(format!(
            "could not open the pull request: {}",
            detail(&created.body)
        )));
    }

    created
        .body
        .get("html_url")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            PullRequestError("the pull request was created but GitHub did not say where".to_owned())
        })
}

/// The request body, with `head` naming the fork only when there is one.
///
/// A same-repository request names the branch alone. Qualifying it with the
/// owner is what a cross-repository request needs, and GitHub refuses the
/// qualified form when the head and the base are the same repository.
fn make_pull(title: &str, fork_owner: Option<&str>, branch: &str, base: &str, body: &str) -> Value {
    #[derive(Serialize)]
    struct NewPull<'a> {
        title: &'a str,
        head: String,
        base: &'a str,
        body: &'a str,
        maintainer_can_modify: bool,
    }
    json!(NewPull {
        title,
        head: match fork_owner {
            Some(owner) => format!("{owner}:{branch}"),
            None => branch.to_owned(),
        },
        base,
        body,
        maintainer_can_modify: true,
    })
}

/// Sleeps for real, between looks for a fork.
pub fn pause(ms: u64) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(tokio::time::sleep(Duration::from_millis(ms)))
}

/// Runs a command on the host, for the daemon's own git operations.
pub fn run_command(
    command: Vec<String>,
    cwd: Option<String>,
    env: BTreeMap<String, String>,
) -> Pin<Box<dyn Future<Output = Ran> + Send>> {
    Box::pin(async move {
        let mut names = command.into_iter();
        let program = names.next().unwrap_or_default();
        let mut process = tokio::process::Command::new(&program);
        process.args(names);
        process.stdout(std::process::Stdio::piped());
        process.stderr(std::process::Stdio::piped());
        if let Some(cwd) = cwd {
            process.current_dir(cwd);
        }
        // The daemon's environment holds the bot token and the chat one. Only
        // what is named here crosses into git.
        process.env_clear();
        process.env("PATH", std::env::var("PATH").unwrap_or_default());
        process.env("HOME", std::env::var("HOME").unwrap_or_default());
        for (name, value) in env {
            process.env(name, value);
        }
        let output = match process.output().await {
            Ok(output) => output,
            Err(error) => {
                return Ran {
                    code: 127,
                    stdout: String::new(),
                    stderr: error.to_string(),
                };
            }
        };
        Ran {
            code: output.status.code().unwrap_or(127),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    })
}

/// How the daemon names itself to GitHub.
///
/// GitHub refuses a request that carries no `User-Agent` with a 403 that
/// reads like a permissions failure. Deno's `fetch` sets one of its own, so
/// the TypeScript daemon never had to; `reqwest` sets none, and without this
/// every call fails, which looks from a thread like a repository the bot
/// cannot reach.
const USER_AGENT: &str = concat!("errand/", env!("CARGO_PKG_VERSION"));

/// The client the daemon calls GitHub with.
fn github_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(USER_AGENT)
        .build()
        .unwrap_or_default()
}

/// Calls the GitHub REST API as the bot.
pub fn call_api(path: String, init: ApiCall) -> Pin<Box<dyn Future<Output = ApiReply> + Send>> {
    Box::pin(async move {
        let client = github_client();
        let method =
            reqwest::Method::from_bytes(init.method.as_bytes()).unwrap_or(reqwest::Method::GET);
        let mut request = client
            .request(method, format!("https://api.github.com{path}"))
            .header("Authorization", format!("Bearer {}", init.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(body) = &init.body {
            request = request
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }
        match request.send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                let body = response.json::<Value>().await.unwrap_or_else(|_| json!({}));
                ApiReply { status, body }
            }
            Err(error) => ApiReply {
                status: 0,
                body: json!({ "message": error.to_string() }),
            },
        }
    })
}

#[cfg(test)]
mod tests;
