//! Remembers which thread belonged to which session, across daemon restarts.
//!
//! The agent keeps its own conversation history in the session state
//! directory, so resuming a thread needs only enough to find that directory
//! again. That is what is stored, and nothing else: no message content and no
//! credential.
//!
//! This is the daemon's only durable state, so it is written carefully. Losing
//! it does not stop the daemon starting, but it does silently withdraw every
//! thread anybody had open, which is the kind of failure that gets blamed on
//! the chat service.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::log::{LogValue, Logger};

/// Filename of the index inside the daemon's state directory.
pub const REGISTRY_FILENAME: &str = "threads.json";

/// How many threads are remembered before the oldest are dropped.
pub const MAX_REMEMBERED: usize = 500;

/// What is needed to put a thread back to work after a restart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRecord {
    /// The chat thread the session runs in.
    pub thread_id: String,
    /// The session the thread belongs to.
    pub session_id: String,
    /// Where the agent's own session history lives.
    pub state_dir: String,
    /// The project the session works on.
    pub project_name: String,
    /// Where that project lives on the host.
    pub project_path: String,
    /// Who started the session, so ownership survives a restart.
    pub owner_id: String,
    /// Accounts the owner invited to take part.
    ///
    /// Kept so a restart does not silently withdraw access somebody was given,
    /// which would look like the bot ignoring them.
    pub guests: Vec<String>,
    /// The provider the session was started on, when it was chosen rather than
    /// taken from the configuration.
    ///
    /// Kept so a restart puts the thread back on the model it was working
    /// with. Coming back on a different one is a change nobody asked for, and
    /// a quiet one: the answers simply start reading differently.
    pub provider: Option<String>,
    /// The model the session was started on, when it was chosen.
    pub model: Option<String>,
    /// Milliseconds since the epoch, for evicting the least recently used.
    pub updated_at: i64,
}

/// Whether a parsed entry is a record this daemon can resume.
pub fn is_record(value: &Value) -> bool {
    let Some(record) = value.as_object() else {
        return false;
    };
    let is_string = |key: &str| record.get(key).is_some_and(Value::is_string);
    let is_optional_string =
        |key: &str| matches!(record.get(key), None | Some(Value::Null | Value::String(_)));
    is_string("threadId")
        && is_string("sessionId")
        && is_string("stateDir")
        && is_string("projectName")
        && is_string("projectPath")
        && is_string("ownerId")
        && record.get("guests").is_some_and(|guests| {
            guests.is_array()
                && guests
                    .as_array()
                    .expect("checked above")
                    .iter()
                    .all(Value::is_string)
        })
        && is_optional_string("provider")
        && is_optional_string("model")
        && record.get("updatedAt").is_some_and(Value::is_number)
}

/// A durable thread-to-session index.
pub struct ThreadRegistry {
    path: String,
    log: Logger,
    records: BTreeMap<String, ThreadRecord>,
}

impl ThreadRegistry {
    /// Opens the index kept at `path`, which [`path_for`](Self::path_for)
    /// builds from a state directory.
    pub fn new(path: String, log: Logger) -> Self {
        Self {
            path,
            log,
            records: BTreeMap::new(),
        }
    }

    /// The index path inside a daemon state directory.
    pub fn path_for(state_dir: &str) -> String {
        Path::new(state_dir)
            .join(REGISTRY_FILENAME)
            .to_string_lossy()
            .into_owned()
    }

