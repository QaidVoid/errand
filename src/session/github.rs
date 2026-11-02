//! The forge identity a session pushes with.
//!
//! The agent gets a credential, because reading issues, leaving comments and
//! checking a build are most of working on somebody's repository and none of
//! it is possible without one. That is the daemon's own token, or, with a
//! GitHub App configured, a narrower token minted for the session alone.
//!
//! Pull requests are the exception it is asked to route through the daemon: it
//! writes a title into its state directory and the daemon composes the
//! description, so what a pull request says about where the work came from is
//! not left to a model. That is a request the agent makes, not a wall, since a
//! credential that can comment can also open a pull request. Treat the
//! credential as known to the agent, and scope it on that basis.

use std::collections::BTreeMap;

use crate::config::schema::{ForgeConfig, ForgeKind};
use crate::forge;
use crate::sandbox::backend::{AGENT_BIN, STATE_PATH, WORKSPACE_PATH};

/// The environment variable `gh` and the credential helper both read.
pub const TOKEN_VARIABLE: &str = "GH_TOKEN";

/// Where the git configuration is written, relative to the agent's home.
pub const GITCONFIG_FILENAME: &str = ".gitconfig";

/// Where the credential helper for a forge without a `gh` to route through
/// is written, relative to the agent's home.
pub const FORGE_CREDENTIAL_FILENAME: &str = "forge-credential";

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

/// A `gh` that refuses what a session is not to do with it, and is otherwise
/// the real thing.
///
/// It refuses opening a pull request, which the daemon does, the verbs that
/// destroy or hand over a repository or its secrets, defining an alias, which
/// would rename any of them, and printing the credential, each with what to
/// do instead. `gh api` is refused a `DELETE`, `PATCH` or `PUT`, the transfer
/// and fork routes, and any host but GitHub's own. GraphQL mutations are not
/// read.
///
/// For when an instruction has been ignored: this makes the habitual command
/// fail where the agent will read the reason. It is not a wall, because the
/// credential can reach the API directly and the wrapper sits in a directory
/// the session can write. The wall, where there is one, is the credential:
/// an App token is narrowed to the installation and the permissions asked
/// for, and GitHub refuses the rest.
///
/// With `reads_token`, the session holds a token the daemon renews, kept in
/// its state directory, and the wrapper reads it before every call, so `gh`
/// and git through it always present the current one.
///
/// The real program is found by walking PATH and skipping this directory, so
/// it does not have to be told where the host keeps it.
pub fn gh_shim_contents(reads_token: bool) -> String {
    let token = if reads_token { GH_SHIM_TOKEN } else { "" };
    GH_SHIM
        .replace("@TOKEN@", token)
        .replace("@STATE@", STATE_PATH)
        .replace("@REQUEST@", REQUEST_FILENAME)
        .replace("@TOKEN_FILE@", app::TOKEN_FILENAME)
        .replace("@AGENT_BIN@", AGENT_BIN)
        .replace("@GH@", GH_SHIM_FILENAME)
}

/// The wrapper's lines that read the current token, when the session holds
/// one the daemon renews. Unset first, so a missing file fails closed.
const GH_SHIM_TOKEN: &str = r#"unset GH_TOKEN
if [ -r "@STATE@/@TOKEN_FILE@" ]; then
  IFS= read -r GH_TOKEN < "@STATE@/@TOKEN_FILE@"
  export GH_TOKEN
fi
"#;

/// The `gh` a Forgejo session finds, which refuses.
///
/// The agent reaches for `gh` the way it does anywhere, and without a stub
/// the reflex dies on a missing command or, worse, runs a real `gh` at
/// GitHub, where the session holds neither business nor credential. The
/// refusal says where git does reach and who opens the pull request, so the
/// first miss teaches the way this forge works.
pub fn forge_gh_shim_contents() -> String {
    r#"#!/bin/sh
# Generated per session by errand. Do not edit.
echo "errand: gh speaks to GitHub, and this session works with Forgejo." >&2
echo "Git alone reaches the forge: clone, branch, commit and push with the credential errand set up." >&2
echo "Pull requests, and forks for them, are opened by the daemon: write the title to @STATE@/@REQUEST@ and it opens when your turn ends." >&2
exit 1
"#
    .replace("@STATE@", STATE_PATH)
    .replace("@REQUEST@", REQUEST_FILENAME)
}

