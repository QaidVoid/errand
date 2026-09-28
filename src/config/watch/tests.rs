//! Tests for watching the configuration file and applying a change to it.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use super::{AfterReload, POLL, looks_changed, reload, stamp, watch};
use crate::config::live::{LiveConfig, Reloaded};
use crate::config::load::{Environment, load_config};
use crate::log::{LogFields, LogLevel, Logger};
use serde_json::json;

/// What a recording logger kept, shared with the test that reads it.
type Kept = std::sync::Arc<std::sync::Mutex<Vec<(LogLevel, String)>>>;

/// A logger that keeps what it was told, so a test can read the log.
fn recording() -> (Logger, Kept) {
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let kept = std::sync::Arc::clone(&lines);
    (
        Logger::new(
            LogFields::new(),
            std::sync::Arc::new(move |level, line| {
                kept.lock()
                    .expect("the log lock")
                    .push((level, line.to_owned()));
            }),
        ),
        lines,
    )
}

fn said(lines: &std::sync::Mutex<Vec<(LogLevel, String)>>) -> String {
    lines
        .lock()
        .expect("the log lock")
        .iter()
        .map(|(_, line)| line.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn env() -> Environment {
    Environment::new()
}

/// A valid configuration written to a file, and the path it was written to.
fn written(dir: &std::path::Path, model: &str) -> String {
    let settings = json!({
        "chat": { "token": "a.token.value", "channelId": "chan", "allowedUserIds": ["u1"] },
        "agent": {
            "provider": "zai",
            "model": model,
            "providers": { "zai": { "credential": "a-credential" } },
        },
        "projectRoot": dir.join("projects").display().to_string(),
        "stateDir": dir.join("state").display().to_string(),
    });
    let path = dir.join("config.json");
    std::fs::write(&path, settings.to_string()).expect("the file is written");
    path.display().to_string()
}

/// The configuration the daemon was started on, read from the file it was
/// started from, which is the only pair that can be compared honestly.
fn started_from(path: &str) -> LiveConfig {
    let config = load_config(
        path,
        |path| std::fs::read_to_string(path),
        &env(),
        crate::config::load::file_exists,
    )
    .expect("the written configuration is accepted");
    LiveConfig::new(config)
}

/// The headline case: an edit caught half-written is refused, and the daemon
/// carries on serving under what it had rather than stopping.
#[test]
fn a_file_caught_mid_write_leaves_the_daemon_running_on_what_it_had() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let path = written(dir.path(), "first");
    let live = started_from(&path);
    let (log, lines) = recording();

    // What an editor leaves behind for a moment: a file cut off part way.
    std::fs::write(&path, "{\"chat\": {\"token\": \"a.tok").expect("the file is truncated");
    let outcome = reload(&live, &path, &env(), &log);

    assert!(matches!(outcome, Reloaded::Rejected(_)), "{outcome:?}");
    assert_eq!(
        live.get().agent.model.as_deref(),
        Some("first"),
        "the daemon stopped using a configuration it had already accepted"
    );
    assert!(said(&lines).contains("carries on"));
}

/// A file that parses but says something impossible is refused the same way,
/// and for the same reason: it is not a smaller change, it is no change.
#[test]
fn a_file_that_says_something_impossible_is_refused_rather_than_applied() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let path = written(dir.path(), "first");
    let live = started_from(&path);
    let (log, _lines) = recording();

    std::fs::write(&path, json!({ "agent": { "provider": "zai" } }).to_string())
        .expect("the file is written");
    let outcome = reload(&live, &path, &env(), &log);

    assert!(matches!(outcome, Reloaded::Rejected(_)), "{outcome:?}");
    assert_eq!(live.get().agent.model.as_deref(), Some("first"));
    assert_eq!(live.get().chat.channel_id, "chan");
}

