//! Garbage collection: what a cold session left on disk, collected together
//! once it has gone unused for long enough.
//!
//! A session is collected as a unit or not at all: its state directory, its
//! record, its project when that is its own, and last its index entry, so a
//! sweep cut short leaves an entry still naming what remains. A named project
//! is never collected, a live session never, and nothing is deleted through a
//! link or outside the root it belongs under. Every path is derived from the
//! session's id and the configured roots, never read from anything a session
//! could have written.

use std::path::{Path, PathBuf};

use crate::session::disk::tree_bytes;
use crate::session::record::transcript_path;
use crate::session::registry::ThreadRecord;

/// What sits in the state root and is not a session, so a sweep never
/// touches it.
const NOT_SESSIONS: [&str; 1] = ["probe"];

/// What a sweep is asked to judge by.
pub struct Sweep<'a> {
    /// Where session state directories live.
    pub state_root: &'a str,
    /// Where session projects live.
    pub project_root: &'a str,
    /// Now, in milliseconds since the epoch.
    pub now_ms: i64,
    /// How long a session goes unused before it is collected.
    pub older_than_ms: i64,
    /// Whether a session is running, by its id. A live session is never
    /// collected, whatever the index says of it.
    pub is_live: &'a dyn Fn(&str) -> bool,
}

/// A session a sweep would collect, and what of it is on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cold {
    /// The session's id.
    pub session_id: String,
    /// The thread it ran in, when the index still names it.
    pub thread_id: Option<String>,
    /// Whether its project is its own, named after it, and goes with it.
    pub unnamed: bool,
    /// What would be deleted: the unnamed project, the state directory and
    /// the record, whichever exist.
    pub paths: Vec<PathBuf>,
    /// How much those hold.
    pub bytes: u64,
}

/// The sessions `sweep` would collect: those the index names that have gone
/// unused for long enough, and with `unindexed`, state directories the index
/// no longer names whose last activity is that old.
///
/// A session's last activity is the later of its index entry and the last
/// write to its transcript, which every turn appends to. Unindexed state is
/// only taken when asked for: an index that went missing or would not load
/// reads as empty, and would otherwise make every old session look
/// abandoned. Its project goes with it only when the id is a bare token,
/// which only an unnamed session's is, and no entry names the directory.
pub fn cold_sessions(sweep: &Sweep, records: &[ThreadRecord], unindexed: bool) -> Vec<Cold> {
    let cutoff = sweep.now_ms.saturating_sub(sweep.older_than_ms);
    let mut cold = Vec::new();
    for record in records {
        let active = last_written(sweep, &record.session_id).max(record.updated_at);
        if active >= cutoff || (sweep.is_live)(&record.session_id) {
            continue;
        }
        let unnamed = owns_project(sweep.project_root, record, records);
        cold.extend(what_is_held(
            sweep,
            &record.session_id,
            Some(record.thread_id.clone()),
            unnamed,
        ));
    }
    if !unindexed {
        return cold;
    }
    for session_id in unindexed_state(sweep.state_root, records) {
        if (sweep.is_live)(&session_id) {
            continue;
        }
        let state = Path::new(sweep.state_root).join(&session_id);
        let active = modified_ms(&state).max(last_written(sweep, &session_id));
        if active < cutoff {
            let unnamed = is_bare_token(&session_id)
                && !uses_project(records, &Path::new(sweep.project_root).join(&session_id));
            cold.extend(what_is_held(sweep, &session_id, None, unnamed));
        }
    }
    cold
}

/// Collects one cold session: its state, its record, then its project when
/// it is its own. Stops at the first failure, saying which path, so a later
/// sweep finds the rest and tries again. The index entry is the caller's, to
/// forget once this succeeds.
pub fn collect(sweep: &Sweep, cold: &Cold) -> Result<(), String> {
    let id = &cold.session_id;
    let mut parts = vec![
        (sweep.state_root, id.clone()),
        (sweep.state_root, format!("{id}.record")),
    ];
    if cold.unnamed {
        parts.push((sweep.project_root, id.clone()));
    }
    for (root, name) in parts {
        remove_beneath(root, &name).map_err(|error| {
            format!(
                "could not delete {}: {error}",
                Path::new(root).join(&name).display()
            )
        })?;
    }
    Ok(())
}

