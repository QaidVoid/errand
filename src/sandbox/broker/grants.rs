//! What a session's owner lets its broker admit beyond `sandbox.egress.allow`.
//!
//! A host the allowlist does not name is refused at once, never held: a chat
//! thread cannot be a modal the agent waits on. The refusal tells the agent
//! what to ask for, and the thread is told once per host and cooldown, so the
//! owner's `!grant` lets the agent's next attempt through. A grant names a
//! host alone, since a tunnel is opaque once it is open.
//!
//! Grants are kept in a file the daemon writes beside the session's state,
//! never inside it, so a resumed session keeps them and the session cannot
//! write its own. A grant holds a host, times and an account id, and nothing
//! a credential could be carried in.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::host_allowed;
use crate::log::{Logger, fields};

/// The file a session's grants are kept in, in its record directory.
pub const GRANTS_FILENAME: &str = "grants.json";

/// How long a refused host goes before the thread is told of it again, so an
/// agent that retries in a loop is one message rather than a flood.
pub const ASK_COOLDOWN_MS: i64 = 60_000;

pub use super::receipts::Clock;

/// How many hosts the thread is asked about within one cooldown, so a
/// session that reaches for many names is a few messages, not one for each.
pub const MAX_ASKS: usize = 3;

/// What is called with a host the broker refused, at most once per host and
/// cooldown.
pub type Ask = Arc<dyn Fn(&str) + Send + Sync>;

/// One thing the owner let through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    /// Short and never issued twice in one session, as `g3`.
    pub id: String,
    /// When it was granted.
    pub created_at: i64,
    /// When it stops admitting, or never while the session lasts.
    pub expires_at: Option<i64>,
    /// Whether the first connection it admits uses it up.
    pub once: bool,
    /// The host, or `*.` and a domain for every name beneath it.
    pub host: String,
    /// The account that granted it.
    pub granted_by: String,
}

/// How long a grant lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lasting {
    /// The next connection to the host, and no more.
    Once,
    /// This many milliseconds.
    For(i64),
    /// The rest of the session, resumes included.
    Always,
}

/// A session's grants, shared by its broker and its thread.
pub struct GrantStore {
    path: PathBuf,
    now: Clock,
    ask: Ask,
    log: Logger,
    held: Mutex<Vec<Grant>>,
    issued: std::sync::atomic::AtomicU64,
    asked: Mutex<HashMap<String, i64>>,
}

