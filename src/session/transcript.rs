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

use crate::log::fields;
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

/// The most of a transcript, or of the agent's own session file, read back.
///
/// Both grow with a session's age, and a resume or a withdrawal reads one
/// whole, so a bound keeps a very long-lived session from being read into
/// memory entire.
pub const MAX_TRANSCRIPT_BYTES: u64 = 64 * 1024 * 1024;

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

        // A link at the name is refused rather than written through: the
        // daemon writes this file, and what it would reach is not its to say.
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
            .open(&self.path)
            .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()));
        if let Err(error) = written
            && let Some(log) = &self.log
        {
            log.warn(
                "a transcript entry could not be written",
                &fields([("detail", error.to_string().into())]),
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

    /// The newest [`MAX_TRANSCRIPT_BYTES`] of the file as text, and how many
    /// lines before them were skipped, or nothing when it is missing or
    /// unreadable.
    ///
    /// The newest part rather than none: a long session's history is still
    /// its history, and only the newest entries are replayed anyway. The
    /// skipped lines are counted as they pass, not held, and the skip is
    /// said in the log.
    fn read_capped(&self) -> Option<(String, usize)> {
        let file = std::fs::File::open(&self.path).ok()?;
        let skip = file
            .metadata()
            .ok()?
            .len()
            .saturating_sub(MAX_TRANSCRIPT_BYTES);
        let mut skipped = 0;
        let mut ended_a_line = true;
        if skip > 0 {
            let mut head = (&file).take(skip);
            let mut chunk = vec![0; 64 * 1024];
            loop {
                let read = head.read(&mut chunk).ok()?;
                if read == 0 {
                    break;
                }
                #[expect(
                    clippy::naive_bytecount,
                    reason = "only a transcript past the cap is counted, which is rare \
                              enough not to warrant a crate"
                )]
                let lines = chunk[..read].iter().filter(|byte| **byte == b'\n').count();
                skipped += lines;
                ended_a_line = chunk[read - 1] == b'\n';
            }
        }
        let mut bytes = Vec::new();
        (&file)
            .take(MAX_TRANSCRIPT_BYTES)
            .read_to_end(&mut bytes)
            .ok()?;
        if skip > 0 {
            // The newest part starts inside a line unless the skip ended one.
            if !ended_a_line {
                let cut = bytes.iter().position(|byte| *byte == b'\n');
                bytes.drain(..cut.map_or(bytes.len(), |cut| cut + 1));
                skipped += 1;
            }
            if let Some(log) = &self.log {
                log.warn(
                    "a transcript is past the read cap, so only its newest part is read",
                    &fields([
                        ("path", self.path.display().to_string().into()),
                        ("limit_bytes", MAX_TRANSCRIPT_BYTES.into()),
                        ("skipped_lines", skipped.into()),
                    ]),
                );
            }
        }
        match String::from_utf8(bytes) {
            Ok(text) => Some((text, skipped)),
            // One torn byte must not cost the whole history: the lines around
            // it are whole, and the damaged one shows what can be read of it.
            Err(torn) => {
                if let Some(log) = &self.log {
                    log.warn(
                        "a transcript holds bytes that are not UTF-8, which are read as \
                         replacement characters",
                        &fields([("path", self.path.display().to_string().into())]),
                    );
                }
                Some((
                    String::from_utf8_lossy(torn.as_bytes()).into_owned(),
                    skipped,
                ))
            }
        }
    }

    /// Reads the most recent entries. A missing or unreadable file reads
    /// empty.
    pub fn read(&self) -> StoredTranscript {
        self.read_up_to(MAX_REPLAYED)
    }

    /// Reads the most recent entries back to a limit of one's own choosing.
    pub fn read_up_to(&self, limit: usize) -> StoredTranscript {
        let Some((text, skipped)) = self.read_capped() else {
            return StoredTranscript {
                entries: Vec::new(),
                dropped: 0,
            };
        };

        let mut parsed = journaled(&text);
        if parsed.len() <= limit {
            return StoredTranscript {
                entries: parsed,
                dropped: skipped,
            };
        }
        let dropped = parsed.len() - limit;
        StoredTranscript {
            entries: parsed.split_off(dropped),
            dropped: dropped + skipped,
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
mod tests;