    /// Reads the index.
    ///
    /// A missing file is an empty index, which is the state on a first run. A
    /// file that will not parse is kept aside rather than overwritten, because
    /// it is the only copy of what was there and something has to be able to
    /// look at it afterwards.
    pub fn load(&mut self) {
        self.records.clear();

        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    self.log.error(
                        "the thread index could not be read, so no thread can be resumed",
                        &crate::log::fields([
                            ("path", self.path.as_str().into()),
                            ("detail", error.to_string().into()),
                        ]),
                    );
                }
                return;
            }
        };

        let parsed: Result<Value, _> = serde_json::from_str(&text);
        let Ok(parsed) = parsed else {
            self.set_aside("the index is not JSON");
            return;
        };
        let Some(entries) = parsed.as_array() else {
            self.set_aside("the index is not a list of threads");
            return;
        };

        let mut skipped = 0;
        for entry in entries {
            if is_record(entry) {
                let record: ThreadRecord =
                    serde_json::from_value(entry.clone()).expect("checked by is_record");
                self.records.insert(record.thread_id.clone(), record);
            } else {
                skipped += 1;
            }
        }
        if skipped > 0 {
            let skipped = i64::from(skipped);
            self.log.warn(
                "entries in the thread index were not readable and were dropped",
                &crate::log::fields([("skipped", LogValue::from(skipped))]),
            );
        }
    }

    /// The record for a thread, if one was ever kept.
    pub fn get(&self, thread_id: &str) -> Option<&ThreadRecord> {
        self.records.get(thread_id)
    }

    /// Number of threads currently remembered.
    pub fn size(&self) -> usize {
        self.records.len()
    }

    /// Every remembered thread, most recently used first.
    pub fn all(&self) -> Vec<ThreadRecord> {
        let mut all: Vec<ThreadRecord> = self.records.values().cloned().collect();
        all.sort_by_key(|record| std::cmp::Reverse(record.updated_at));
        all
    }

    /// Records a thread, evicting the least recently used past the bound.
    pub fn remember(&mut self, record: ThreadRecord) {
        self.records.insert(record.thread_id.clone(), record);

        if self.records.len() > MAX_REMEMBERED {
            let mut ordered: Vec<(String, i64)> = self
                .records
                .iter()
                .map(|(id, record)| (id.clone(), record.updated_at))
                .collect();
            ordered.sort_by_key(|(_, updated_at)| *updated_at);
            for (thread_id, _) in ordered
                .into_iter()
                .take(self.records.len() - MAX_REMEMBERED)
            {
                self.records.remove(&thread_id);
            }
        }

        self.save();
    }

    /// Forgets a thread, so it is never resumed again.
    pub fn forget(&mut self, thread_id: &str) {
        if self.records.remove(thread_id).is_some() {
            self.save();
        }
    }

    /// Writes the index so that a crash leaves either the old one or the new
    /// one.
    ///
    /// Written beside the target, flushed, and renamed. The rename is what
    /// makes it atomic; the flush before it is what stops a crash leaving a
    /// file that was renamed into place before its contents reached the disk.
    /// The directory is flushed afterwards so the rename itself survives.
    fn save(&mut self) {
        let temporary = format!("{}.tmp", self.path);
        let records: Vec<&ThreadRecord> = self.records.values().collect();
        let body = format!(
            "{}\n",
            serde_json::to_string_pretty(&records).unwrap_or_default()
        );

        let parent = Path::new(&self.path)
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
        let written = std::fs::create_dir_all(&parent)
            .and_then(|()| std::fs::write(&temporary, body))
            .and_then(|()| {
                let file = std::fs::OpenOptions::new().write(true).open(&temporary)?;
                file.sync_all()
            })
            .and_then(|()| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
                }
                #[cfg(not(unix))]
                {
                    Ok(())
                }
            })
            .and_then(|()| std::fs::rename(&temporary, &self.path));

        if let Err(error) = written {
            // Loud, because the cost is not visible until a restart, by which
            // time the threads are gone and nothing says why.
            self.log.error(
                "the thread index could not be written, so a restart will forget threads",
                &crate::log::fields([
                    ("path", self.path.as_str().into()),
                    ("detail", error.to_string().into()),
                ]),
            );
            let _ = std::fs::remove_file(&temporary);
            return;
        }
        Self::sync_directory(&parent);
    }

    /// Flushes the directory entry, so the rename survives a power loss.
    ///
    /// Not every platform allows opening a directory, and not every file
    /// system answers the flush. Either way the rename already happened; only
    /// its durability is weaker, which is not worth failing the write over.
    fn sync_directory(parent: &Path) {
        let Ok(directory) = std::fs::File::open(parent) else {
            return;
        };
        let _ = directory.sync_all();
    }

    /// Keeps an unreadable index rather than overwriting the only copy.
    fn set_aside(&self, reason: &str) {
        let kept = format!("{}.broken", self.path);
        match std::fs::rename(&self.path, &kept) {
            Ok(()) => self.log.error(
                "the thread index was unreadable and was kept aside; no thread can resume",
                &crate::log::fields([("kept", kept.as_str().into()), ("detail", reason.into())]),
            ),
            Err(error) => self.log.error(
                "the thread index was unreadable and could not be kept aside",
                &crate::log::fields([
                    ("path", self.path.as_str().into()),
                    ("detail", error.to_string().into()),
                ]),
            ),
        }
    }
}

#[cfg(test)]
mod tests;