impl std::fmt::Debug for GrantStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GrantStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl GrantStore {
    /// The grants kept at `path`, or none when nothing is kept there yet.
    ///
    /// A file that cannot be read or parsed is not taken as granting
    /// anything: the store starts empty and says why, for the thread.
    pub fn open(
        path: impl Into<PathBuf>,
        now: Clock,
        ask: Ask,
        log: Logger,
    ) -> (Self, Option<String>) {
        let path = path.into();
        let (held, problem) = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Vec<Grant>>(&bytes) {
                Ok(held) => (held, None),
                Err(error) => (
                    Vec::new(),
                    Some(format!(
                        "{} is not a list of grants: {error}",
                        path.display()
                    )),
                ),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
            Err(error) => (
                Vec::new(),
                Some(format!("{} could not be read: {error}", path.display())),
            ),
        };
        let issued = held
            .iter()
            .filter_map(|grant| grant.id.strip_prefix('g')?.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        let store = Self {
            path,
            now,
            ask,
            log,
            held: Mutex::new(held),
            issued: std::sync::atomic::AtomicU64::new(issued),
            asked: Mutex::new(HashMap::new()),
        };
        (store, problem)
    }

    /// Whether a grant in force names `host`, using nothing up.
    pub fn holds(&self, host: &str) -> bool {
        let now = (self.now)();
        self.held
            .lock()
            .expect("the grants lock")
            .iter()
            .any(|grant| live(grant, now) && names(grant, host))
    }

    /// The grant in force that would let `host` through, when one would.
    pub fn covering(&self, host: &str) -> Option<Grant> {
        let now = (self.now)();
        self.held
            .lock()
            .expect("the grants lock")
            .iter()
            .find(|grant| live(grant, now) && names(grant, host))
            .cloned()
    }

    /// Lets one connection to `host` through on a grant, using it up when it
    /// was for once, and says whether a grant was there to let it through.
    ///
    /// Called once the broker has judged where the host points, so a
    /// connection refused for its address does not spend a grant for once.
    /// Two connections racing for one such grant cannot both have it.
    pub fn spend(&self, host: &str) -> bool {
        let now = (self.now)();
        let mut held = self.held.lock().expect("the grants lock");
        held.retain(|grant| live(grant, now));
        // A standing grant that names the host is used before one for once,
        // which would otherwise be spent on a connection already admitted.
        let Some(at) = held
            .iter()
            .position(|grant| !grant.once && names(grant, host))
            .or_else(|| held.iter().position(|grant| names(grant, host)))
        else {
            return false;
        };
        if held[at].once {
            held.remove(at);
            // A grant kept on disk past its one use would come back on a
            // resume, so that is said.
            if let Err(why) = self.write(&held) {
                self.log.warn(
                    "a grant for once was used, but could not be removed from disk, so a resume \
                     would bring it back",
                    &fields([("host", host.into()), ("detail", why.into())]),
                );
            }
        }
        true
    }

    /// Notes that `host` was refused, and asks the thread about it when it is
    /// a name a grant could be written for, it was not asked about within the
    /// cooldown, and fewer than [`MAX_ASKS`] hosts were.
    ///
    /// The bound across hosts is what keeps a session that reaches for many
    /// names from flooding the thread, and the map from growing with them.
    pub fn refused(&self, host: &str) {
        let host = host.to_ascii_lowercase();
        if !askable(&host) {
            return;
        }
        let now = (self.now)();
        let due = {
            let mut asked = self.asked.lock().expect("the asked lock");
            asked.retain(|_, at| now.saturating_sub(*at) < ASK_COOLDOWN_MS);
            let due = !asked.contains_key(&host) && asked.len() < MAX_ASKS;
            if due {
                asked.insert(host.clone(), now);
            }
            due
        };
        if due {
            (self.ask)(&host);
        }
    }

    /// Grants `host` for as long as `lasting` says. The grant is in force
    /// even when it could not be kept, which the error says.
    pub fn grant(&self, host: &str, lasting: Lasting, granted_by: &str) -> (Grant, Option<String>) {
        let now = (self.now)();
        let mut held = self.held.lock().expect("the grants lock");
        held.retain(|grant| live(grant, now));
        let next = self
            .issued
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let id = format!("g{next}");
        let grant = Grant {
            id,
            created_at: now,
            expires_at: match lasting {
                Lasting::For(ms) => Some(now.saturating_add(ms)),
                Lasting::Once | Lasting::Always => None,
            },
            once: lasting == Lasting::Once,
            host: host.to_owned(),
            granted_by: granted_by.to_owned(),
        };
        held.push(grant.clone());
        (grant, self.write(&held).err())
    }

    /// Drops every grant for `host`, as written, and says how many went.
    pub fn revoke(&self, host: &str) -> (usize, Option<String>) {
        let mut held = self.held.lock().expect("the grants lock");
        let before = held.len();
        held.retain(|grant| !grant.host.eq_ignore_ascii_case(host));
        let gone = before - held.len();
        let problem = if gone > 0 {
            self.write(&held).err()
        } else {
            None
        };
        (gone, problem)
    }

    /// The grants in force, oldest first.
    pub fn list(&self) -> Vec<Grant> {
        let now = (self.now)();
        self.held
            .lock()
            .expect("the grants lock")
            .iter()
            .filter(|grant| live(grant, now))
            .cloned()
            .collect()
    }

    /// Writes the grants whole, to a file only the daemon user can read,
    /// renamed into place so a reader never sees half of one.
    fn write(&self, held: &[Grant]) -> Result<(), String> {
        let written = serde_json::to_vec_pretty(held).map_err(|error| error.to_string())?;
        let directory = self.path.parent().unwrap_or(Path::new("."));
        let saving = || -> std::io::Result<()> {
            std::fs::create_dir_all(directory)?;
            let mut file = tempfile::NamedTempFile::new_in(directory)?;
            file.write_all(&written)?;
            file.as_file().sync_all()?;
            file.persist(&self.path).map_err(|error| error.error)?;
            Ok(())
        };
        saving().map_err(|error| {
            format!(
                "the grants could not be saved to {}: {error}",
                self.path.display()
            )
        })
    }
}

/// Whether a grant names `host`.
fn names(grant: &Grant, host: &str) -> bool {
    host_allowed(host, std::slice::from_ref(&grant.host))
}

/// Whether a refused host is a name a grant can be suggested for: one exact
/// name, as `parse_grant` reads it. A wildcard or anything else a session
/// wrote is never put in front of the owner to paste.
pub fn askable(host: &str) -> bool {
    !host.contains('*')
        && parse_grant(host).is_ok_and(|(parsed, _)| parsed == host.to_ascii_lowercase())
}

/// Whether a grant still admits anything at `now`.
fn live(grant: &Grant, now: i64) -> bool {
    grant.expires_at.is_none_or(|at| now < at)
}

/// Reads what `!grant` was given: a host, then how long, as `10m` or
/// `always`, or nothing for once.
pub fn parse_grant(rest: &str) -> Result<(String, Lasting), String> {
    let mut words = rest.split_whitespace();
    let Some(host) = words.next() else {
        return Err("say which host, as `!grant api.example.com 10m`".to_owned());
    };
    let host = host.to_ascii_lowercase();
    let name = host.strip_prefix("*.").unwrap_or(&host);
    let plain = !name.is_empty()
        && !name.starts_with('.')
        && !name.ends_with('.')
        && !name.contains("..")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.');
    if !plain {
        return Err(format!(
            "`{host}` is not a host. Name one, as `api.example.com`, or every name beneath a \
             domain, as `*.example.com`"
        ));
    }
    let length = words.collect::<Vec<_>>().join(" ");
    let lasting = match length.as_str() {
        "" => Lasting::Once,
        "always" => Lasting::Always,
        written => match crate::config::duration::parse_duration(written) {
            Some(ms) if ms > 0 => Lasting::For(ms),
            _ => {
                return Err(format!(
                    "`{written}` is not how long, such as `10m`, `1h` or `always`"
                ));
            }
        },
    };
    Ok((host, lasting))
}

#[cfg(test)]
mod tests;
