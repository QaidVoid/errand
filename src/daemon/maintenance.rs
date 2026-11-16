//! Maintenance: draining the daemon so its binary can be replaced, the note
//! a restart leaves for the daemon that comes up after it, what a sweep for
//! cold sessions asked for by hand says it did, and what the host is doing,
//! for `!status` in the channel.
//!
//! A restart loses only the turns running and the prompts waiting when it
//! happens. A session's history and workspace outlive it, so a drain waits
//! for turns rather than sessions, and a turn cut off by a restart is
//! continued once the daemon is back.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::duration::parse_duration;
use crate::session::disk::human_size;
use crate::session::event::{Card, CardColor, CardField};

/// The file a restart leaves in the state directory.
pub const NOTE_FILENAME: &str = "restart.json";

/// What the session of a turn cut off by a restart is told once the daemon is
/// back, in its owner's name.
pub const CONTINUE_PROMPT: &str = "The errand daemon restarted for maintenance while you \
     were working, which cut off the step you were on. Continue the task from where you \
     stopped. Check the state of whatever that step was doing before you repeat it.";

/// Restarts the daemon into the binary it was started from.
#[derive(Clone)]
pub struct Restarter {
    exe: PathBuf,
    trigger: Arc<dyn Fn() + Send + Sync>,
}

impl Restarter {
    /// A restarter into `exe`, which `trigger` asks the running daemon to do.
    pub fn new(exe: PathBuf, trigger: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self { exe, trigger }
    }

    /// Says why the binary could not be started, before anything is drained
    /// for it.
    pub fn check(&self) -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;
        let shown = self.exe.display();
        let found = std::fs::metadata(&self.exe)
            .map_err(|error| format!("{shown} cannot be read: {error}"))?;
        if found.is_file() && found.permissions().mode() & 0o111 != 0 {
            Ok(())
        } else {
            Err(format!("{shown} is not an executable file"))
        }
    }

    /// Asks the daemon to shut down and start again.
    pub fn restart(&self) {
        (self.trigger)();
    }
}

/// A turn that was running, by the thread it runs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    /// The thread the session talks in.
    pub thread_id: String,
    /// Who started the session, in whose name it is continued.
    pub owner_id: String,
}

/// What a restart leaves for the daemon that comes up after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartNote {
    /// Where `!restart` was asked, so the daemon says it is back there.
    pub asked_in: String,
    /// The turns the restart cut off, to be continued.
    pub interrupted: Vec<Turn>,
    /// Threads with a prompt that was waiting for a turn and never ran.
    pub dropped: Vec<String>,
}

/// Writes the note into `state_dir`, whole or not at all.
pub fn leave_note(state_dir: &str, note: &RestartNote) -> std::io::Result<()> {
    let path = Path::new(state_dir).join(NOTE_FILENAME);
    let written = serde_json::to_vec_pretty(note).map_err(std::io::Error::other)?;
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, written)?;
    std::fs::rename(&partial, &path)
}

