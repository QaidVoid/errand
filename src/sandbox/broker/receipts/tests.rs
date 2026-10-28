//! Tests for the record of what brokered credentials were used for.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use super::{Ledger, Outcome, Receipt, ReceiptGuard, ReceiptStore, receipts_for};
use crate::log::{LogFields, Logger};

fn receipt(session_id: &str, path: &str) -> Receipt {
    Receipt {
        at: 0,
        session_id: session_id.to_owned(),
        provider: "/provider/zai".to_owned(),
        credential_name: Some("ZAI_KEY".to_owned()),
        upstream_host: Some("api.example.com".to_owned()),
        method: "POST".to_owned(),
        path: path.to_owned(),
        query_bytes: None,
        status: Some(200),
        outcome: Outcome::Completed,
        elapsed_ms: 3,
        bytes_out: 10,
        bytes_in: 20,
    }
}

fn recording() -> (Logger, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&lines);
    let log = Logger::new(
        LogFields::new(),
        Arc::new(move |_level, line| kept.lock().unwrap().push(line.to_owned())),
    );
    (log, lines)
}

/// What the daemon writes, the CLI reads back from another connection, by
/// session and newest first, without creating a store that is not there.
#[test]
fn receipts_written_are_read_back_by_session_newest_first() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("receipts.db");
    assert!(receipts_for(&path, "s-1", 20).unwrap().is_empty());
    assert!(!path.exists(), "reading created the store");

    let store = ReceiptStore::open(&path).unwrap();
    let (log, _) = recording();
    for (session, path) in [("s-1", "/one"), ("s-2", "/other"), ("s-1", "/two")] {
        store.record(&receipt(session, path), &log);
    }

    let read = receipts_for(&path, "s-1", 20).unwrap();
    assert_eq!(read, [receipt("s-1", "/two"), receipt("s-1", "/one")]);
    assert_eq!(receipts_for(&path, "s-1", 1).unwrap().len(), 1);
}

/// A receipt that cannot be written is counted and said, and nothing else
/// fails because of it.
#[test]
fn a_failed_write_is_said_with_a_running_count() {
    let root = tempfile::tempdir().unwrap();
    let store = ReceiptStore::open(root.path().join("receipts.db")).unwrap();
    store
        .db
        .lock()
        .unwrap()
        .execute_batch("DROP TABLE broker_receipts")
        .unwrap();
    let (log, lines) = recording();

    store.record(&receipt("s-1", "/one"), &log);
    store.record(&receipt("s-1", "/two"), &log);

    let lines = lines.lock().unwrap();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[1].contains("could not be written") && lines[1].contains("failed_so_far=2"));
    assert!(lines[1].contains("path=/two"));
}

/// A guard writes exactly once: what it was finished with, or interrupted
/// when it is dropped first, stamped and timed by the injected clock.
#[test]
fn a_guard_writes_once_and_a_dropped_one_is_interrupted() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("receipts.db");
    let clock = Arc::new(AtomicI64::new(1_000));
    let ticking = Arc::clone(&clock);
    let ledger = Ledger {
        store: Arc::new(ReceiptStore::open(&path).unwrap()),
        now: Arc::new(move || ticking.fetch_add(5, Ordering::SeqCst)),
        session_id: "s-9".to_owned(),
    };
    let (log, _) = recording();

    let mut finished = ReceiptGuard::begin(Some(&ledger), &log, receipt("", "/done"));
    finished.relayed(7);
    finished.finish(Outcome::Completed);
    {
        let mut dropped = ReceiptGuard::begin(Some(&ledger), &log, receipt("", "/cut"));
        dropped.relayed(3);
    }
    ReceiptGuard::begin(None, &log, receipt("", "/nowhere")).finish(Outcome::Completed);

    let read = receipts_for(&path, "s-9", 20).unwrap();
    assert_eq!(read.len(), 2);
    assert_eq!(read[0].path, "/cut");
    assert_eq!(read[0].outcome, Outcome::Interrupted);
    assert_eq!(read[0].bytes_in, 23);
    assert_eq!(read[1].path, "/done");
    assert_eq!(read[1].outcome, Outcome::Completed);
    assert_eq!(read[1].bytes_in, 27);
    assert_eq!(read[1].elapsed_ms, 5);
    assert_eq!(read[1].at, 1_005);
}

/// The store is the daemon user's alone, and a store a later errand wrote is
/// refused rather than read wrongly or written over.
#[test]
fn the_store_is_private_and_a_later_shape_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("receipts.db");
    drop(ReceiptStore::open(&path).unwrap());
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    rusqlite::Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 2)
        .unwrap();
    assert!(ReceiptStore::open(&path).is_err());
    assert!(receipts_for(&path, "s-1", 20).is_err());
}
