use std::path::Path;

use super::{Cold, Sweep, cold_sessions, collect, remove_beneath};
use crate::session::registry::ThreadRecord;

const DAY: i64 = 24 * 60 * 60 * 1_000;
const NOW: i64 = 100 * DAY;

struct Roots {
    _root: tempfile::TempDir,
    state: String,
    projects: String,
}

fn roots() -> Roots {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let projects = root.path().join("projects");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&projects).unwrap();
    Roots {
        state: state.display().to_string(),
        projects: projects.display().to_string(),
        _root: root,
    }
}

/// A session as a launch leaves it: its state, its record, and its project.
fn session(roots: &Roots, session_id: &str, project: &str) {
    for directory in [
        Path::new(&roots.state).join(session_id),
        Path::new(&roots.state).join(format!("{session_id}.record")),
        Path::new(&roots.projects).join(project),
    ] {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("held"), "x".repeat(10)).unwrap();
    }
}

fn record(session_id: &str, project: &str, updated_at: i64) -> ThreadRecord {
    ThreadRecord {
        thread_id: format!("thread-{session_id}"),
        session_id: session_id.to_owned(),
        state_dir: String::new(),
        project_name: project.to_owned(),
        project_path: String::new(),
        owner_id: "u".to_owned(),
        guests: Vec::new(),
        provider: None,
        model: None,
        updated_at,
    }
}

fn sweep<'a>(roots: &'a Roots, is_live: &'a dyn Fn(&str) -> bool) -> Sweep<'a> {
    Sweep {
        state_root: &roots.state,
        project_root: &roots.projects,
        now_ms: NOW,
        older_than_ms: 30 * DAY,
        is_live,
    }
}

fn ids(cold: &[Cold]) -> Vec<&str> {
    cold.iter().map(|cold| cold.session_id.as_str()).collect()
}

/// Only what has gone unused past the window is cold, and a live session is
/// not, whatever the index says of it.
#[test]
fn only_a_session_unused_past_the_window_and_not_live_is_cold() {
    let roots = roots();
    for id in ["old", "fresh", "running"] {
        session(&roots, id, id);
    }
    let records = [
        record("old", "old", NOW - 40 * DAY),
        record("fresh", "fresh", NOW - DAY),
        record("running", "running", NOW - 40 * DAY),
    ];
    let live = |id: &str| id == "running";

    let cold = cold_sessions(&sweep(&roots, &live), &records, false);

    assert_eq!(ids(&cold), ["old"]);
    assert_eq!(cold[0].thread_id.as_deref(), Some("thread-old"));
    assert_eq!(cold[0].paths.len(), 3);
    assert_eq!(cold[0].bytes, 30);
}

/// An unnamed session's project goes with it. A named project stays, though
/// the session that worked in it is collected.
#[test]
fn an_unnamed_project_goes_and_a_named_one_stays() {
    let roots = roots();
    session(&roots, "abc123", "abc123");
    session(&roots, "demo-def456", "demo");
    let records = [
        record("abc123", "abc123", 0),
        record("demo-def456", "demo", 0),
    ];
    let live = |_: &str| false;
    let sweep = sweep(&roots, &live);

    for cold in cold_sessions(&sweep, &records, false) {
        collect(&sweep, &cold).expect("collected");
    }

    assert!(!Path::new(&roots.projects).join("abc123").exists());
    assert!(Path::new(&roots.projects).join("demo").exists());
    for gone in [
        "abc123",
        "abc123.record",
        "demo-def456",
        "demo-def456.record",
    ] {
        assert!(!Path::new(&roots.state).join(gone).exists(), "{gone}");
    }
}

/// A state directory the index no longer names is taken only when asked for,
/// since an index that would not load reads as empty, and then by its own
/// age. What the daemon keeps in the state root is never taken for a session.
#[test]
fn an_unindexed_state_directory_is_taken_only_when_asked_and_by_its_age() {
    let roots = roots();
    session(&roots, "orphan", "orphan");
    std::fs::create_dir_all(Path::new(&roots.state).join("probe")).unwrap();
    std::fs::write(Path::new(&roots.state).join("memory.db"), "").unwrap();
    let live = |_: &str| false;
    let mut judged = sweep(&roots, &live);
    judged.now_ms = crate::log::now_ms() + 40 * DAY;

    assert!(
        cold_sessions(&judged, &[], false).is_empty(),
        "not asked for"
    );
    let cold = cold_sessions(&judged, &[], true);
    assert_eq!(ids(&cold), ["orphan"]);
    collect(&judged, &cold[0]).expect("collected");
    assert!(!Path::new(&roots.state).join("orphan").exists());

    session(&roots, "fresh", "fresh");
    judged.now_ms = crate::log::now_ms();
    assert!(
        cold_sessions(&judged, &[], true).is_empty(),
        "a fresh one is kept"
    );

    session(&roots, "running", "running");
    judged.now_ms = crate::log::now_ms() + 40 * DAY;
    let live = |id: &str| id != "nothing";
    judged.is_live = &live;
    assert!(
        cold_sessions(&judged, &[], true).is_empty(),
        "a live one is kept"
    );
}

