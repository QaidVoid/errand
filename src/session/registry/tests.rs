//! Tests for the thread registry, ported from `registry_test.ts`.

use std::sync::{Arc, Mutex};

use serde_json::json;

use super::{MAX_REMEMBERED, ThreadRecord, ThreadRegistry, is_record};
use crate::log::{LogFields, LogLevel, Logger};
use tempfile::TempDir;

fn record(overrides: impl FnOnce(&mut ThreadRecord)) -> ThreadRecord {
    let mut held = ThreadRecord {
        thread_id: "t-1".to_owned(),
        session_id: "s-1".to_owned(),
        state_dir: "/state/s-1".to_owned(),
        project_name: "demo".to_owned(),
        project_path: "/projects/demo".to_owned(),
        owner_id: "u-1".to_owned(),
        guests: Vec::new(),
        provider: None,
        model: None,
        updated_at: 1_000,
    };
    overrides(&mut held);
    held
}

type Lines = Arc<Mutex<Vec<(LogLevel, String)>>>;

fn collected() -> (Lines, Logger) {
    let lines: Lines = Arc::new(Mutex::new(Vec::new()));
    let sink_lines = Arc::clone(&lines);
    let log = Logger::new(
        LogFields::new(),
        Arc::new(move |level: LogLevel, line: &str| {
            sink_lines.lock().unwrap().push((level, line.to_owned()));
        }),
    );
    (lines, log)
}

fn with_registry() -> (TempDir, ThreadRegistry, String, Lines) {
    let root = TempDir::with_prefix("errand-registry-").expect("a temporary directory");
    let path = root
        .path()
        .join("threads.json")
        .to_string_lossy()
        .into_owned();
    let (lines, log) = collected();
    (root, ThreadRegistry::new(path.clone(), log), path, lines)
}

#[test]
fn what_is_remembered_comes_back_after_a_restart() {
    let (_root, mut registry, path, lines) = with_registry();
    registry.remember(record(|held| held.guests = vec!["u-2".to_owned()]));

    let (reopened_lines, log) = collected();
    let _ = reopened_lines;
    let mut reopened = ThreadRegistry::new(path, log);
    reopened.load();

    assert_eq!(
        reopened.get("t-1").map(|held| held.session_id.as_str()),
        Some("s-1")
    );
    assert_eq!(
        reopened.get("t-1").map(|held| held.guests.clone()),
        Some(vec!["u-2".to_owned()])
    );
    assert_eq!(reopened.size(), 1);
    drop(lines);
}

#[test]
fn a_first_run_has_an_empty_index_and_says_nothing_about_it() {
    let (_root, mut registry, _path, lines) = with_registry();
    registry.load();

    assert_eq!(registry.size(), 0);
    assert_eq!(lines.lock().unwrap().len(), 0);
}

#[test]
fn a_thread_that_is_forgotten_is_not_resumed_again() {
    let (_root, mut registry, path, lines) = with_registry();
    registry.remember(record(|_| {}));
    registry.forget("t-1");

    let (_reopen_lines, log) = collected();
    let mut reopened = ThreadRegistry::new(path, log);
    reopened.load();
    assert!(reopened.get("t-1").is_none());
    let _ = lines;
}

#[test]
fn threads_are_listed_most_recently_used_first() {
    let (_root, mut registry, _path, _lines) = with_registry();
    registry.remember(record(|held| {
        held.thread_id = "old".to_owned();
        held.updated_at = 1;
    }));
    registry.remember(record(|held| {
        held.thread_id = "new".to_owned();
        held.updated_at = 9;
    }));
    registry.remember(record(|held| {
        held.thread_id = "mid".to_owned();
        held.updated_at = 5;
    }));

    assert_eq!(
        registry
            .all()
            .iter()
            .map(|entry| entry.thread_id.as_str())
            .collect::<Vec<_>>(),
        ["new", "mid", "old"]
    );
}

