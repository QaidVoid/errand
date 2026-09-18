//! Outbox tests, ported from `outbox_test.ts`.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use super::{DEFAULT_MAX_BUFFERED, Outbox, OutboxTask, TaskError};
use crate::log::{LogFields, LogLevel, Logger};

type Lines = Arc<Mutex<Vec<(LogLevel, String)>>>;

fn logger(lines: &Lines) -> Logger {
    let sink = Arc::clone(lines);
    Logger::new(
        LogFields::new(),
        Arc::new(move |level, line| {
            sink.lock().unwrap().push((level, line.to_owned()));
        }),
    )
}

type Gaps = Arc<Mutex<Vec<usize>>>;

fn outbox(max_buffered: usize) -> (Outbox, Gaps, Lines) {
    let lines: Lines = Arc::new(Mutex::new(Vec::new()));
    let gaps: Gaps = Arc::new(Mutex::new(Vec::new()));
    let gaps_for_announce = Arc::clone(&gaps);
    let announce: super::AnnounceDrops = Arc::new(move |count| {
        let gaps = Arc::clone(&gaps_for_announce);
        Box::pin(async move {
            gaps.lock().unwrap().push(count);
            Ok(())
        })
    });
    let logged = Arc::clone(&lines);
    let box_ = Outbox::new(logger(&logged), announce, max_buffered);
    (box_, gaps, lines)
}

/// Builds a task from a factory, because a task that fails is run again in
/// place, so it has to be able to build its future more than once.
fn task<F, Fut>(run: F) -> OutboxTask
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), TaskError>> + Send + 'static,
{
    Box::new(move || Box::pin(run()) as Pin<Box<dyn Future<Output = Result<(), TaskError>> + Send>>)
}

/// Lets the spawned drain and follow tasks run.
async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

fn failed(why: &str) -> Result<(), TaskError> {
    Err(Box::new(std::io::Error::other(why)) as TaskError)
}

/// A task that records it ran.
fn recording(ran: &Arc<Mutex<Vec<String>>>, word: &str) -> OutboxTask {
    let ran = Arc::clone(ran);
    let word = word.to_owned();
    task(move || {
        let ran = Arc::clone(&ran);
        let word = word.clone();
        async move {
            ran.lock().unwrap().push(word);
            Ok(())
        }
    })
}

/// A task that records it ran, once a release has been signalled.
fn recording_slow(ran: &Arc<Mutex<Vec<String>>>, release: &Arc<Notify>) -> OutboxTask {
    let ran = Arc::clone(ran);
    let release = Arc::clone(release);
    task(move || {
        let ran = Arc::clone(&ran);
        let release = Arc::clone(&release);
        async move {
            release.notified().await;
            ran.lock().unwrap().push("slow".to_owned());
            Ok(())
        }
    })
}

#[tokio::test]
async fn what_is_queued_runs_in_the_order_it_was_queued() {
    let (box_, _gaps, _lines) = outbox(DEFAULT_MAX_BUFFERED);
    let ran = Arc::new(Mutex::new(Vec::new()));

    for index in 1..=3 {
        let ran = Arc::clone(&ran);
        box_.enqueue(task(move || {
            let ran = Arc::clone(&ran);
            async move {
                ran.lock().unwrap().push(index);
                Ok(())
            }
        }));
    }
    box_.flush().await;

    assert_eq!(*ran.lock().unwrap(), [1, 2, 3]);
    assert_eq!(box_.pending(), 0);
}

/// Two overlapping actions on one thread would otherwise interleave.
#[tokio::test]
async fn a_slow_task_holds_the_ones_behind_it_until_it_finishes() {
    let (box_, _gaps, _lines) = outbox(DEFAULT_MAX_BUFFERED);
    let ran = Arc::new(Mutex::new(Vec::new()));
    let release = Arc::new(Notify::new());

    box_.enqueue(recording_slow(&ran, &release));
    box_.enqueue(recording(&ran, "fast"));

    assert_eq!(*ran.lock().unwrap(), Vec::<String>::new());
    release.notify_one();
    box_.flush().await;
    assert_eq!(*ran.lock().unwrap(), ["slow".to_owned(), "fast".to_owned()]);
}

/// Output posted into a closed connection is lost, so it waits instead.
#[tokio::test]
async fn nothing_is_attempted_while_the_connection_is_down() {
    let (box_, _gaps, _lines) = outbox(DEFAULT_MAX_BUFFERED);
    let ran = Arc::new(Mutex::new(Vec::new()));

    box_.set_connected(false);
    box_.enqueue(recording(&ran, "held"));
    box_.flush().await;
    assert_eq!(*ran.lock().unwrap(), Vec::<String>::new());

    box_.set_connected(true);
    box_.flush().await;
    assert_eq!(*ran.lock().unwrap(), ["held".to_owned()]);
}

/// A failed action stays at the head, or a reconnect reorders the thread.
#[tokio::test]
async fn a_task_that_fails_is_retried_in_place_when_the_connection_returns() {
    let (box_, _gaps, lines) = outbox(DEFAULT_MAX_BUFFERED);
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let failing = Arc::new(Mutex::new(true));

    let first_attempts = Arc::clone(&attempts);
    let first_failing = Arc::clone(&failing);
    box_.enqueue(task(move || {
        let attempts = Arc::clone(&first_attempts);
        let failing = Arc::clone(&first_failing);
        async move {
            attempts.lock().unwrap().push("first".to_owned());
            if *failing.lock().unwrap() {
                return failed("the gateway went");
            }
            Ok(())
        }
    }));
    box_.enqueue(recording(&attempts, "second"));
    box_.flush().await;

    assert_eq!(*attempts.lock().unwrap(), ["first".to_owned()]);
    assert!(
        lines
            .lock()
            .unwrap()
            .iter()
            .any(|(level, _)| *level == LogLevel::Warn)
    );

    *failing.lock().unwrap() = false;
    box_.set_connected(true);
    box_.flush().await;

    assert_eq!(
        *attempts.lock().unwrap(),
        ["first".to_owned(), "first".to_owned(), "second".to_owned()]
    );
}

