//! Durable memory, so the agent knows who it is talking to and where it is.
//!
//! SQLite through rusqlite with the bundled library, so the binary does not
//! depend on whichever libsqlite a host happens to carry. The data is
//! kilobytes of short facts; anything heavier would be machinery in search of
//! a use.
//!
//! Facts are kept short and injected as plain lines rather than as JSON,
//! because every one of them is paid for in the agent's context on every
//! session that user starts.
//!
//! The store is synchronous and small; it is reached off the async runtime
//! through `spawn_blocking` where the daemon calls it.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

/// What a fact is about.
///
/// `user` follows a person between sessions. `project` follows the working
/// directory, so a decision made in one thread is known to the next thread
/// that works there, whoever starts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Held for one person.
    User,
    /// Held for one working directory.
    Project,
}

impl Scope {
    fn as_str(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Project => "project",
        }
    }
}

/// One remembered fact.
///
// `fact` is what the field is called in the store and in every message that
// carries one.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq)]
pub struct Fact {
    /// Where it sits in the store, which orders it against its neighbours.
    pub id: i64,
    /// What was remembered.
    pub fact: String,
    /// When it was remembered, in milliseconds.
    pub created_at: i64,
}

/// Longest single fact kept. Anything longer is a note, not a fact.
pub const MAX_FACT_LENGTH: usize = 300;

/// Default ceiling on the memory block injected into a session.
pub const DEFAULT_MEMORY_BUDGET: usize = 2_000;

/// Facts kept for a project before the oldest are dropped.
pub const MAX_PROJECT_FACTS: i64 = 500;

/// How many facts render by default, newest first.
const DEFAULT_FACT_LIMIT: i64 = 100;

/// Memory backed by one SQLite file.
///
/// The connection sits behind a mutex rather than the store living behind
/// one: the methods are short and never hold the lock across a wait, and the
/// session task holds the store through an `Arc`.
pub struct MemoryStore {
    db: Mutex<Connection>,
}