/// Unindexed, a project goes with its state only when the id is a bare token,
/// as an unnamed session's is, and no entry works in that directory.
#[test]
fn an_unindexed_project_goes_only_when_it_can_only_be_the_sessions() {
    let roots = roots();
    for id in ["abc123", "def456", "demo-ghi789"] {
        session(&roots, id, id);
    }
    let live = |_: &str| false;
    let mut judged = sweep(&roots, &live);
    judged.now_ms = crate::log::now_ms() + 40 * DAY;
    let mut named = record("demo2-jkl012", "def456", judged.now_ms);
    named.project_path = Path::new(&roots.projects)
        .join("def456")
        .display()
        .to_string();

    let mut cold = cold_sessions(&judged, &[named], true);
    cold.sort_by(|a, b| a.session_id.cmp(&b.session_id));

    let unnamed: Vec<_> = cold
        .iter()
        .map(|cold| (cold.session_id.as_str(), cold.unnamed))
        .collect();
    assert_eq!(
        unnamed,
        [("abc123", true), ("def456", false), ("demo-ghi789", false)]
    );
}

/// An unnamed session's directory that a later session took as its named
/// project is that session's too, and stays when the old one is collected.
#[test]
fn a_project_another_session_named_stays() {
    let roots = roots();
    session(&roots, "abc123", "abc123");
    let mut later = record("abc123-def456", "abc123", NOW);
    later.project_path = Path::new(&roots.projects)
        .join("abc123")
        .display()
        .to_string();
    let records = [record("abc123", "abc123", 0), later];
    let live = |_: &str| false;
    let sweep = sweep(&roots, &live);

    let cold = cold_sessions(&sweep, &records, false);

    assert_eq!(ids(&cold), ["abc123"]);
    assert!(!cold[0].unnamed);
    collect(&sweep, &cold[0]).expect("collected");
    assert!(Path::new(&roots.projects).join("abc123").exists());
}

/// A session whose index entry is old but whose transcript was written
/// recently is still in use, and is kept.
#[test]
fn a_recent_turn_keeps_a_session_its_index_calls_old() {
    let roots = roots();
    session(&roots, "busy", "busy");
    std::fs::write(
        Path::new(&roots.state).join("busy.record/transcript.jsonl"),
        "{}\n",
    )
    .unwrap();
    let live = |_: &str| false;
    let mut judged = sweep(&roots, &live);
    judged.now_ms = crate::log::now_ms();

    let cold = cold_sessions(&judged, &[record("busy", "busy", 0)], false);

    assert!(cold.is_empty(), "{cold:?}");
}

/// A link planted at a session's name is removed as a link, and what it
/// points at survives. The root itself, and a name that climbs out, are
/// refused.
#[test]
fn nothing_is_deleted_through_a_link_or_outside_the_root() {
    let roots = roots();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("precious"), "keep").unwrap();
    std::os::unix::fs::symlink(outside.path(), Path::new(&roots.state).join("linked")).unwrap();

    remove_beneath(&roots.state, "linked").expect("the link is removed");

    assert!(!Path::new(&roots.state).join("linked").exists());
    assert!(outside.path().join("precious").exists());
    for name in ["", ".", "..", "a/b"] {
        assert!(remove_beneath(&roots.state, name).is_err(), "{name:?}");
    }
    assert!(Path::new(&roots.state).exists());
    remove_beneath(&roots.state, "missing").expect("nothing to do");
    assert!(!Path::new(&roots.state).join("missing").exists());
}

/// A path that fails to delete stops the collection, so the caller keeps the
/// index entry and a later sweep tries again.
#[test]
fn a_failed_delete_stops_the_collection() {
    let roots = roots();
    let file = Path::new(&roots.state).join("stuck");
    std::fs::write(&file, "not a directory").unwrap();
    let live = |_: &str| false;
    let sweep = sweep(&roots, &live);
    let cold = Cold {
        session_id: "stuck".to_owned(),
        thread_id: None,
        unnamed: false,
        paths: vec![file.clone()],
        bytes: 0,
    };

    let failed = collect(&sweep, &cold).expect_err("refused");

    assert!(failed.contains("stuck"), "{failed}");
    assert!(file.exists());
}
