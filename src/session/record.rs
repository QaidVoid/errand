//! Where the daemon keeps what a session must not be able to rewrite.
//!
//! A session's state directory is handed to it as `/state`, writable, because
//! the agent genuinely needs somewhere to keep its home, its notes, and the
//! file it asks for a pull request with. Two things were living there that are
//! not the agent's to change: the transcript, which is the record of what
//! happened, and the mark saying a person asked for a pull request, which is
//! the thing that makes the request attributable at all.
//!
//! Both live in a directory beside the state one that is never granted.
//! Beside rather than beneath, because the grant covers the state directory
//! whole and anything under it comes with it.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::session::transcript::TRANSCRIPT_FILENAME;

/// The directory holding a session's record, given its state directory.
pub fn record_dir(state_dir: &str) -> String {
    format!("{state_dir}.record")
}

/// The transcript to read, preferring the record directory.
///
/// A session written before the record directory existed kept its transcript
/// in the state directory, and that history is still worth showing, so the
/// older place is read when the newer one holds nothing. Writing always goes
/// to the newer one, which [`prepare_record_dir`] has moved anything older
/// into.
pub fn transcript_path(state_dir: &str) -> PathBuf {
    let placed = Path::new(&record_dir(state_dir)).join(TRANSCRIPT_FILENAME);
    if placed.is_file() {
        return placed;
    }
    let legacy = Path::new(state_dir).join(TRANSCRIPT_FILENAME);
    if legacy.is_file() { legacy } else { placed }
}

/// Makes the record directory, moving a transcript left in the older place.
///
/// The move is what takes an existing session's history out of the agent's
/// reach; without it a resumed thread would keep appending where the agent can
/// still rewrite. A failure to move is not a failure to start: the session
/// matters more than where its record sits, and the next attempt tries again.
pub fn prepare_record_dir(state_dir: &str) -> String {
    let directory = record_dir(state_dir);
    let _ = std::fs::create_dir_all(&directory);

    let placed = Path::new(&directory).join(TRANSCRIPT_FILENAME);
    let legacy = Path::new(state_dir).join(TRANSCRIPT_FILENAME);
    if !placed.is_file() && legacy.is_file() {
        // Left where it is on a failure; it is still read, and still shown.
        let _ = std::fs::rename(&legacy, &placed);
    }
    directory
}

/// Takes a withdrawn message out of the record, in place.
///
/// The entry stays and loses its text. A transcript that quietly drops a turn
/// reads as one that never had it, and the replies around it stop making
/// sense; saying that something was withdrawn keeps the conversation
/// followable and is also the honest thing to show.
///
/// Written to a new file and renamed over the old one, so a reader sees the
/// whole of one version or the whole of the other. A line that cannot be
/// parsed is carried across untouched rather than dropped: this is a
/// redaction, not a repair.
///
/// Returns the text that was withdrawn, or None when nothing matched. The
/// caller needs it: the agent keeps no chat id, so what was said is the only
/// thing the two records have in common. A failure to write the corrected
/// record is an error, not a matchless day.
pub fn withdraw_from_record(state_dir: &str, message_id: &str) -> io::Result<Option<String>> {
    let path = transcript_path(state_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };

    let mut said: Option<String> = None;
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                return line.to_owned();
            }
            let Ok(mut parsed) = serde_json::from_str::<Value>(line) else {
                return line.to_owned();
            };
            let checked = parsed.get("entry");
            let Some(entry) = checked else {
                return line.to_owned();
            };
            if entry.get("id").and_then(Value::as_str) != Some(message_id) {
                return line.to_owned();
            }
            let call = entry.get("call").and_then(Value::as_str);
            if call != Some("prompt") && call != Some("aside") {
                return line.to_owned();
            }
            if let Some(text) = entry.get("text").and_then(Value::as_str)
                && !text.is_empty()
            {
                said = Some(text.to_owned());
            }
            let entry = parsed
                .get_mut("entry")
                .expect("the entry was there a moment ago");
            entry["text"] = json!("");
            entry["withdrawn"] = json!(true);
            serde_json::to_string(&parsed).unwrap_or_else(|_| line.to_owned())
        })
        .collect();
    let Some(said) = said else {
        return Ok(None);
    };

    write_over(&path, &lines.join("\n"))?;
    Ok(Some(said))
}

/// What stands in for a withdrawn message, so the conversation still follows.
const WITHDRAWN_TEXT: &str = "[a message here was withdrawn by the person who sent it]";

/// Takes a withdrawn message out of the agent's own stored conversation.
///
/// This is the copy that decides what a resumed session sends to a model, and
/// it belongs to the agent rather than to the daemon. Two things make writing
/// it safe enough to do: the agent appends and closes rather than holding the
/// file open, and the caller only reaches here between turns.
///
/// The record keeps its identity and loses its words. The file is a chain by
/// `parentId`, so removing an entry would strand everything after it; what is
/// left is a message that says it was withdrawn, which reads correctly and
/// sends nothing.
///
/// A record that cannot be parsed is carried across untouched. Repairing the
/// agent's file is not this function's business, and a withdrawal is no reason
/// to start.
///
/// Returns whether anything was withdrawn.
pub fn withdraw_from_agent_session(state_dir: &str, said: &str) -> io::Result<bool> {
    let Ok(entries) = std::fs::read_dir(Path::new(state_dir).join("sessions")) else {
        return Ok(false);
    };

    let mut withdrew = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|name| name != "jsonl") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };

        let mut found = false;
        let lines: Vec<String> = text
            .split('\n')
            .map(|line| {
                if line.trim().is_empty() {
                    return line.to_owned();
                }
                let Ok(mut parsed) = serde_json::from_str::<Value>(line) else {
                    return line.to_owned();
                };
                if !carries_withdrawn(&parsed, said) {
                    return line.to_owned();
                }
                found = true;
                if let Some(message) = parsed.get_mut("message") {
                    message["content"] = json!([{ "type": "text", "text": WITHDRAWN_TEXT }]);
                }
                serde_json::to_string(&parsed).unwrap_or_else(|_| line.to_owned())
            })
            .collect();
        if !found {
            continue;
        }

        write_over(&path, &lines.join("\n"))?;
        withdrew = true;
    }
    Ok(withdrew)
}

/// Writes a corrected file and renames it over the one it corrects, so a
/// reader sees the whole of one version or the whole of the other.
fn write_over(path: &Path, text: &str) -> io::Result<()> {
    let staging = PathBuf::from(format!("{}.withdrawing", path.display()));
    std::fs::write(&staging, text)?;
    std::fs::rename(&staging, path)
}

/// Whether a stored record is the user message that carried what was
/// withdrawn.
///
/// Matched on the words, because there is nothing else to match on: the chat's
/// own id never reaches the agent. errand wraps a prompt with context before
/// sending it, so the stored text contains what was said rather than equalling
/// it.
fn carries_withdrawn(parsed: &Value, said: &str) -> bool {
    let Some(message) = parsed.get("message") else {
        return false;
    };
    if message.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    let Some(content) = message.get("content").and_then(Value::as_array) else {
        return false;
    };
    content.iter().any(|block| {
        block
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains(said))
    })
}

#[cfg(test)]
#[path = "record/tests.rs"]
mod tests;