/// Whether `record`'s project is its own: named after it, as only an
/// unnamed session's is, and not the project of any other entry.
///
/// A later session can name its project after an old unnamed one's token,
/// and then the directory is that session's too.
pub fn owns_project(project_root: &str, record: &ThreadRecord, records: &[ThreadRecord]) -> bool {
    let own = Path::new(project_root).join(&record.session_id);
    record.project_name == record.session_id
        && !records
            .iter()
            .filter(|other| other.session_id != record.session_id)
            .any(|other| Path::new(&other.project_path) == own)
}

/// Whether any entry works in `project`.
fn uses_project(records: &[ThreadRecord], project: &Path) -> bool {
    records
        .iter()
        .any(|record| Path::new(&record.project_path) == project)
}

/// Whether an id is a token alone, as an unnamed session's is. A named
/// session's id always joins its project name to a token with a dash.
fn is_bare_token(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase())
}

/// When a session's transcript was last written, or never.
fn last_written(sweep: &Sweep, session_id: &str) -> i64 {
    let state = Path::new(sweep.state_root).join(session_id);
    modified_ms(&transcript_path(&state.display().to_string()))
}

/// When a path last changed, in milliseconds since the epoch, or the start
/// of time when it is not there.
fn modified_ms(path: &Path) -> i64 {
    std::fs::symlink_metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|since| i64::try_from(since.as_millis()).ok())
        .unwrap_or(i64::MIN)
}

/// What of a session is on disk, when anything is.
fn what_is_held(
    sweep: &Sweep,
    session_id: &str,
    thread_id: Option<String>,
    unnamed: bool,
) -> Option<Cold> {
    if !plausible_id(session_id) {
        return None;
    }
    let mut paths = Vec::new();
    if unnamed {
        paths.push(Path::new(sweep.project_root).join(session_id));
    }
    paths.push(Path::new(sweep.state_root).join(session_id));
    paths.push(Path::new(sweep.state_root).join(format!("{session_id}.record")));
    paths.retain(|path| std::fs::symlink_metadata(path).is_ok());
    if paths.is_empty() && thread_id.is_none() {
        return None;
    }
    let bytes = paths
        .iter()
        .filter_map(|path| tree_bytes(&path.display().to_string()))
        .sum();
    Some(Cold {
        session_id: session_id.to_owned(),
        thread_id,
        unnamed,
        paths,
        bytes,
    })
}

/// State directories the index does not name, by session id.
fn unindexed_state(state_root: &str, records: &[ThreadRecord]) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(state_root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| {
            !NOT_SESSIONS.contains(&name.as_str())
                && !name.ends_with(".record")
                && plausible_id(name)
                && !records.iter().any(|record| record.session_id == *name)
        })
        .collect()
}

/// Whether a name could be a session's id: one path segment of letters,
/// digits, dashes, underscores and dots, as a project name may hold.
fn plausible_id(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Deletes `name` beneath `root`, creating nothing on the way.
///
/// A link at the name is removed as a link and what it points at is left
/// alone. A directory is deleted only when its real path lies strictly
/// inside the real root, so neither the root itself nor anything a session
/// moved out of it can be named. A name that is not there is nothing to do.
pub fn remove_beneath(root: &str, name: &str) -> std::io::Result<()> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(std::io::Error::other(format!(
            "{name:?} is not one name beneath {root}"
        )));
    }
    let path = Path::new(root).join(name);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if meta.file_type().is_symlink() {
        return std::fs::remove_file(&path);
    }
    if !meta.is_dir() {
        return Err(std::io::Error::other(format!(
            "{} is not a directory",
            path.display()
        )));
    }
    let real_root = std::fs::canonicalize(root)?;
    let real_path = std::fs::canonicalize(&path)?;
    if real_path == real_root || !real_path.starts_with(&real_root) {
        return Err(std::io::Error::other(format!(
            "{} is not inside {}",
            path.display(),
            real_root.display()
        )));
    }
    std::fs::remove_dir_all(&real_path)
}

#[cfg(test)]
mod tests;