/// The wrapper, with its paths left as `@NAME@` for [`gh_shim_contents`].
///
/// A refused verb is refused wherever it follows its group, whatever stands
/// between, so a flag and its value, before the verb or between the two, can
/// hide neither. That refuses the rare call where the words fall that way by
/// chance, such as a value of `delete` after `repo`, which is the side to
/// fail on. A call naming `api`
/// anywhere has every spelling of its method checked, and the routes that
/// hand a repository over refused by path. PATH is split by the shell rather
/// than by `tr`, so the wrapper needs nothing on PATH to find what is on
/// PATH.
const GH_SHIM: &str = r#"#!/bin/sh
# Generated per session by errand. Do not edit.
@TOKEN@refuse() {
  echo "errand: $1 is not run from a session." >&2
  echo "$2" >&2
  exit 1
}
daemon="Pull requests, and forks for them, are opened by the daemon: write the title to @STATE@/@REQUEST@ and it opens when your turn ends."
ask="Ask somebody in the thread to do it, if it needs doing."
if [ -n "${GH_HOST:-}" ] && [ "$GH_HOST" != "github.com" ]; then
  refuse "gh for $GH_HOST" "Only github.com is reached from a session."
fi
seen=" "; api=""; method=""; host=""; next=""
for arg in "$@"; do
  case "$next" in method) method="$arg" ;; host) host="$arg" ;; esac
  next=""
  case "$arg" in
    -X|--method) next=method; continue ;;
    --hostname) next=host; continue ;;
    -X=*) method="${arg#-X=}"; continue ;;
    -X*) method="${arg#-X}"; continue ;;
    --method=*) method="${arg#--method=}"; continue ;;
    --hostname=*) host="${arg#--hostname=}"; continue ;;
    -[!-]*X*) method="${arg#*X}"; [ -z "$method" ] && next=method; continue ;;
    -t*|-[!-]*t*|--show-token|--show-token=*)
      case "$seen" in *" auth "*) refuse "gh auth status $arg" "The credential is gh's to use, not to print." ;; esac
      continue ;;
    -*) continue ;;
    api) api=1 ;;
  esac
  if [ -n "$api" ]; then
    route="${arg%%\?*}"; route="${route%/}"
    case "$route" in */transfer|*/forks) refuse "gh api $arg" "$daemon" ;; esac
  fi
  for group in $seen; do
    case "$group $arg" in
      "pr create"|"pr new"|"repo fork") refuse "gh $group $arg" "$daemon" ;;
      "repo delete"|"repo transfer"|"repo archive"|"release delete"|"workflow disable"|"secret set"|"secret delete"|"secret remove"|"variable set"|"variable delete"|"variable remove"|"alias set"|"alias import")
        refuse "gh $group $arg" "$ask" ;;
      "auth token") refuse "gh auth token" "The credential is gh's to use, not to print." ;;
    esac
  done
  case "$arg" in
    pr|repo|release|workflow|secret|variable|alias|auth) seen="$seen$arg " ;;
  esac
done
if [ -n "$api" ]; then
  case "$method" in
    [Dd][Ee][Ll][Ee][Tt][Ee]|[Pp][Aa][Tt][Cc][Hh]|[Pp][Uu][Tt]) refuse "gh api -X $method" "$ask" ;;
  esac
  if [ -n "$host" ] && [ "$host" != "github.com" ]; then
    refuse "gh api for $host" "Only github.com is reached from a session."
  fi
fi
IFS=:
for dir in $PATH; do
  if [ "$dir" != "@AGENT_BIN@" ] && [ -x "$dir/@GH@" ]; then
    exec "$dir/@GH@" "$@"
  fi
done
echo "errand: gh is not installed" >&2
exit 127
"#;

/// The git configuration a session runs with.
///
/// The credential helper answers with whatever credential the session holds:
/// the daemon's token in the environment, or, on GitHub with an App, the App
/// token the wrapper reads from the state directory. No copy is written here.
/// GitHub routes through `gh` itself; a Forgejo forge has no `gh`, so a
/// helper of the daemon's own answers in its place.
pub fn git_config_contents(forge: &ForgeConfig) -> String {
    let helper = match forge.kind {
        ForgeKind::Github => "!gh auth git-credential".to_owned(),
        ForgeKind::Forgejo => format!("!{AGENT_BIN}/{FORGE_CREDENTIAL_FILENAME}"),
    };
    [
        "# Generated per session by errand. Do not edit.",
        "[user]",
        &format!("\tname = {}", forge.user_name),
        &format!("\temail = {}", forge.user_email),
        "",
        &format!("[credential \"{}\"]", forge::site_base(forge)),
        &format!("\thelper = {helper}"),
        "",
    ]
    .join("\n")
}