/// A disconnected daemon must not grow without limit.
#[tokio::test]
async fn a_full_buffer_drops_the_oldest_and_keeps_the_newest() {
    let (box_, gaps, _lines) = outbox(3);
    let ran = Arc::new(Mutex::new(Vec::new()));

    box_.set_connected(false);
    for index in 1..=5 {
        let ran = Arc::clone(&ran);
        box_.enqueue(task(move || {
            let ran = Arc::clone(&ran);
            async move {
                ran.lock().unwrap().push(index);
                Ok(())
            }
        }));
    }
    assert_eq!(box_.pending(), 3);
    assert_eq!(box_.dropped_count(), 2);

    box_.set_connected(true);
    box_.flush().await;

    assert_eq!(*ran.lock().unwrap(), [3, 4, 5]);
    // A truncated conversation is never passed off as a complete one.
    assert_eq!(*gaps.lock().unwrap(), [2]);
}

#[tokio::test]
async fn a_gap_is_announced_once_not_on_every_drain_after_it() {
    let (box_, gaps, _lines) = outbox(1);

    box_.set_connected(false);
    for _ in 0..3 {
        box_.enqueue(task(|| async { Ok(()) }));
    }
    box_.set_connected(true);
    box_.flush().await;
    box_.flush().await;
    box_.enqueue(task(|| async { Ok(()) }));
    box_.flush().await;

    assert_eq!(*gaps.lock().unwrap(), [2]);
}

#[tokio::test]
async fn closing_discards_what_is_queued_and_takes_nothing_more() {
    let (box_, _gaps, _lines) = outbox(DEFAULT_MAX_BUFFERED);
    let ran = Arc::new(Mutex::new(Vec::new()));

    box_.set_connected(false);
    box_.enqueue(recording(&ran, "queued"));
    box_.close();
    box_.enqueue(recording(&ran, "after"));

    box_.set_connected(true);
    box_.flush().await;

    assert_eq!(*ran.lock().unwrap(), Vec::<String>::new());
    assert!(box_.is_closed());
    assert_eq!(box_.pending(), 0);
}

/// A caller must not see an empty-looking outbox while a drain is in flight.
#[tokio::test]
async fn flushing_waits_for_a_drain_already_running() {
    let (box_, _gaps, _lines) = outbox(DEFAULT_MAX_BUFFERED);
    let ran = Arc::new(Mutex::new(Vec::new()));
    let release = Arc::new(Notify::new());

    box_.enqueue(recording_slow(&ran, &release));
    let flushing = box_.flush();
    release.notify_one();
    flushing.await;

    assert_eq!(*ran.lock().unwrap(), ["slow".to_owned()]);
}

#[tokio::test]
async fn a_failure_to_announce_a_gap_is_reported_and_not_lost() {
    let lines: Lines = Arc::new(Mutex::new(Vec::new()));
    let logged = Arc::clone(&lines);
    let announce: super::AnnounceDrops =
        Arc::new(|_count| Box::pin(async { failed("the thread is gone") }));
    let box_ = Outbox::new(logger(&logged), announce, 1);

    box_.set_connected(false);
    box_.enqueue(task(|| async { Ok(()) }));
    box_.enqueue(task(|| async { Ok(()) }));
    box_.set_connected(true);
    box_.flush().await;

    let said = lines
        .lock()
        .unwrap()
        .iter()
        .map(|(_, line)| line.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(said.contains("reporting dropped messages"));
}

#[tokio::test]
async fn a_followed_outbox_picks_back_up_when_the_connection_returns() {
    // A failed task turns the outbox off on its own. Without something to
    // turn it back on, the thread stops posting for the rest of the session.
    let (connection, watching) = tokio::sync::watch::channel(true);
    let (box_, _gaps, _lines) = outbox(8);
    box_.follow(watching);
    let attempts = Arc::new(Mutex::new(0_usize));
    box_.enqueue(task(move || {
        let attempts = Arc::clone(&attempts);
        async move {
            let mut count = attempts.lock().unwrap();
            *count += 1;
            if *count == 1 {
                failed("the gateway went away")
            } else {
                Ok(())
            }
        }
    }));
    settle().await;
    assert!(box_.pending() > 0, "the failed task is kept");

    let _ = connection.send(false);
    settle().await;
    let _ = connection.send(true);
    settle().await;

    assert_eq!(
        box_.pending(),
        0,
        "the buffer drains when the gateway is back"
    );
}

#[tokio::test]
async fn a_closed_outbox_stops_following() {
    let (connection, watching) = tokio::sync::watch::channel(true);
    let (box_, _gaps, _lines) = outbox(8);
    box_.follow(watching);
    box_.close();
    settle().await;

    let _ = connection.send(false);
    settle().await;

    assert!(box_.is_closed());
    assert_eq!(connection.receiver_count(), 0, "the follower let go");
}
