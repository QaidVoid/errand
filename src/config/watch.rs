//! Watching the configuration file and applying a change to it while the
//! daemon runs.
//!
//! Polled rather than watched through the operating system, because a
//! configuration file is edited a handful of times a day and the alternative
//! is a dependency and a thread per daemon for a check that costs one stat.
//!
//! Every failure here is a reason to carry on, never a reason to stop. A file
//! caught mid-write does not parse, and a daemon that stopped serving because
//! somebody was halfway through an edit would be worse than one carrying on
//! under the configuration it already had.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::config::live::{LiveConfig, Reloaded};
use crate::config::load::{Environment, file_exists, load_config};
use crate::log::{LogValue, Logger, fields};

/// How often the file is looked at. Long enough not to matter, short enough
/// that an edit is in force while the person who made it is still looking.
pub const POLL: Duration = Duration::from_secs(2);

/// What a file looks like from outside: when it changed and how big it is.
///
/// Size is in there because an editor that saves without touching the
/// timestamp still wrote something, and a file that was truncated and put back
/// the same length within the clock's resolution would otherwise look
/// unchanged. A file that cannot be read has no stamp, which is what makes
/// creating one look like a change.
pub fn stamp(path: &str) -> Option<Stamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

/// A file's modification time and length, as compared between polls.
type Stamp = (SystemTime, u64);

/// Whether the file looks different from the last stamp taken.
///
/// Compares a stamp rather than the content, since reading the whole file
/// every two seconds to conclude nothing has changed is work the daemon does
/// not need to do. A file that cannot be stamped is left to the read, which
/// says what could not be read rather than guessing that it was deleted.
pub fn looks_changed(last: &mut Option<Stamp>, path: &str) -> bool {
    let Some(current) = stamp(path) else {
        return false;
    };
    if *last == Some(current) {
        return false;
    }
    *last = Some(current);
    true
}

/// Reads the file and puts what it says in force, and says what came of it.
///
/// The whole file is read and validated before anything is applied, so a
/// half-written one is refused whole and the daemon carries on rather than
/// running on half an edit.
///
/// Reads the file directly, so it is called from a blocking task rather than
/// on the runtime's own threads. A configuration file is a few kilobytes, and
/// blocking a worker on it is cheaper than moving the read off one.
pub fn reload(live: &LiveConfig, path: &str, env: &Environment, log: &Logger) -> Reloaded {
    let incoming = load_config(path, |path| std::fs::read_to_string(path), env, file_exists);
    let outcome = live.reload(incoming);
    report(&outcome, path, log);
    outcome
}

/// Says what a reload did, in the log, and what to do about it if it failed.
///
/// Every reason is logged rather than the first. The person who has to fix the
/// file is the one reading this, and one error at a time makes each fix
/// another round trip.
fn report(outcome: &Reloaded, path: &str, log: &Logger) {
    match outcome {
        Reloaded::Unchanged => log.debug(
            "the configuration file was saved without changing it",
            &fields([("path", LogValue::from(path))]),
        ),
        Reloaded::Changed => log.info(
            "the configuration was reloaded",
            &fields([
                ("path", LogValue::from(path)),
                (
                    "note",
                    LogValue::from(
                        "the connection, the sandbox backend, and sessions already running keep \
                         what they were started with",
                    ),
                ),
            ]),
        ),
        Reloaded::Rejected(problems) => {
            log.error(
                "the configuration file could not be used, so the daemon carries on with the \
                 one it is running on",
                &fields([("path", LogValue::from(path))]),
            );
            for problem in problems {
                log.error(problem, &fields([]));
            }
        }
    }
}

/// What the watcher does after a change is in force.
///
/// The configuration is not the only thing derived from it: which models a
/// session may switch to is read out of the providers it defines, and that
/// reading has to be taken again or an edited `models` list changes nothing
/// anybody can see. Called only when a change was actually applied, and only
/// for the change itself, so a save that says nothing does not reach it.
pub type AfterReload = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Watches the configuration file for as long as the daemon runs.
///
/// Owns nothing but the loop: the configuration it applies to is shared, so
/// the reload reaches every holder through them rather than through anything
/// this holds.
pub fn watch(
    live: LiveConfig,
    path: String,
    env: Environment,
    log: Logger,
    every: Duration,
    after: AfterReload,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last = stamp(&path);
        loop {
            tokio::time::sleep(every).await;
            if !looks_changed(&mut last, &path) {
                continue;
            }
            // Off the runtime's threads, since the read is blocking and the
            // other tasks here are answering messages. The handle is a clone
            // of a shared configuration, not a copy of it.
            let (live, path, env, log) = (live.clone(), path.clone(), env.clone(), log.clone());
            let applied =
                tokio::task::spawn_blocking(move || reload(&live, &path, &env, &log)).await;
            // Only a change that was actually applied reaches what is derived
            // from the configuration. A refused one left the same
            // configuration in force, so there is nothing new to derive from.
            if matches!(applied, Ok(Reloaded::Changed)) {
                after().await;
            }
        }
    })
}

#[cfg(test)]
mod tests;