/// The point of the whole thing: a good edit is in force without a restart.
#[test]
fn a_good_edit_is_in_force_on_the_next_reload() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let path = written(dir.path(), "first");
    let live = started_from(&path);
    let (log, lines) = recording();

    assert_eq!(
        reload(&live, &path, &env(), &log),
        Reloaded::Unchanged,
        "the file the daemon started on is not a change to it"
    );
    assert!(!said(&lines).contains("reloaded"));

    // And a later one, as a second edit would be.
    let _ = written(dir.path(), "a-much-longer-model-name-than-before");
    assert_eq!(reload(&live, &path, &env(), &log), Reloaded::Changed);
    assert_eq!(
        live.get().agent.model.as_deref(),
        Some("a-much-longer-model-name-than-before"),
        "a second edit did not reach the running daemon"
    );
    assert!(said(&lines).contains("reloaded"));
}

/// A save with no edit in it is not a reload, and saying it was would be a
/// lie in the log.
#[test]
fn a_save_that_changes_nothing_is_not_reported_as_a_reload() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let path = written(dir.path(), "only");
    let live = started_from(&path);
    let (log, lines) = recording();

    assert_eq!(reload(&live, &path, &env(), &log), Reloaded::Unchanged);
    assert!(!said(&lines).contains("reloaded"));
}

/// A file that is not there at all is refused the same way, since a daemon
/// that has one deleted underneath it is not a reason to stop either.
#[test]
fn a_file_that_is_gone_is_refused_rather_than_fatal() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let path = written(dir.path(), "working");
    let live = started_from(&path);
    let (log, _lines) = recording();

    let outcome = reload(
        &live,
        &dir.path().join("absent.json").display().to_string(),
        &env(),
        &log,
    );

    assert!(matches!(outcome, Reloaded::Rejected(_)), "{outcome:?}");
    assert_eq!(live.get().agent.model.as_deref(), Some("working"));
}

/// Every reason is logged, so a person fixing the file sees the whole list
/// rather than playing find-the-next-error.
#[test]
fn every_reason_a_reload_was_refused_reaches_the_log() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let path = written(dir.path(), "working");
    let live = started_from(&path);
    let (log, lines) = recording();

    // A relative project root, which validation refuses, alongside the rest.
    std::fs::write(
        dir.path().join("config.json"),
        json!({
            "chat": { "token": "a.token.value", "channelId": "chan", "allowedUserIds": ["u1"] },
            "agent": { "provider": "zai", "providers": { "zai": { "credential": "c" } } },
            "projectRoot": "relative/root",
            "stateDir": "/tmp/s",
        })
        .to_string(),
    )
    .expect("the file is written");
    let path = dir.path().join("config.json").display().to_string();
    let outcome = reload(&live, &path, &env(), &log);

    let Reloaded::Rejected(problems) = outcome else {
        panic!("the reload was expected to be refused: {outcome:?}");
    };
    assert!(!problems.is_empty());
    // The reasons are in the log rather than swallowed by a summary.
    let said = said(&lines);
    for problem in &problems {
        assert!(said.contains(problem), "{problem} was not logged: {said}");
    }
}

/// A save that changes nothing is not looked at again, so the daemon is not
/// re-reading a file that did not change.
#[test]
fn a_save_that_changes_nothing_is_not_looked_at_twice() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let path = written(dir.path(), "only");
    let mut last = stamp(&path);

    assert!(
        !looks_changed(&mut last, &path),
        "a file already seen is not new"
    );
    assert!(!looks_changed(&mut last, &path));
}

/// An edit is noticed, which is the whole reason the loop exists.
#[test]
fn an_edit_is_noticed() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let path = written(dir.path(), "first");
    let mut last = stamp(&path);

    assert!(!looks_changed(&mut last, &path));

    // A different length, which is what a real edit is.
    std::fs::write(
        &path,
        json!({
            "chat": { "token": "a.token.value", "channelId": "chan", "allowedUserIds": ["u1"] },
            "agent": {
                "provider": "zai",
                "model": "a-much-longer-model-name-than-before",
                "providers": { "zai": { "credential": "a-credential" } },
            },
            "projectRoot": dir.path().join("projects").display().to_string(),
            "stateDir": dir.path().join("state").display().to_string(),
        })
        .to_string(),
    )
    .expect("the file is written");

    assert!(looks_changed(&mut last, &path), "an edit was not noticed");
}

