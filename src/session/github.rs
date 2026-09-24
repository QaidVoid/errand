//! The GitHub identity a session pushes with.
//!
//! The agent gets the token, because reading issues, leaving comments and
//! checking a build are most of working on somebody's repository and none of
//! it is possible without one.
//!
//! Pull requests are the exception it is asked to route through the daemon: it
//! writes a title into its state directory and the daemon composes the
//! description, so what a pull request says about where the work came from is
//! not left to a model. That is a request the agent makes, not a wall, since a
//! credential that can comment can also open a pull request. Treat the token
//! as known to the agent, and scope it on that basis.

use std::collections::BTreeMap;

use crate::config::schema::GithubConfig;
use crate::sandbox::backend::{AGENT_BIN, STATE_PATH, WORKSPACE_PATH};

/// The environment variable `gh` and the credential helper both read.
pub const TOKEN_VARIABLE: &str = "GH_TOKEN";

/// Where the git configuration is written, relative to the agent's home.
pub const GITCONFIG_FILENAME: &str = ".gitconfig";

/// Where the agent asks for a pull request, inside its state directory.
///
/// A file rather than a credential. The state directory is the one place both
/// sides can reach, so it is how the agent says it is ready without being
/// given anything it could push with. The title is the first line. A later
/// line of `repository: <path>` says which clone it is for, by its path within
/// the workspace, which matters only when the session holds more than one.
pub const REQUEST_FILENAME: &str = "pull-request.txt";

/// Marks that somebody in the thread has asked for a pull request, and who.
///
/// On disk rather than in memory because a thread outlives the session running
/// it. Asking, having the attempt fail, and saying "try again" after a resume
/// is an ordinary sequence, and it must not be the resume that refuses.
///
/// Holds the asker's account id, and their display name after it when the chat
/// service gave one. A thread belongs to whoever opened it, but the person
/// asking for a pull request in it is often somebody else, and it is their
/// name the request should carry.
pub const ASKED_FILENAME: &str = "pull-request-asked";

/// Name of the wrapper, which has to be the name of what it stands in for.
pub const GH_SHIM_FILENAME: &str = "gh";

/// A `gh` that refuses to open a pull request, and is otherwise the real
/// thing.
///
/// For when the instruction not to has been ignored: this makes the habitual
/// command fail where the agent will read the reason. It is not a wall,
/// because the token can reach the API directly and the wrapper sits in a
/// directory the session can write. It costs one file, and it catches the
/// failure that actually happens.
///
/// The real program is found by walking PATH and skipping this directory, so
/// it does not have to be told where the host keeps it.
pub fn gh_shim_contents() -> String {
    [
        "#!/bin/sh",
        "# Generated per session by errand. Do not edit.",
        "if [ \"$1\" = \"pr\" ] && [ \"$2\" = \"create\" ]; then",
        "  echo \"errand: pull requests here are opened by the daemon, not by gh.\" >&2",
        &format!(
            "  echo \"Write the title to {STATE_PATH}/{REQUEST_FILENAME} and it opens when your turn ends.\" >&2"
        ),
        "  exit 1",
        "fi",
        // Split by shell rather than by `tr`, so the wrapper needs nothing on
        // PATH to find what is on PATH.
        "IFS=:",
        "for dir in $PATH; do",
        &format!(
            "  if [ \"$dir\" != \"{AGENT_BIN}\" ] && [ -x \"$dir/{GH_SHIM_FILENAME}\" ]; then"
        ),
        &format!("    exec \"$dir/{GH_SHIM_FILENAME}\" \"$@\""),
        "  fi",
        "done",
        "echo \"errand: gh is not installed\" >&2",
        "exit 127",
        "",
    ]
    .join("\n")
}

/// The git configuration a session runs with.
///
/// The credential helper is `gh` itself, so the token exists in exactly one
/// place: the environment. Writing it into a file here would leave a second
/// copy on disk in the session's state, for no gain.
pub fn git_config_contents(github: &GithubConfig) -> String {
    [
        "# Generated per session by errand. Do not edit.",
        "[user]",
        &format!("\tname = {}", github.user_name),
        &format!("\temail = {}", github.user_email),
        "",
        "[credential \"https://github.com\"]",
        "\thelper = !gh auth git-credential",
        "",
    ]
    .join("\n")
}

