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
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::config::schema::GithubConfig;
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
    /// Which repository in it, by directory name. Only needed when it holds
    /// several.
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
/// layer. The push happens in a clone the session never wrote, and anything
/// that reads a commit reads it there too, where the repository's
/// configuration is gone rather than merely overridden. See [`push_work`].
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

/// The upstream this work came from, read from the remote the agent cloned.
pub async fn upstream(run: &Run, project_path: &str) -> Result<Repo, PullRequestError> {
    let remote = git(run, project_path, &["remote", "get-url", "origin"], None).await;
    if remote.code != 0 {
        return Err(PullRequestError(
            "this project has no origin remote to open a pull request against".to_owned(),
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

fn repositories_in(project_path: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(project_path) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| is_work_tree(&Path::new(project_path).join(name)))
        .collect();
    names.sort();
    names
}

/// Finds the working tree the pull request is for.
///
/// A session's directory is not itself a repository. Whatever the agent was
/// asked to work on is cloned into it, so the repository is normally one level
/// down, and looking only at the top would report that there is nothing to
/// open when there plainly is.
///
/// `named` picks between them, which a session reused under the same name for
/// a while needs.
pub fn find_repository(
    project_path: &str,
    named: Option<&str>,
) -> Result<String, PullRequestError> {
    if let Some(named) = named.filter(|name| !name.is_empty()) {
        if named.contains('/') || named == "." || named == ".." {
            return Err(PullRequestError(format!(
                "`{named}` is not the name of a repository in this session"
            )));
        }
        let chosen = Path::new(project_path).join(named);
        if !is_work_tree(&chosen) {
            return Err(PullRequestError(format!(
                "there is no repository called `{named}` in this session"
            )));
        }
        return Ok(chosen.display().to_string());
    }

    if is_work_tree(Path::new(project_path)) {
        return Ok(project_path.to_owned());
    }

    let found = repositories_in(project_path);
    match found.as_slice() {
        [one] => Ok(Path::new(project_path).join(one).display().to_string()),
        [] => Err(PullRequestError(
            "nothing in this session is a git repository yet, so there is nothing to open"
                .to_owned(),
        )),
        many => Err(PullRequestError(format!(
            "this session holds several repositories ({}), so say which one to open",
            many.join(", ")
        ))),
    }
}

/// Longest wait for a new fork to appear, and how often it is looked for.
pub const FORK_WAIT_MS: u64 = 30_000;
const FORK_POLL_MS: u64 = 1_000;

/// Whether the account this token belongs to can already push to the target.
///
/// GitHub refuses to fork a repository into the account that owns it, so a
/// session working in one of the bot's own repositories could never open a
/// pull request: the fork answered 403 and the session reported that it could
/// not fork. Asked first, so the fork is attempted only where it is possible.
async fn can_push_to(api: &Api, token: &str, target: &Repo) -> bool {
    let answer = api(
        format!("/repos/{}/{}", target.owner, target.name),
        ApiCall {
            method: "GET".to_owned(),
            token: token.to_owned(),
            body: None,
        },
    )
    .await;
    answer.status == 200
        && answer
            .body
            .get("permissions")
            .and_then(|permissions| permissions.get("push"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
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

/// The bot's fork of the upstream, once it is there to push to.
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

/// What the upstream merges into, which is not always `main`.
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
/// So the commit is pushed from a bare repository made here, holding one ref
/// and a pointer to the session's objects. Objects are data and are only ever
/// read; the configuration, which is code, is left behind. The session never
/// learns of this directory and cannot write to it.
async fn push_work(
    run: &Run,
    project_path: &str,
    branch: &str,
    url: &str,
    token: &str,
) -> Result<String, PullRequestError> {
    let staging = tempfile::tempdir()
        .map_err(|error| PullRequestError(format!("could not prepare the push: {error}")))?;
    let repository = staging.path().join("repository.git");

    // A clone takes the refs and leaves the configuration: the copy gets a
    // fresh one, and hooks are never carried over. `--shared` borrows the
    // objects rather than copying them, so a long history costs nothing here,
    // and objects are read-only data in any case.
    let cloned = git(
        run,
        staging.path().to_str().unwrap_or_default(),
        &[
            "clone",
            "--shared",
            "--bare",
            "--quiet",
            project_path,
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
/// The fork is made first and pushed to, rather than pushing to the upstream:
/// a bot that never needs write access to somebody else's repository cannot
/// lose it.
pub async fn open_pull_request(
    request: &Request,
    run: &Run,
    api: &Api,
    sleep: &Sleep,
) -> Result<String, PullRequestError> {
    let project_path = find_repository(&request.project_path, request.repository.as_deref())?;
    assert_contained(run, &project_path).await?;
    let branch = current_branch(run, &project_path).await?;
    let target = upstream(run, &project_path).await?;

    // Forking is for somebody else's repository. On one the bot can already
    // push to, the branch goes straight to the upstream and the request is a
    // same-repository one; the least-privilege reason for forking stands
    // everywhere it applies, which is everywhere the bot has no write access.
    let own = can_push_to(api, &request.github.token, &target).await;
    let pushing_to = if own {
        target.clone()
    } else {
        fork_of(api, &request.github.token, &target, sleep).await?
    };

    let summary = push_work(
        run,
        &project_path,
        &branch,
        &format!(
            "https://github.com/{}/{}.git",
            pushing_to.owner, pushing_to.name
        ),
        &request.github.token,
    )
    .await?;

    let created = api(
        format!("/repos/{}/{}/pulls", target.owner, target.name),
        ApiCall {
            method: "POST".to_owned(),
            token: request.github.token.clone(),
            body: Some(make_pull(
                &request.title,
                (!own).then_some(pushing_to.owner.as_str()),
                &branch,
                &default_branch(api, &request.github.token, &target).await,
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