impl MemoryStore {
    /// Opens or creates the store at `path`.
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let db = Connection::open(path)?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
                user_id TEXT PRIMARY KEY,
                display_name TEXT,
                updated_at INTEGER NOT NULL
            );
            -- The unique constraint is what makes remembering idempotent: an
            -- agent that writes the same fact every session must not grow the
            -- block every session.
            CREATE TABLE IF NOT EXISTS facts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                scope TEXT NOT NULL,
                subject TEXT NOT NULL,
                fact TEXT NOT NULL,
                session_id TEXT,
                created_at INTEGER NOT NULL,
                UNIQUE (scope, subject, fact)
            );
            CREATE INDEX IF NOT EXISTS facts_by_subject
                ON facts (scope, subject, id DESC);",
        )?;
        Ok(Self { db: Mutex::new(db) })
    }

    /// Records who an account id belongs to, for addressing them by name.
    pub fn remember_user(
        &self,
        user_id: &str,
        display_name: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.db.lock().expect("the memory lock").execute(
            "INSERT INTO users (user_id, display_name, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (user_id) DO UPDATE SET display_name = excluded.display_name,
                                                 updated_at = excluded.updated_at",
            rusqlite::params![user_id, display_name, now],
        )?;
        Ok(())
    }

    /// The name last seen for an account, if any.
    pub fn display_name(&self, user_id: &str) -> rusqlite::Result<Option<String>> {
        self.db
            .lock()
            .expect("the memory lock")
            .query_row(
                "SELECT display_name FROM users WHERE user_id = ?1",
                [user_id],
                |row| row.get("display_name"),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
    }

    /// Stores one fact, returning false when it was empty or already known, so
    /// a caller can report how much was actually new.
    pub fn remember(
        &self,
        scope: Scope,
        subject: &str,
        fact: &str,
        session_id: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let trimmed = collapse_spaces(fact.trim());
        let trimmed: String = trimmed.chars().take(MAX_FACT_LENGTH).collect();
        if trimmed.is_empty() || subject.is_empty() {
            return Ok(false);
        }

        let written = self.db.lock().expect("the memory lock").execute(
            "INSERT OR IGNORE INTO facts (scope, subject, fact, session_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![scope.as_str(), subject, trimmed, session_id, now],
        )?;
        let stored = written > 0;

        // A project accumulates facts from everyone who works on it, so it is
        // the one that needs pruning. Oldest go first; the newest are the live
        // ones.
        if stored && scope == Scope::Project {
            self.db.lock().expect("the memory lock").execute(
                "DELETE FROM facts WHERE scope = 'project' AND subject = ?1 AND id NOT IN (
                    SELECT id FROM facts WHERE scope = 'project' AND subject = ?1
                    ORDER BY id DESC LIMIT ?2
                )",
                rusqlite::params![subject, MAX_PROJECT_FACTS],
            )?;
        }
        Ok(stored)
    }

    /// Facts for one subject, newest first.
    pub fn facts_for(
        &self,
        scope: Scope,
        subject: &str,
        limit: i64,
    ) -> rusqlite::Result<Vec<Fact>> {
        let db = self.db.lock().expect("the memory lock");
        let mut statement = db.prepare(
            "SELECT id, fact, created_at FROM facts
             WHERE scope = ?1 AND subject = ?2 ORDER BY id DESC LIMIT ?3",
        )?;
        let rows =
            statement.query_map(rusqlite::params![scope.as_str(), subject, limit], |row| {
                Ok(Fact {
                    id: row.get("id")?,
                    fact: row.get("fact")?,
                    created_at: row.get("created_at")?,
                })
            })?;
        rows.collect()
    }

    /// Removes everything held for one subject, returning how many facts went,
    /// so somebody asking to be forgotten is told what was actually there.
    pub fn forget(&self, scope: Scope, subject: &str) -> rusqlite::Result<usize> {
        let gone = self.db.lock().expect("the memory lock").execute(
            "DELETE FROM facts WHERE scope = ?1 AND subject = ?2",
            rusqlite::params![scope.as_str(), subject],
        )?;
        if scope == Scope::User {
            self.db
                .lock()
                .expect("the memory lock")
                .execute("DELETE FROM users WHERE user_id = ?1", [subject])?;
        }
        Ok(gone)
    }

    /// Renders a user's memory as the block appended to the agent's system
    /// prompt.
    ///
    /// Newest facts win the budget, because a contradiction is usually a
    /// correction. Returns an empty string when there is nothing worth
    /// sending, so somebody new costs no context at all.
    pub fn render(&self, user_id: &str, budget: usize) -> rusqlite::Result<String> {
        let name = self.display_name(user_id)?;
        let facts = self.facts_for(Scope::User, user_id, DEFAULT_FACT_LIMIT)?;
        if name.is_none() && facts.is_empty() {
            return Ok(String::new());
        }

        let header = match name {
            None => "You are talking to somebody in a chat thread.".to_owned(),
            Some(name) => format!("You are talking to {name}."),
        };
        let mut lines = vec![header.clone()];
        let mut used = header.len();

        if !facts.is_empty() {
            let intro = "What you have been told about them before:";
            used += intro.len() + 2;
            lines.push(String::new());
            lines.push(intro.to_owned());
            append_within(&mut lines, &facts, used, budget);
        }

        Ok(lines.join("\n"))
    }

    /// Renders what is known about a project, for its session's context.
    pub fn render_project(&self, project: &str, budget: usize) -> rusqlite::Result<String> {
        let facts = self.facts_for(Scope::Project, project, DEFAULT_FACT_LIMIT)?;
        if facts.is_empty() {
            return Ok(String::new());
        }

        let header = format!("What earlier conversations recorded about the {project} project:");
        let mut lines = vec![header.clone()];
        append_within(&mut lines, &facts, header.len(), budget);
        Ok(lines.join("\n"))
    }

    /// Renders memory for somebody who joins a conversation already in
    /// progress.
    ///
    /// Sent with their first message rather than in the system prompt, which
    /// was fixed when the session started. Costs once per speaker per session,
    /// not once per turn.
    pub fn render_for_speaker(&self, user_id: &str, budget: usize) -> rusqlite::Result<String> {
        let block = self.render(user_id, budget)?;
        if block.is_empty() {
            return Ok(String::new());
        }
        let joined = collapse_newlines(&block);
        Ok(format!("[context: {joined}]"))
    }

    /// Closes the database.
    #[allow(
        dead_code,
        reason = "the daemon closes by dropping the store; the tests close to surface an error"
    )]
    pub fn close(self) -> rusqlite::Result<()> {
        self.db
            .into_inner()
            .expect("the memory lock")
            .close()
            .map_err(|(_, error)| error)
    }
}

