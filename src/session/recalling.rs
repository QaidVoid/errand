//! The daemon's half of a recall: reading queries and writing what matched.
//!
//! The rendered memory block carries only the newest facts that fit its
//! budget. When the agent needs an older one it runs `recall`, which writes a
//! query into the one directory both sides can reach; this picks it up,
//! searches the store, and writes the matches back beside it.
//!
//! The same file exchange as a delegation, and the same polling for the same
//! reason: a request arrives at most a few times a turn, and a watch on a
//! directory inside a sandbox is more machinery than that is worth. Nothing
//! here can fail a turn.

use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::agent::requests::RECALL_DIR;
use crate::log::{LogValue, Logger, fields};
use crate::memory::store::{Fact, MemoryStore, Scope};
use crate::sandbox::paths;

/// How many facts one recall returns, per subject, at most.
const RECALL_LIMIT: i64 = 30;

/// Picks up recall requests for one session and answers them from the store.
///
/// The two subjects a session can recall are fixed for its life: the person
/// who owns it and the project it works in. A shared thread attributes new
/// facts to whoever spoke, but a recall reads the owner's, which is whose
/// memory the session was started under.
pub struct Recalling {
    directory: PathBuf,
    memory: Arc<MemoryStore>,
    owner_id: String,
    project: String,
    log: Logger,
}

impl Recalling {
    /// Watches one session's recall directory against `memory`.
    pub fn new(
        state_dir: &Path,
        memory: Arc<MemoryStore>,
        owner_id: String,
        project: String,
        log: Logger,
    ) -> Self {
        Self {
            directory: state_dir.join(RECALL_DIR),
            memory,
            owner_id,
            project,
            log,
        }
    }

    /// Answers every query waiting in the directory.
    pub fn sweep(&self) {
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            // No directory yet, which is every session that has not recalled.
            return;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".request"))
            .collect();
        names.sort();

        for name in names {
            self.answer(&name);
        }
    }

    fn answer(&self, name: &str) {
        let id = name.strip_suffix(".request").unwrap_or(name).to_owned();
        let path = self.directory.join(name);

        let query = paths::read_beneath(&self.directory.to_string_lossy(), name)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|raw| raw.get("query").and_then(Value::as_str).map(str::to_owned));
        // Removed before it is answered, so a request is never answered twice
        // if a later sweep overlaps this one.
        let _ = std::fs::remove_file(&path);

        let Some(query) = query else {
            self.warn("a recall request could not be read", name);
            self.write(&id, "the recall could not be read");
            return;
        };

        let found = self.look_up(&query);
        self.write(&id, &render(&query, &found));
        self.log.info(
            "answered a recall",
            &fields([
                ("query", LogValue::from(query.as_str())),
                ("found", LogValue::from(found.len())),
            ]),
        );
    }

    /// The project's matches first, then the owner's, newest within each.
    fn look_up(&self, query: &str) -> Vec<Fact> {
        let mut found = self
            .memory
            .search(Scope::Project, &self.project, query, RECALL_LIMIT)
            .unwrap_or_default();
        found.extend(
            self.memory
                .search(Scope::User, &self.owner_id, query, RECALL_LIMIT)
                .unwrap_or_default(),
        );
        found
    }

    /// Writes the answer where the agent is waiting for it.
    ///
    /// Under a temporary name and then renamed, so the agent cannot read half
    /// of one and treat it as the whole answer.
    fn write(&self, id: &str, body: &str) {
        let target = self.directory.join(format!("{id}.answer"));
        let writing = self.directory.join(format!("{id}.answer.writing"));
        let body = if body.ends_with('\n') {
            body.to_owned()
        } else {
            format!("{body}\n")
        };
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&writing)
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(body.as_bytes())
            })
            .and_then(|()| std::fs::rename(&writing, &target));
        if let Err(error) = written {
            self.warn("a recall answer could not be written", &error.to_string());
        }
    }

    fn warn(&self, message: &str, detail: &str) {
        self.log
            .warn(message, &fields([("detail", LogValue::from(detail))]));
    }
}

/// What the agent reads back: the matches, one per line, or a plain nothing.
///
/// Said as an answer to the query so the agent attributes it to what it asked,
/// and empty is stated rather than left blank, because a blank answer reads as
/// a command that failed rather than a search that found nothing.
fn render(query: &str, found: &[Fact]) -> String {
    if found.is_empty() {
        return format!("Nothing recorded matches `{query}`.");
    }
    let mut lines = vec![format!("What is recorded matching `{query}`:")];
    lines.extend(found.iter().map(|fact| format!("- {}", fact.fact)));
    lines.join("\n")
}

#[cfg(test)]
mod tests;