#[test]
fn the_oldest_are_dropped_once_the_bound_is_passed() {
    let (_root, mut registry, _path, _lines) = with_registry();
    for index in 0..(MAX_REMEMBERED + 3) {
        let index = i64::try_from(index).expect("a count fits");
        registry.remember(record(|held| {
            held.thread_id = format!("t-{index}");
            held.updated_at = index;
        }));
    }

    assert_eq!(registry.size(), MAX_REMEMBERED);
    assert!(registry.get("t-0").is_none());
    assert!(registry.get("t-2").is_none());
    assert!(registry.get(&format!("t-{}", MAX_REMEMBERED + 2)).is_some());
}

/// The rename is what makes it atomic; nothing may be left half written.
#[test]
fn writing_leaves_no_temporary_file_behind() {
    let (root, mut registry, _path, _lines) = with_registry();
    registry.remember(record(|_| {}));

    let mut left: Vec<String> = std::fs::read_dir(root.path())
        .expect("readable")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    left.sort();
    assert_eq!(left, vec!["threads.json".to_owned()]);
}

/// It is the only copy of what was there, so it is kept for someone to look at
/// rather than quietly replaced.
#[test]
fn an_index_that_will_not_parse_is_kept_aside_loudly() {
    let (root, mut registry, path, lines) = with_registry();
    std::fs::write(&path, "{ this is not json").expect("written");

    registry.load();

    assert_eq!(registry.size(), 0);
    assert!(
        std::fs::metadata(format!("{path}.broken"))
            .expect("kept aside")
            .is_file()
    );
    let taken = lines.lock().unwrap();
    assert_eq!(taken[0].0, LogLevel::Error);
    assert!(taken[0].1.contains("kept aside"));
    drop(root);
}

#[test]
fn entries_that_are_not_records_are_dropped_and_the_rest_survive() {
    let (_root, mut registry, path, lines) = with_registry();
    let good = serde_json::to_value(record(|_| {})).expect("serialises");
    let second =
        serde_json::to_value(record(|held| held.thread_id = "t-2".to_owned())).expect("serialises");
    std::fs::write(
        &path,
        serde_json::to_string(&vec![good, json!({ "threadId": "half" }), second]).expect("written"),
    )
    .expect("written");

    registry.load();

    assert_eq!(registry.size(), 2);
    let taken = lines.lock().unwrap();
    assert_eq!(taken[0].0, LogLevel::Warn);
    assert!(taken[0].1.contains("skipped=1"));
}

/// The cost is invisible until a restart, by which time the threads are gone.
#[test]
fn an_index_that_cannot_be_written_says_so_at_the_time() {
    let (_root, _registry, path, lines) = with_registry();
    // A file where a directory has to be, so the write cannot succeed.
    std::fs::write(&path, "not a directory").expect("written");
    let sink_lines = Arc::clone(&lines);
    let log = Logger::new(
        LogFields::new(),
        Arc::new(move |level: LogLevel, line: &str| {
            sink_lines.lock().unwrap().push((level, line.to_owned()));
        }),
    );
    let mut blocked = ThreadRegistry::new(format!("{path}/nested/threads.json"), log);

    blocked.remember(record(|_| {}));

    let taken = lines.lock().unwrap();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].0, LogLevel::Error);
    assert!(taken[0].1.contains("restart will forget threads"));
}

/// A record written before the model was remembered must still load.
#[test]
fn a_record_without_a_model_is_still_a_record() {
    let older = json!({
        "threadId": "t1",
        "sessionId": "s1",
        "stateDir": "/tmp/s1",
        "projectName": "demo",
        "projectPath": "/tmp/demo",
        "ownerId": "u1",
        "guests": [],
        "updatedAt": 1,
    });
    assert!(is_record(&older));
    let with_model = {
        let mut copy = older.clone();
        copy["provider"] = json!("meta");
        copy["model"] = json!("muse:xhigh");
        copy
    };
    assert!(is_record(&with_model));
    // A model that is not a name is not a record.
    let bad_model = {
        let mut copy = older.clone();
        copy["model"] = json!(7);
        copy
    };
    assert!(!is_record(&bad_model));
}
