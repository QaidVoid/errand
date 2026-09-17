//! Daemon lock tests, ported from `lock_test.ts`.

use std::process;

use super::{LOCK_FILENAME, acquire_lock, is_running};

fn with_state_dir(run: impl FnOnce(&str)) {
    let dir = tempfile::tempdir().expect("a temp directory");
    run(dir.path().display().to_string().as_str());
}

#[test]
fn taking_the_lock_writes_the_holders_process_id() {
    with_state_dir(|state_dir| {
        let mut lock = acquire_lock(state_dir, 4242).expect("the lock is free");

        assert_eq!(
            lock.path(),
            std::path::Path::new(state_dir).join(LOCK_FILENAME)
        );
        assert_eq!(std::fs::read_to_string(lock.path()).unwrap().trim(), "4242");
        lock.release();
    });
}

/// This process's own id, as the lock file records one.
fn own_pid() -> i32 {
    i32::try_from(process::id()).expect("a pid fits an i32 everywhere the daemon runs")
}

/// Two daemons on one token both act on every message.
#[test]
fn a_second_daemon_is_refused_while_the_first_is_alive() {
    with_state_dir(|state_dir| {
        let mut first = acquire_lock(state_dir, own_pid()).expect("the lock is free");

        let error = acquire_lock(state_dir, 9999).expect_err("the second daemon is refused");

        assert_eq!(error.pid, own_pid());
        assert!(error.to_string().contains(state_dir));
        first.release();
    });
}

/// A killed daemon must not block every restart after it.
#[test]
fn a_lock_left_by_a_process_that_is_gone_is_taken_over() {
    with_state_dir(|state_dir| {
        std::fs::write(
            std::path::Path::new(state_dir).join(LOCK_FILENAME),
            "999999999\n",
        )
        .unwrap();

        let mut lock = acquire_lock(state_dir, 4242).expect("the stale lock is taken");

        assert_eq!(std::fs::read_to_string(lock.path()).unwrap().trim(), "4242");
        lock.release();
    });
}

#[test]
fn a_lock_holding_nonsense_is_treated_as_stale() {
    with_state_dir(|state_dir| {
        std::fs::write(
            std::path::Path::new(state_dir).join(LOCK_FILENAME),
            "not a pid\n",
        )
        .unwrap();

        let mut lock = acquire_lock(state_dir, 4242).expect("the nonsense lock is taken");

        assert_eq!(std::fs::read_to_string(lock.path()).unwrap().trim(), "4242");
        lock.release();
    });
}

#[test]
fn releasing_removes_the_lock_and_twice_is_not_an_error() {
    with_state_dir(|state_dir| {
        let mut lock = acquire_lock(state_dir, 4242).expect("the lock is free");

        lock.release();
        lock.release();

        assert!(!lock.path().exists());
        acquire_lock(state_dir, 4243)
            .expect("the lock is free again")
            .release();
    });
}

#[test]
fn a_process_is_running_when_it_is_and_not_when_it_is_not() {
    assert!(is_running(own_pid()));
    assert!(!is_running(999_999_999));
    assert!(!is_running(0));
    assert!(!is_running(-1));
}
