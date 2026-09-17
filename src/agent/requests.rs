//! How the agent asks for a delegation, and how the answer gets back.
//!
//! The agent has no way to call the daemon: it is in a sandbox, and the one
//! place both sides can reach is the session's state directory. So a request
//! is a file there, and the answer is a file beside it. The same channel
//! already carries a pull request, and using it twice beats inventing a second
//! one.
//!
//! The agent runs a command rather than writing the file by hand. A command on
//! its PATH is something a coding agent already knows how to find and use, and
//! it means the request shape is written down in one place instead of being
//! described in a prompt and hoped for.

/// The directory requests and answers are exchanged in.
pub const DELEGATE_DIR: &str = "delegations";

/// Name of the command the agent runs.
pub const DELEGATE_COMMAND: &str = "delegate";

/// The command put on the agent's PATH.
///
/// Written in shell so it needs nothing installed beyond what a sandbox
/// already has. It writes the request, waits for the answer beside it, and
/// prints it. A refusal is printed and exits non-zero, so the agent reads it
/// as the tool not having worked rather than as an answer.
///
/// The answer is written to a temporary name and renamed, so this can never
/// read half of one.
pub fn delegate_command_contents(deadline_ms: u64) -> String {
    // A little longer than the daemon's own deadline, so the daemon is the one
    // that gives up and can say why, rather than this exiting first and
    // leaving an answer nobody reads.
    let wait_tenths = deadline_ms.div_ceil(1000) * 10 + 100;

    format!(
        r#"#!/bin/sh
# Generated per session by errand. Do not edit.
set -e

usage() {{
  echo "usage: delegate --file <path>|--call <id>|--attachment <name> <question>" >&2
  echo "Asks a cheaper model one question about one thing that already exists." >&2
  echo "It sees only what you name and can run nothing, so it answers in words alone." >&2
  exit 2
}}

kind=""
what=""
case "$1" in
  --file) kind="path" ;;
  --call) kind="callId" ;;
  --attachment) kind="attachment" ;;
  *) usage ;;
esac
what="$2"
shift 2
question="$*"
[ -n "${{what}}" ] || usage
[ -n "${{question}}" ] || usage

dir={STATE}/{DIR}
mkdir -p "${{dir}}"
id="$$-$(date +%s%N 2>/dev/null || date +%s)"

# JSON by hand, with the two characters that would break it escaped. A
# question is prose and a path is a path; neither needs more than this.
escape() {{
  printf '%s' "$1" | sed -e 's/\\\\/\\\\\\\\/g' -e 's/"/\\\\"/g' | tr -d '\n'
}}

printf '{{"question":"%%s","%%s":"%%s"}}' "$(escape "${{question}}")" "${{kind}}" "$(escape "${{what}}")" \
  > "${{dir}}/${{id}}.writing"
mv "${{dir}}/${{id}}.writing" "${{dir}}/${{id}}.request"

waited=0
while [ "${{waited}}" -lt {WAIT} ]; do
  if [ -f "${{dir}}/${{id}}.answer" ]; then
    cat "${{dir}}/${{id}}.answer"
    rm -f "${{dir}}/${{id}}.answer"
    exit 0
  fi
  if [ -f "${{dir}}/${{id}}.refused" ]; then
    cat "${{dir}}/${{id}}.refused" >&2
    rm -f "${{dir}}/${{id}}.refused"
    exit 1
  fi
  sleep 0.1
  waited=$((waited + 1))
done

rm -f "${{dir}}/${{id}}.request"
echo "the delegation was not answered in time; carry on yourself" >&2
exit 1
"#,
        STATE = crate::sandbox::backend::STATE_PATH,
        DIR = DELEGATE_DIR,
        WAIT = wait_tenths,
    )
}

/// What the agent is told about delegating.
///
/// Appended to its system prompt only when a model is configured to ask, so a
/// session that cannot delegate is never told about a command it does not
/// have.
pub fn delegate_instructions(model: &str, per_turn: usize) -> String {
    [
        String::new(),
        String::new(),
        "## Asking a cheaper model".to_owned(),
        format!("`{DELEGATE_COMMAND}` asks {model} one question about one thing that already"),
        "exists, and prints what it said. Use it to read something you would".to_owned(),
        "otherwise pull into this conversation whole: a long log, a large file, a".to_owned(),
        "diff you only need the shape of.".to_owned(),
        String::new(),
        "```".to_owned(),
        format!("{DELEGATE_COMMAND} --file src/parse.ts \"which functions does this export?\""),
        format!("{DELEGATE_COMMAND} --call <tool call id> \"what failed, and on which line?\""),
        format!("{DELEGATE_COMMAND} --attachment screenshot.png \"transcribe the error\""),
        "```".to_owned(),
        String::new(),
        "It is shown that one thing and nothing else: not this conversation, not".to_owned(),
        "what you are trying to do. It is asked for text and given no way to call".to_owned(),
        "anything, so it cannot read another file, run a command, or change".to_owned(),
        "anything. Ask it about the thing in front of it. What it says is a".to_owned(),
        "description to check, not an observation you made.".to_owned(),
        String::new(),
        format!("You may ask {per_turn} times per turn. When it refuses, or you need to be"),
        "certain, read the thing yourself.".to_owned(),
    ]
    .join("\n")
}

#[cfg(test)]
mod tests;
