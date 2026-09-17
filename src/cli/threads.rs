//! Managing threads and what they left on disk, from the terminal.
//!
//! Deliberately not reachable from chat or from the interface. Removing a
//! thread's data is destructive and irreversible, and the people who can post
//! in a channel are not the same people who administer the host it runs on.
//!
//! Everything here reads the same index the daemon writes. Running it while
//! the daemon is up is safe for the commands that only read; the ones that
//! change the index say so.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use crate::session::registry::{ThreadRecord, ThreadRegistry};

/// Where the project a session worked in is read from the policy it kept.
#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    pub path: String,
}

/// Bytes a thread's state directory holds, or none when it is gone.
type SizeOf =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<u64>> + Send>> + Send + Sync>;
/// Deletes a thread's state directory.
type Remove =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>;
/// The project a session worked in, read back from what it left on disk.
type ProjectOf =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<Project>> + Send>> + Send + Sync>;

/// What the commands need, injected so a test needs no disk and no clock.
pub struct Deps {
    pub registry: Arc<Mutex<ThreadRegistry>>,
    pub size_of: SizeOf,
    pub remove: Remove,
    /// Where session state directories live, so a forgotten one can be found.
    pub state_root: String,
    pub project_of: ProjectOf,
    /// One line of output, wherever the terminal is.
    pub write: Arc<dyn Fn(&str) + Send + Sync>,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
}

const USAGE: &str = "\
usage: errand threads <command>

  list                 every remembered thread, most recent first
  show <thread>        one thread in full, with what it holds on disk
  forget <thread>      stop resuming it, and keep its data
  remove <thread>      forget it and delete its data, needs --yes
  prune                forget threads whose data is already gone
  revive <session>     put a forgotten thread back, needs --thread and --owner";

fn human_size(bytes: u64) -> String {
    let units = ["B", "K", "M", "G", "T"];
    // A directory that would lose precision past 52 bits is terabytes, and
    // the drift is the same one the TypeScript table showed.
    #[allow(clippy::cast_precision_loss)]
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < units.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{}{}", bytes, units[unit])
    } else {
        format!("{size:.1}{}", units[unit])
    }
}

fn ago(at: i64, now: i64) -> String {
    let seconds = (now - at).div_euclid(1000).max(0);
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3600 {
        return format!("{}m", seconds / 60);
    }
    if seconds < 86_400 {
        return format!("{}h", seconds / 3600);
    }
    format!("{}d", seconds / 86_400)
}

/// What looking for a thread found.
pub enum Found {
    One(ThreadRecord),
    /// Several thread ids match the fragment the caller gave.
    Ambiguous(Vec<ThreadRecord>),
}

/// Finds a thread by its id, or by a unique part of one.
///
/// A thread id is a snowflake: nineteen digits that differ only near the end,
/// so the tail is what somebody copies and what tells two of them apart. The
/// head is matched too, since that is what people try first.
pub fn find_thread(records: &[ThreadRecord], wanted: &str) -> Option<Found> {
    if let Some(exact) = records.iter().find(|record| record.thread_id == wanted) {
        return Some(Found::One(exact.clone()));
    }

    for matches in [
        &(|id: &str| id.ends_with(wanted)) as &dyn Fn(&str) -> bool,
        &(|id: &str| id.starts_with(wanted)) as &dyn Fn(&str) -> bool,
    ] {
        let matching: Vec<ThreadRecord> = records
            .iter()
            .filter(|record| matches(&record.thread_id))
            .cloned()
            .collect();
        if matching.len() == 1 {
            return Some(Found::One(matching.into_iter().next().expect("one match")));
        }
        if matching.len() > 1 {
            return Some(Found::Ambiguous(matching));
        }
    }
    None
}

async fn list(deps: &Deps) -> i32 {
    let records = deps.registry.lock().expect("the registry lock").all();
    if records.is_empty() {
        (deps.write)("no threads are remembered");
        return 0;
    }

    let widest = records
        .iter()
        .map(|record| record.thread_id.len())
        .max()
        .unwrap_or(6)
        .max(6);
    (deps.write)(&format!(
        "{:<width$}  {:<18}    used     size  owner",
        "thread",
        "project",
        width = widest
    ));
    for record in &records {
        let bytes = (deps.size_of)(record.state_dir.clone()).await;
        (deps.write)(&format!(
            "{:<width$}  {:<18}  {:>6}  {:>7}  {}",
            record.thread_id,
            truncate(&record.project_name, 18),
            format!("{:>6}", ago(record.updated_at, (deps.now)())),
            bytes.map_or_else(|| "gone".to_owned(), human_size),
            record.owner_id,
            width = widest
        ));
    }
    0
}

