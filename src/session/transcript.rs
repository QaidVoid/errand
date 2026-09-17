//! A session's output, written down so it outlives the sandbox that produced
//! it.
//!
//! A session reports what it is doing to its views, which are in memory: close
//! the sandbox and the record of what happened goes with it. That is fine for
//! a chat thread, which keeps its own copy, and wrong for anything reading a
//! session back, which would show an empty pane for every session that has
//! stopped.
//!
//! One file per session, beside its state, so removing a session's state
//! removes its transcript with it. Appending is best effort: a session must
//! never fail because its history could not be written.

use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::log::{Logger, now_ms};
use crate::session::event::SessionEvent;
use crate::session::views::Recorder;

/// One recorded thing, with when it happened and the turn it belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Journaled {
    /// When it was written, in milliseconds.
    pub at: i64,
    /// Absent in a transcript written before turns were recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
    /// The event itself, exactly as the session reported it.
    pub entry: SessionEvent,
}

/// The filename inside a session's state directory.
pub const TRANSCRIPT_FILENAME: &str = "transcript.jsonl";

/// How much is read back.
///
/// A session that ran for hours can have written far more than is useful to
/// reopen, and the most recent is the part worth showing.
pub const MAX_REPLAYED: usize = 2_000;

/// How much of a transcript is read when looking for what was first asked.
///
/// The opening prompt arrives within the first few lines, after a notice or
/// two. Reading the whole file for it costs the length of the session, and it
/// is read once per stopped session every time they are listed.
pub const OPENING_SCAN_BYTES: usize = 64 * 1024;

/// What was read back, and what was left behind.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredTranscript {
    /// What was read back, oldest first.
    pub entries: Vec<Journaled>,
    /// Entries older than those returned, so a replay can admit to the gap.
    pub dropped: usize,
}

/// Reads whole entries out of text, skipping anything torn or unrecognised.
fn journaled(text: &str) -> Vec<Journaled> {
    text.split('\n')
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// An append-only transcript for one session.
pub struct Transcript {
    path: PathBuf,
    log: Option<Logger>,
}

impl Transcript {
    /// Opens a transcript at a path, optionally telling a log about failures.
    pub fn new(path: impl Into<PathBuf>, log: Option<Logger>) -> Self {
        Self {
            path: path.into(),
            log,
        }
    }

    /// Appends one entry at a given moment. Never fails the caller: history is
    /// not worth failing a turn for.
    pub fn append_at(&self, entry: &SessionEvent, turn: Option<u32>, at: i64) {
        let line = serde_json::to_string(&Journaled {
            at,
            turn,
            entry: entry.clone(),
        })
        .expect("a transcript entry serializes");

        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
            .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()));
        if let Err(error) = written
            && let Some(log) = &self.log
        {
            log.warn(
                "a transcript entry could not be written",
                &crate::log::fields([("detail", error.to_string().into())]),
            );
        }
    }

    /// The first thing this session was asked to do.
    ///
    /// Read from here rather than from the thread index, which deliberately
    /// holds no message content and exists only to find a session again.
    ///
    /// Only the head of the file is scanned. A session whose first prompt is
    /// somehow beyond that is left unnamed, which costs a title, where reading
    /// every transcript in full costs the length of every session.
    ///
    /// Returns None when the session was never asked anything, as when it
    /// failed before its first prompt.
    pub fn opening(&self) -> Option<String> {
        let head = self.read_head(OPENING_SCAN_BYTES).ok()?;
        journaled(&head)
            .into_iter()
            .find_map(|held| match held.entry {
                SessionEvent::Prompt { text, .. } => Some(text.trim().to_owned()),
                _ => None,
            })
    }

    /// Reads the most recent entries. A missing or unreadable file reads
    /// empty.
    pub fn read(&self) -> StoredTranscript {
        self.read_up_to(MAX_REPLAYED)
    }

    /// Reads the most recent entries back to a limit of one's own choosing.
    pub fn read_up_to(&self, limit: usize) -> StoredTranscript {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return StoredTranscript {
                entries: Vec::new(),
                dropped: 0,
            };
        };

        let mut parsed = journaled(&text);
        if parsed.len() <= limit {
            return StoredTranscript {
                entries: parsed,
                dropped: 0,
            };
        }
        let dropped = parsed.len() - limit;
        StoredTranscript {
            entries: parsed.split_off(dropped),
            dropped,
        }
    }

    /// The first bytes of the file, cut back to the last whole line.
    ///
    /// Cutting back matters: a read that lands mid-line would otherwise hand a
    /// truncated JSON object to the parser, which would drop the entry it was
    /// halfway through even though the file holds it in full.
    fn read_head(&self, bytes: usize) -> std::io::Result<String> {
        let mut file = std::fs::File::open(&self.path)?;
        let mut buffer = vec![0; bytes];
        let read = file.read(&mut buffer)?;
        buffer.truncate(read);
        let text = String::from_utf8_lossy(&buffer);
        let cut = text.rfind('\n').unwrap_or(text.len());
        Ok(text[..cut].to_owned())
    }
}

impl Recorder for Transcript {
    fn append(&self, entry: &SessionEvent, turn: u32) {
        self.append_at(entry, Some(turn), now_ms());
    }
}

#[cfg(test)]
#[path = "transcript/tests.rs"]
mod tests;