/// The credential helper a Forgejo session runs git through, where there is
/// no `gh` to route the question through itself.
///
/// Git asks with `get` and reads a username and a password; the forge
/// authenticates the token in the password and pays no attention to the
/// name beside it, which only has to be there. The header says as much for
/// the agent that finds the name in its git configuration and comes
/// looking, as one did.
pub fn forge_credential_contents() -> String {
    [
        "#!/bin/sh",
        "# Generated per session by errand. Do not edit.",
        "# Git runs this itself when it needs the forge's credential. It is",
        "# not a tool for you: called any other way, it answers nothing.",
        "if [ \"$1\" = get ]; then",
        "  echo \"username=errand\"",
        &format!(r#"  echo "password=${TOKEN_VARIABLE}""#),
        "fi",
        "",
    ]
    .join("\n")
}

/// The identity git uses, in the environment as well as in the config file.
///
/// The file alone is not enough. An agent passing its own name at commit time
/// authors as itself, and these beat the file, so anything short of a
/// deliberate override is attributed to the bot.
pub fn git_identity_env(forge: &ForgeConfig) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("GIT_AUTHOR_NAME".to_owned(), forge.user_name.clone()),
        ("GIT_AUTHOR_EMAIL".to_owned(), forge.user_email.clone()),
        ("GIT_COMMITTER_NAME".to_owned(), forge.user_name.clone()),
        ("GIT_COMMITTER_EMAIL".to_owned(), forge.user_email.clone()),
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
/// footer that `@` mentioned the forge account matching a chat name.
///
/// The last is why the exact footer is here. The agent holds a token that can
/// open a pull request, so "do not" is the preference and not the guarantee,
/// and a bypass that still attributes the work correctly is much the smaller
/// failure.
pub fn review_instructions(
    forge: &ForgeConfig,
    requested_by: &str,
    links: &SessionLinks,
) -> String {
    let gh_line = match forge.kind {
        ForgeKind::Github => {
            if forge.app.is_some() {
                "`gh` is authenticated for the repositories listed for this session's GitHub App, \
                 or every one it was installed on when none are listed, so read issues, leave \
                 comments and check builds there as you would anywhere. GitHub refuses anything \
                 outside them, and a fork or a pull request on another repository is opened by \
                 the daemon, as below. Two things are different."
            } else {
                "`gh` is authenticated, so read issues, leave comments and check builds as you \
                 would anywhere. Two things are different."
            }
        }
        ForgeKind::Forgejo => {
            "Git reaches the forge on its own: cloning, pushing and pulling authenticate \
             through `forge-credential`, a helper script errand installs that git runs itself \
             when it needs the credential, so push branches as you would anywhere and leave \
             the helper to git. There is no `gh`: the one on your path refuses and says so, \
             since reading issues, leaving comments and pull requests are the daemon's work, \
             as below. Two things are different."
        }
    };
    [
        "",
        "## Opening a pull request",
        "",
        gh_line,
        "",
        &format!(
            "Commit as `{} <{}>`, which is already set up.",
            forge.user_name, forge.user_email
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
        &format!(
            "into {WORKSPACE_PATH}, add a line of `repository: <directory>` naming the clone's"
        ),
        &format!(
            "directory under {WORKSPACE_PATH}, not its name on the forge: a clone at \
             `{WORKSPACE_PATH}/tools/parser`"
        ),
        "is `repository: tools/parser`. Nothing else in the file is read: the branch is",
        "the one checked out, and the description is the body of its last commit",
        "message. Write the file once, when the work is finished.",
        "",
        "**That file is a request, not the result.** The pull request is opened",
        "after your turn ends, and the thread is told whether it worked and where",
        "it went. You are told too, at the start of your next turn. Until then,",
        "do not say a pull request is open and do not quote an address for one.",
        "",
        "The daemon opens it so the description ends with the lines below, which",
        "are how a repository receiving work from a bot can tell where it came",
        "from. If you ever open one yourself despite the above, end the description",
        "with exactly these lines, after a `---` rule. Copy them literally. Never",
        "write an `@` mention: the name is a chat name, and the forge account that",
        "happens to match it belongs to somebody who asked for nothing.",
        "",
        "```",
        &attribution_footer(requested_by, links),
        "```",
        "",
    ]
    .join("\n")
}

/// A session's credential minted from a GitHub App.
pub mod app;

#[cfg(test)]
mod tests;