fn truncate(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

async fn show(deps: &Deps, wanted: Option<&str>) -> i32 {
    let Some(wanted) = wanted else {
        (deps.write)("say which thread, as `errand threads show <thread>`");
        return 2;
    };

    let found = find_thread(
        &deps.registry.lock().expect("the registry lock").all(),
        wanted,
    );
    let record = match found {
        None => {
            (deps.write)(&format!("no thread here is called {wanted}"));
            return 1;
        }
        Some(Found::Ambiguous(records)) => {
            (deps.write)(&format!(
                "{wanted} matches {} threads; give more of the id",
                records.len()
            ));
            return 1;
        }
        Some(Found::One(record)) => record,
    };

    let bytes = (deps.size_of)(record.state_dir.clone()).await;
    (deps.write)(&format!("thread    {}", record.thread_id));
    (deps.write)(&format!("session   {}", record.session_id));
    (deps.write)(&format!(
        "project   {}  {}",
        record.project_name, record.project_path
    ));
    (deps.write)(&format!(
        "state     {}  {}",
        record.state_dir,
        bytes.map_or_else(|| "gone".to_owned(), human_size)
    ));
    (deps.write)(&format!("owner     {}", record.owner_id));
    (deps.write)(&format!(
        "guests    {}",
        if record.guests.is_empty() {
            "none".to_owned()
        } else {
            record.guests.join(", ")
        }
    ));
    (deps.write)(&format!(
        "used      {} ago",
        ago(record.updated_at, (deps.now)())
    ));
    0
}

fn not_found(deps: &Deps, wanted: &str, ambiguous: Option<&[ThreadRecord]>) -> i32 {
    (deps.write)(&match ambiguous {
        Some(..) => format!("{wanted} matches several threads; give more of the id"),
        None => format!("no thread here is called {wanted}"),
    });
    1
}

fn forget(deps: &Deps, wanted: Option<&str>) -> i32 {
    let Some(wanted) = wanted else {
        (deps.write)("say which thread, as `errand threads forget <thread>`");
        return 2;
    };

    let found = find_thread(
        &deps.registry.lock().expect("the registry lock").all(),
        wanted,
    );
    match found {
        None => not_found(deps, wanted, None),
        Some(Found::Ambiguous(records)) => not_found(deps, wanted, Some(&records)),
        Some(Found::One(record)) => {
            deps.registry
                .lock()
                .expect("the registry lock")
                .forget(&record.thread_id);
            (deps.write)(&format!(
                "forgot {}; its data is still at {}",
                record.thread_id, record.state_dir
            ));
            0
        }
    }
}

async fn remove(deps: &Deps, wanted: Option<&str>, confirmed: bool) -> i32 {
    let Some(wanted) = wanted else {
        (deps.write)("say which thread, as `errand threads remove <thread> --yes`");
        return 2;
    };

    let found = find_thread(
        &deps.registry.lock().expect("the registry lock").all(),
        wanted,
    );
    let record = match found {
        None => return not_found(deps, wanted, None),
        Some(Found::Ambiguous(records)) => return not_found(deps, wanted, Some(&records)),
        Some(Found::One(record)) => record,
    };

    // Asked for rather than assumed: this deletes the agent's history, and
    // nothing else keeps a copy of it.
    if !confirmed {
        let bytes = (deps.size_of)(record.state_dir.clone()).await;
        (deps.write)(&format!(
            "this deletes {}{}",
            record.state_dir,
            bytes.map_or_else(String::new, |size| format!(" and its {}", human_size(size)))
        ));
        (deps.write)("run it again with --yes to go ahead");
        return 1;
    }

    if let Err(error) = (deps.remove)(record.state_dir.clone()).await {
        (deps.write)(&format!("could not delete {}: {}", record.state_dir, error));
        return 1;
    }
    deps.registry
        .lock()
        .expect("the registry lock")
        .forget(&record.thread_id);
    (deps.write)(&format!(
        "removed {} and deleted {}",
        record.thread_id, record.state_dir
    ));
    0
}

async fn prune(deps: &Deps) -> i32 {
    let mut forgotten = 0;
    // The index is read into a value before the loop, so the forget inside
    // cannot wait on the lock the iteration holds.
    let records = deps.registry.lock().expect("the registry lock").all();
    for record in records {
        if (deps.size_of)(record.state_dir.clone()).await.is_some() {
            continue;
        }
        let tail = std::path::Path::new(&record.state_dir)
            .file_name()
            .map_or_else(
                || record.state_dir.clone(),
                |name| name.to_string_lossy().into_owned(),
            );
        (deps.write)(&format!(
            "forgot {}, whose data at {tail} is gone",
            record.thread_id
        ));
        deps.registry
            .lock()
            .expect("the registry lock")
            .forget(&record.thread_id);
        forgotten += 1;
    }
    (deps.write)(&if forgotten == 0 {
        "every remembered thread still has its data".to_owned()
    } else {
        format!("forgot {forgotten}")
    });
    0
}

/// Reads a flag written as `--name value`.
fn flag(args: &[String], name: &str) -> Option<String> {
    let marker = format!("--{name}");
    let at = args.iter().position(|arg| *arg == marker)?;
    let value = args.get(at + 1)?;
    (!value.starts_with("--")).then(|| value.clone())
}

/// Puts a thread back that was forgotten, so its session can be resumed.
///
/// Only the index is rebuilt: the session's own history and the project it
/// worked in are still on disk, and what was lost is which thread they belong
/// to and who owns them. Neither can be read back from the session directory,
/// so both are given here rather than guessed at.
async fn revive(deps: &Deps, session: Option<&str>, args: &[String]) -> i32 {
    let Some(session) = session else {
        (deps.write)(
            "say which session, as `errand threads revive <session> --thread <id> --owner <id>`",
        );
        return 2;
    };
    let Some(thread_id) = flag(args, "thread") else {
        (deps.write)(
            "both --thread and --owner are needed: neither is recorded in the session itself",
        );
        return 2;
    };
    let Some(owner_id) = flag(args, "owner") else {
        (deps.write)(
            "both --thread and --owner are needed: neither is recorded in the session itself",
        );
        return 2;
    };
    if deps
        .registry
        .lock()
        .expect("the registry lock")
        .get(&thread_id)
        .is_some()
    {
        (deps.write)(&format!(
            "thread {thread_id} is already remembered; nothing to put back"
        ));
        return 1;
    }

    let state_dir = std::path::Path::new(&deps.state_root)
        .join(session)
        .display()
        .to_string();
    if (deps.size_of)(state_dir.clone()).await.is_none() {
        (deps.write)(&format!(
            "there is nothing on disk for {session}, so there is nothing to resume"
        ));
        return 1;
    }
    let Some(project) = (deps.project_of)(state_dir.clone()).await else {
        (deps.write)(&format!(
            "{session} does not say which project it worked in, so it cannot be put back"
        ));
        return 1;
    };

    deps.registry
        .lock()
        .expect("the registry lock")
        .remember(ThreadRecord {
            thread_id: thread_id.clone(),
            session_id: session.to_owned(),
            state_dir: state_dir.clone(),
            project_name: project.name.clone(),
            project_path: project.path.clone(),
            owner_id,
            guests: Vec::new(),
            provider: None,
            model: None,
            updated_at: (deps.now)(),
        });
    (deps.write)(&format!(
        "put {session} back on thread {thread_id}, in {} ({})",
        project.name, project.path
    ));
    (deps.write)("post in the thread to resume it");
    0
}

/// Runs one thread command.
///
/// Returns the process exit code: 0 for done, 1 for refused, 2 for misused.
pub async fn run_threads(args: &[String], deps: &Deps) -> i32 {
    let command = args.first().map(String::as_str);
    let target = args.get(1).map(String::as_str);
    let confirmed = args.iter().any(|arg| arg == "--yes");

    match command {
        None | Some("list") => list(deps).await,
        Some("show") => show(deps, target).await,
        Some("forget") => forget(deps, target),
        Some("remove") => remove(deps, target, confirmed).await,
        Some("prune") => prune(deps).await,
        Some("revive") => revive(deps, target, args).await,
        Some("help" | "--help") => {
            (deps.write)(USAGE);
            0
        }
        Some(other) => {
            (deps.write)(&format!("there is no threads command called {other}"));
            (deps.write)(USAGE);
            2
        }
    }
}

#[cfg(test)]
#[path = "threads/tests.rs"]
mod tests;