/// Reads the note a restart left in `state_dir`, and removes it so it is
/// acted on once.
///
/// Returns nothing when there is none, which is every start that did not
/// follow `!restart`.
pub fn take_note(state_dir: &str) -> Result<Option<RestartNote>, String> {
    let path = Path::new(state_dir).join(NOTE_FILENAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{} cannot be read: {error}", path.display())),
    };
    std::fs::remove_file(&path)
        .map_err(|error| format!("{} cannot be removed: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|error| format!("{} does not read as a note: {error}", path.display()))
}

/// How long `!restart` waits for running turns, from what follows it.
pub fn wait_of(argument: &str, configured_ms: u64) -> Result<i64, String> {
    match argument.trim() {
        "" => Ok(i64::try_from(configured_ms).unwrap_or(i64::MAX)),
        "now" => Ok(0),
        given => parse_duration(given).ok_or_else(|| {
            format!("`{given}` is not a wait; say `!restart now`, or a time such as `!restart 30m`")
        }),
    }
}

/// Where a drain stands, as the card that says so.
///
/// `restart_at` is when the daemon restarts at the latest, in epoch
/// milliseconds, or nothing for a drain that only waits.
pub fn draining_card(
    running: &[Turn],
    unthreaded: usize,
    waiting: usize,
    restart_at: Option<i64>,
) -> Card {
    let mut lines = vec![match restart_at {
        Some(_) => "New work is refused until the bot is back.".to_owned(),
        None => "New work is refused until `!undrain`.".to_owned(),
    }];
    lines.extend(
        running
            .iter()
            .map(|turn| format!("- <#{}> for <@{}>", turn.thread_id, turn.owner_id)),
    );
    if unthreaded > 0 {
        lines.push(format!("- {unthreaded} outside any thread"));
    }
    let mut fields = vec![
        CardField {
            name: "running".to_owned(),
            value: (running.len() + unthreaded).to_string(),
        },
        CardField {
            name: "waiting".to_owned(),
            value: waiting.to_string(),
        },
    ];
    if let Some(at) = restart_at {
        fields.push(CardField {
            name: "restarts".to_owned(),
            value: format!("<t:{}:R> at the latest", at / 1000),
        });
    }
    Card {
        color: CardColor::Warning,
        title: if restart_at.is_some() {
            "restarting once turns finish".to_owned()
        } else {
            "draining".to_owned()
        },
        description: lines.join("\n"),
        fields,
        footer: restart_at.map(|_| {
            "A turn still running then is stopped and continued once the bot is back.".to_owned()
        }),
    }
}

/// A drain that has nothing left to wait for.
pub fn drained_card() -> Card {
    Card {
        color: CardColor::Ready,
        title: "drained".to_owned(),
        description: "No turn is running or waiting, so the bot is safe to restart. \
                      `!undrain` accepts work again."
            .to_owned(),
        fields: Vec::new(),
        footer: None,
    }
}

/// The restart is happening, and what it does to the work it cut off.
pub fn restarting_card(note: &RestartNote) -> Card {
    let mut lines = Vec::new();
    if !note.interrupted.is_empty() {
        lines.push(format!(
            "{} running turn(s) will be continued once the bot is back.",
            note.interrupted.len()
        ));
    }
    if !note.dropped.is_empty() {
        lines.push(format!(
            "{} waiting message(s) did not run; their threads will be told.",
            note.dropped.len()
        ));
    }
    Card {
        color: CardColor::Working,
        title: "restarting now".to_owned(),
        description: lines.join("\n"),
        fields: Vec::new(),
        footer: None,
    }
}

/// The daemon is back from a restart.
pub fn back_card(version: &str) -> Card {
    Card {
        color: CardColor::Ready,
        title: "back".to_owned(),
        description: format!("Running errand {version}."),
        fields: Vec::new(),
        footer: None,
    }
}

/// Work is accepted again.
pub fn undrained_card(called_off_restart: bool) -> Card {
    Card {
        color: CardColor::Ready,
        title: "accepting work again".to_owned(),
        description: if called_off_restart {
            "The restart is called off.".to_owned()
        } else {
            "The drain is over.".to_owned()
        },
        fields: Vec::new(),
        footer: None,
    }
}

/// Said in a thread whose turn a restart cut off, as it is continued.
pub fn continued_card() -> Card {
    Card {
        color: CardColor::Working,
        title: "restarted for maintenance".to_owned(),
        description: "The bot restarted during this turn. Continuing where it stopped.".to_owned(),
        fields: Vec::new(),
        footer: None,
    }
}

/// Said in a thread whose waiting message a restart dropped.
pub fn dropped_card() -> Card {
    Card {
        color: CardColor::Warning,
        title: "restarted for maintenance".to_owned(),
        description: "A message here was waiting for a turn when the bot restarted, and it \
                      did not run. Send it again."
            .to_owned(),
        fields: Vec::new(),
        footer: None,
    }
}

/// The most kept sessions a collection card names; the log names them all.
const KEPT_SHOWN: usize = 10;

/// The most of one reason a collection card shows, so a long path cannot
/// push the card past what the service takes.
const REASON_SHOWN: usize = 200;

/// What a sweep for cold sessions collected and what it kept.
pub fn collection_card(collection: &super::Collection) -> Card {
    let mut lines: Vec<String> = collection
        .kept
        .iter()
        .take(KEPT_SHOWN)
        .map(|(thread_id, why)| {
            let why: String = why.chars().take(REASON_SHOWN).collect();
            format!("- <#{thread_id}>: {why}")
        })
        .collect();
    if collection.kept.len() > KEPT_SHOWN {
        lines.push(format!(
            "- and {} more, named in the log",
            collection.kept.len() - KEPT_SHOWN
        ));
    }
    let quiet = collection.collected == 0 && collection.kept.is_empty();
    Card {
        color: if collection.kept.is_empty() {
            CardColor::Ready
        } else {
            CardColor::Warning
        },
        title: if quiet {
            "nothing to collect".to_owned()
        } else {
            "collected".to_owned()
        },
        description: lines.join("\n"),
        fields: if quiet {
            Vec::new()
        } else {
            vec![
                CardField {
                    name: "collected".to_owned(),
                    value: collection.collected.to_string(),
                },
                CardField {
                    name: "freed".to_owned(),
                    value: human_size(collection.freed),
                },
                CardField {
                    name: "kept".to_owned(),
                    value: collection.kept.len().to_string(),
                },
            ]
        },
        footer: None,
    }
}

/// What the host is doing, as `!status` in the channel shows it.
#[derive(Debug)]
pub struct HostStatus {
    /// The version running.
    pub version: &'static str,
    /// When the daemon started, in epoch milliseconds.
    pub started_at: i64,
    /// Sessions with a sandbox, and the most there may be.
    pub live: (usize, usize),
    /// Turns running, by thread, and the most there may be at once.
    pub running: (Vec<Turn>, usize),
    /// Turns running outside any thread, such as one from the interface.
    pub unthreaded: usize,
    /// Prompts waiting for a turn, and the most that may wait.
    pub waiting: (usize, usize),
    /// Threads that resume when somebody posts in them.
    pub resumable: usize,
    /// Why turns are held back, when they are.
    pub paused: Option<&'static str>,
    /// Whether new work is refused, and when the pending restart happens at
    /// the latest.
    pub draining: (bool, Option<i64>),
}

/// What the host is doing.
pub fn status_card(status: &HostStatus) -> Card {
    let (running, max_turns) = &status.running;
    let (draining, restart_at) = status.draining;
    let mut lines = Vec::new();
    match (draining, restart_at) {
        (true, Some(at)) => lines.push(format!(
            "Restarting for maintenance <t:{}:R> at the latest; new work is refused.",
            at / 1000
        )),
        (true, None) => lines.push("Draining for maintenance; new work is refused.".to_owned()),
        (false, _) => {}
    }
    if let Some(reason) = status.paused {
        lines.push(format!("Turns are held back: {reason}."));
    }
    lines.extend(
        running
            .iter()
            .map(|turn| format!("- <#{}> for <@{}>", turn.thread_id, turn.owner_id)),
    );
    if status.unthreaded > 0 {
        lines.push(format!("- {} outside any thread", status.unthreaded));
    }
    if running.is_empty() && status.unthreaded == 0 {
        lines.push("No turn is running.".to_owned());
    }
    let field = |name: &str, value: String| CardField {
        name: name.to_owned(),
        value,
    };
    Card {
        color: if draining || status.paused.is_some() {
            CardColor::Warning
        } else {
            CardColor::Ready
        },
        title: "host status".to_owned(),
        description: lines.join("\n"),
        fields: vec![
            field(
                "turns",
                format!("{} of {max_turns}", running.len() + status.unthreaded),
            ),
            field(
                "sessions",
                format!("{} of {}", status.live.0, status.live.1),
            ),
            field(
                "waiting",
                format!("{} of {}", status.waiting.0, status.waiting.1),
            ),
            field("resumable threads", status.resumable.to_string()),
            field("version", status.version.to_owned()),
            field("up since", format!("<t:{}:R>", status.started_at / 1000)),
        ],
        footer: None,
    }
}

/// Maintenance turned down, and why.
pub fn refused_card(why: &str) -> Card {
    Card {
        color: CardColor::Refused,
        title: "maintenance".to_owned(),
        description: why.to_owned(),
        fields: Vec::new(),
        footer: None,
    }
}

#[cfg(test)]
mod tests;