/// A path that cannot be stat'ed is left to the read to report, so the loop
/// does not spin on a file that is not there.
#[test]
fn a_file_that_cannot_be_stat_ed_is_left_to_the_read() {
    let mut last = None;
    assert!(!looks_changed(&mut last, "/nonexistent/errand/config.json"));
}

/// The loop applies an edit to the configuration it was given, which is what
/// makes a change reach the daemon rather than a copy of it.
#[tokio::test]
async fn the_loop_applies_an_edit_to_the_configuration_it_was_given() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let path = written(dir.path(), "first");
    let live = started_from(&path);
    let (log, _lines) = recording();

    let every = Duration::from_millis(20);
    let derived = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&derived);
    let handle = watch(
        live.clone(),
        path.clone(),
        env(),
        log,
        every,
        Arc::new(move || {
            let counted = Arc::clone(&counted);
            Box::pin(async move {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }) as Pin<Box<dyn Future<Output = ()> + Send>>
        }),
    );

    // The loop stamps the file when it starts, so the edit goes in after that
    // or the file it is watching is already the one it will see. Waited for
    // rather than slept for, so the test is not racing the loop.
    tokio::time::sleep(every * 3).await;
    let _ = written(dir.path(), "a-much-longer-model-name-than-before");

    let mut saw = false;
    for _ in 0..200 {
        if live.get().agent.model.as_deref() == Some("a-much-longer-model-name-than-before") {
            saw = true;
            break;
        }
        tokio::time::sleep(every).await;
    }
    handle.abort();

    assert!(saw, "the loop never applied the edit");
    // What is derived from the configuration is re-derived, or an edited
    // `models` list would be in force while `!model` still listed the old one.
    assert_eq!(
        derived.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the model list was not re-read after the change"
    );
}

/// The poll is short enough to feel immediate and long enough not to matter.
#[test]
fn the_poll_is_not_a_busy_loop() {
    assert!(POLL >= Duration::from_secs(1), "the poll is too tight");
    assert!(
        POLL <= Duration::from_secs(10),
        "the poll is too slow to notice"
    );
}

/// A refused change is not a change, so what is derived from the configuration
/// is not re-derived. Asking every provider again because somebody was halfway
/// through an edit would spend requests on a file that was refused.
#[tokio::test]
async fn a_refused_change_does_not_reach_what_is_derived_from_it() {
    let dir = tempfile::tempdir().expect("a temp directory");
    let path = written(dir.path(), "only");
    let live = started_from(&path);
    let (log, _lines) = recording();
    let derived = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&derived);
    let after: AfterReload = Arc::new(move || {
        let counted = Arc::clone(&counted);
        Box::pin(async move {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }) as Pin<Box<dyn Future<Output = ()> + Send>>
    });

    let every = Duration::from_millis(20);
    let handle = watch(live.clone(), path.clone(), env(), log, every, after);
    // The loop stamps the file when it starts, so the cut has to go in after
    // that or the watcher is only ever handed a file it never saw change.
    tokio::time::sleep(every * 3).await;
    // Cut off part way, as an editor leaves a file for a moment.
    std::fs::write(&path, "{\"chat\": {\"token\": \"a.tok").expect("the file is truncated");

    tokio::time::sleep(every * 6).await;
    handle.abort();

    assert_eq!(
        derived.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a refused file was treated as a change"
    );
    assert_eq!(
        live.get().agent.model.as_deref(),
        Some("only"),
        "the daemon stopped using a configuration it had already accepted"
    );
}