/// The identity git uses, in the environment as well as in the config file.
///
/// The file alone is not enough. An agent passing its own name at commit time
/// authors as itself, and these beat the file, so anything short of a
/// deliberate override is attributed to the bot.
pub fn git_identity_env(github: &GithubConfig) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("GIT_AUTHOR_NAME".to_owned(), github.user_name.clone()),
        ("GIT_AUTHOR_EMAIL".to_owned(), github.user_email.clone()),
        ("GIT_COMMITTER_NAME".to_owned(), github.user_name.clone()),
        ("GIT_COMMITTER_EMAIL".to_owned(), github.user_email.clone()),
    ])
}

/// Where a session can be read back, for a pull request to point at.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionLinks {
    /// The chat thread the request was made in, when there is one.
    pub thread: Option<String>,
    /// The session in the web interface, when the interface is published.
    pub transcript: Option<String>,
}

/// The footer saying where the work came from, which every pull request ends
/// with however it was opened.
///
/// No `@` anywhere in it, deliberately. The name here is a chat name, and the
/// GitHub account that happens to match it belongs to somebody who did not ask
/// for anything and should not be notified.
pub fn attribution_footer(requested_by: &str, links: &SessionLinks) -> String {
    let mut said = vec![format!("Requested by {requested_by} via errand.")];
    if let Some(thread) = &links.thread {
        said.push(format!("Conversation: {thread}"));
    }
    if let Some(transcript) = &links.transcript {
        said.push(format!("Transcript: {transcript}"));
    }
    said.join("\n")
}

/// A thread's own address, in the form Discord publishes it.
///
/// Stable and readable by anyone who can see the channel, which is what makes
/// it worth putting in a pull request at all.
pub fn thread_link(guild_id: &str, thread_id: &str) -> String {
    format!("https://discord.com/channels/{guild_id}/{thread_id}")
}

/// Where the interface shows one session, when it is reachable from outside.
pub fn transcript_link(public_url: &str, session_id: &str) -> String {
    let base = public_url.trim_end_matches('/');
    format!("{base}/?session={}", urlencoding_escape(session_id))
}

/// Percent-escapes what a query string cannot carry, as the chat service
/// would. Session ids are letters and digits, so this passes them through.
fn urlencoding_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// What the agent is told about getting work reviewed.
///
/// Three failures this is written against, all of them seen. Opening a pull
/// request nobody asked for, which spends somebody else's review time.
/// Announcing one before it exists, since the file is a request the daemon
/// acts on after the turn. And, when it opened one itself anyway, inventing a
/// footer that `@` mentioned the GitHub account matching a chat name.
///
/// The last is why the exact footer is here. The agent holds a token that can
/// open a pull request, so "do not" is the preference and not the guarantee,
/// and a bypass that still attributes the work correctly is much the smaller
/// failure.
pub fn review_instructions(
    github: &GithubConfig,
    requested_by: &str,
    links: &SessionLinks,
) -> String {
    [
        "",
        "## Opening a pull request",
        "",
        "`gh` is authenticated, so read issues, leave comments and check builds as",
        "you would anywhere. Two things are different.",
        "",
        &format!(
            "Commit as `{} <{}>`, which is already set up.",
            github.user_name, github.user_email
        ),
        "Do not pass `-c user.name`, `-c user.email` or `--author`, and do not set",
        "`GIT_AUTHOR_NAME` or `GIT_COMMITTER_NAME`. The commits belong to the bot",
        "account that opens the pull request, not to you.",
        "",
        "**Open nothing unless you were asked to.** Committing your work to a",
        "branch is the whole job unless somebody in the thread asks for a pull",
        "request. Never run `gh pr create`, and never ask for one unprompted.",
        "",
        "When you are asked: commit to a branch of its own, with a message saying",
        "what changed and why, then write the title you want on the first line of",
        &format!("`{STATE_PATH}/{REQUEST_FILENAME}`. If you have cloned more than one repository"),
        &format!("into {WORKSPACE_PATH}, add a line of `repository: <path within it>`, such as"),
        "`repository: github.com/owner/repo`, so the right one is opened. Write the file",
        "once, when the work is finished.",
        "",
        "**That file is a request, not the result.** The pull request is opened",
        "after your turn ends, and the thread is told whether it worked and where",
        "it went. You will not have seen that outcome, so do not say a pull request",
        "is open and do not quote an address for one.",
        "",
        "The daemon opens it so the description ends with the lines below, which",
        "are how a repository receiving work from a bot can tell where it came",
        "from. If you ever open one yourself despite the above, end the description",
        "with exactly these lines, after a `---` rule. Copy them literally. Never",
        "write an `@` mention: the name is a chat name, and the GitHub account that",
        "happens to match it belongs to somebody who asked for nothing.",
        "",
        "```",
        &attribution_footer(requested_by, links),
        "```",
        "",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests;