/// Adds facts as bullets until the budget is spent. Returns what it used.
fn append_within(lines: &mut Vec<String>, facts: &[Fact], used: usize, budget: usize) -> usize {
    let mut spent = used;
    for held in facts {
        let entry = format!("- {}", held.fact);
        let cost = entry.len() + 1;
        if spent + cost > budget {
            break;
        }
        lines.push(entry);
        spent += cost;
    }
    spent
}

/// Any run of whitespace becomes one space, which is what a fact written by
/// hand looks like once it is stored.
fn collapse_spaces(text: &str) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut previous_was_space = false;
    for character in text.chars() {
        if character.is_whitespace() {
            if !previous_was_space {
                collapsed.push(' ');
            }
            previous_was_space = true;
        } else {
            collapsed.push(character);
            previous_was_space = false;
        }
    }
    collapsed.trim_end().to_owned()
}

/// Every run of newlines becomes one space, for a block said on one line.
fn collapse_newlines(text: &str) -> String {
    let mut joined = String::with_capacity(text.len());
    let mut previous_was_newline = false;
    for character in text.chars() {
        if character == '\n' {
            if !previous_was_newline {
                joined.push(' ');
            }
            previous_was_newline = true;
        } else {
            joined.push(character);
            previous_was_newline = false;
        }
    }
    joined.trim_end().to_owned()
}

/// Filename the agent appends facts about the speaker to.
pub const NOTES_FILENAME: &str = "remember.md";

/// Filename the agent appends facts about the project to.
pub const PROJECT_NOTES_FILENAME: &str = "project-notes.md";

/// Filename of the memory block injected into the agent's system prompt.
pub const BLOCK_FILENAME: &str = "memory.md";

/// The instructions appended to the agent's system prompt.
///
/// The agent cannot call back into the daemon, by design, so remembering
/// something is writing a line to a file in its own state directory. The
/// daemon reads that file after each turn.
pub fn memory_instructions(notes_path: &str, project_notes_path: &str) -> String {
    [
        String::new(),
        String::new(),
        "You have two kinds of memory, both carried between separate conversations.".to_owned(),
        "One fact per line in either. Never record secrets, credentials, or anything".to_owned(),
        "you were told in confidence.".to_owned(),
        String::new(),
        format!("About the person speaking: append to {notes_path}. Their preferences, how"),
        "they want things done, what they are working on. More than one person may".to_owned(),
        "take part here; what is known about somebody new is given to you at the".to_owned(),
        "start of their first message, and facts are held per person.".to_owned(),
        String::new(),
        format!("About this project: append to {project_notes_path}. Decisions taken and why,"),
        "conventions, where things live, approaches already tried and rejected.".to_owned(),
        "Anything a later conversation about this project would otherwise have to".to_owned(),
        "rediscover. This is shared by everyone who works here, so write it for a".to_owned(),
        "reader who was not present.".to_owned(),
    ]
    .join("\n")
}

/// Reads facts an agent wrote, one per line.
///
/// Blank lines, markdown bullets, and comment lines are tolerated because the
/// agent writes this by hand and will not be consistent about it.
pub fn parse_notes(contents: &str) -> Vec<String> {
    contents
        .split('\n')
        .filter_map(|line| {
            let trimmed = line.trim();
            let stripped = match trimmed.strip_prefix(['-', '*']) {
                Some(rest) if rest.starts_with(char::is_whitespace) => rest.trim_start(),
                _ => trimmed,
            };
            let keep = !stripped.is_empty() && !stripped.starts_with('#');
            keep.then(|| stripped.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests;
