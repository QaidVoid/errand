//! Tests for the admission scheduler, ported from `scheduler_test.ts`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::{Clock, QueueEntry, Scheduler, SubmitOutcome, Ticket, Timer};
use crate::config::schema::LimitsConfig;

fn limits() -> LimitsConfig {
    LimitsConfig {
        max_concurrent_turns: 2,
        max_live_sessions: 3,
        max_queue_length: 3,
        max_queue_wait_ms: 1_000,
    }
}

/// A clock a test moves by hand, so nothing waits for real time.
struct TestClock {
    now: AtomicI64,
    next: AtomicU64,
    timers: Mutex<HashMap<u64, (i64, Timer)>>,
    dispatch: Mutex<Option<Arc<Scheduler>>>,
}

impl TestClock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            now: AtomicI64::new(1_000_000),
            next: AtomicU64::new(1),
            timers: Mutex::new(HashMap::new()),
            dispatch: Mutex::new(None),
        })
    }

    fn bind(&self, scheduler: Arc<Scheduler>) {
        *self.dispatch.lock().unwrap() = Some(scheduler);
    }

    fn advance(&self, ms: i64) {
        self.now.fetch_add(ms, Ordering::Relaxed);
        let now = self.now.load(Ordering::Relaxed);
        loop {
            let due: Vec<(u64, Timer)> = {
                let timers = self.timers.lock().unwrap();
                let mut due: Vec<(u64, (i64, Timer))> = timers
                    .iter()
                    .filter(|(_, (at, _))| *at <= now)
                    .map(|(handle, timer)| (*handle, *timer))
                    .collect();
                due.sort_by_key(|(handle, _)| *handle);
                due.into_iter()
                    .map(|(handle, (_, action))| (handle, action))
                    .collect()
            };
            if due.is_empty() {
                return;
            }
            for (handle, action) in due {
                self.timers.lock().unwrap().remove(&handle);
                if let Some(scheduler) = self.dispatch.lock().unwrap().as_ref() {
                    scheduler.timer_fired(action);
                }
            }
        }
    }
}

impl Clock for TestClock {
    fn now(&self) -> i64 {
        self.now.load(Ordering::Relaxed)
    }

    fn set_timeout(&self, action: Timer, ms: i64) -> u64 {
        let handle = self.next.fetch_add(1, Ordering::Relaxed);
        let at = self.now.load(Ordering::Relaxed) + ms;
        self.timers.lock().unwrap().insert(handle, (at, action));
        handle
    }

    fn clear_timeout(&self, handle: u64) {
        self.timers.lock().unwrap().remove(&handle);
    }
}

/// How long a prompt may wait, as the clock counts.
fn wait_ms() -> i64 {
    i64::try_from(limits().max_queue_wait_ms).expect("a configured wait fits an i64")
}

/// A submitted prompt whose progress is recorded in the words a test reads.
fn entry(session_id: &str, seen: &Arc<Mutex<Vec<String>>>) -> QueueEntry {
    let admitted = session_id.to_owned();
    let expired = session_id.to_owned();
    let moved = session_id.to_owned();
    let admitted_seen = Arc::clone(seen);
    let expired_seen = Arc::clone(seen);
    let position_seen = Arc::clone(seen);
    QueueEntry {
        session_id: session_id.to_owned(),
        on_admitted: Box::new(move |_ticket: Ticket| {
            admitted_seen
                .lock()
                .unwrap()
                .push(format!("admitted:{admitted}"));
        }),
        on_expired: Box::new(move || {
            expired_seen
                .lock()
                .unwrap()
                .push(format!("expired:{expired}"));
        }),
        on_position_changed: Some(Box::new(move |position: usize| {
            position_seen
                .lock()
                .unwrap()
                .push(format!("position:{moved}:{position}"));
        })),
    }
}

fn status_of(outcome: &SubmitOutcome) -> &'static str {
    match outcome {
        SubmitOutcome::Admitted { .. } => "admitted",
        SubmitOutcome::Queued { .. } => "queued",
        SubmitOutcome::Rejected { .. } => "rejected",
    }
}

#[test]
fn admits_up_to_the_cap_and_queues_the_rest() {
    let clock = TestClock::new();
    let scheduler = Scheduler::start(limits(), clock, 5_000, 300_000, 750);
    let seen = Arc::new(Mutex::new(Vec::new()));

    let first = scheduler.submit(entry("a", &seen));
    let second = scheduler.submit(entry("b", &seen));
    assert_eq!(status_of(&first), "admitted");
    assert_eq!(status_of(&second), "admitted");

    let third = scheduler.submit(entry("c", &seen));
    match &third {
        SubmitOutcome::Queued { position } => assert_eq!(*position, 1),
        other => panic!("expected queued, got {other:?}"),
    }
    assert_eq!(scheduler.turns_in_flight(), 2);
}

#[test]
fn a_full_queue_refuses_rather_than_growing_without_bound() {
    let clock = TestClock::new();
    let scheduler = Scheduler::start(limits(), clock, 5_000, 300_000, 750);
    let seen = Arc::new(Mutex::new(Vec::new()));
    for index in 0..2 + limits().max_queue_length {
        scheduler.submit(entry(&format!("s{index}"), &seen));
    }

    let refused = scheduler.submit(entry("over", &seen));
    match &refused {
        SubmitOutcome::Rejected { reason } => assert!(reason.contains("queue is full")),
        other => panic!("expected rejected, got {other:?}"),
    }
}

#[test]
fn releasing_a_slot_admits_the_next_in_line() {
    let clock = TestClock::new();
    let scheduler = Scheduler::start(limits(), clock, 5_000, 300_000, 750);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let first = scheduler.submit(entry("a", &seen));
    scheduler.submit(entry("b", &seen));
    scheduler.submit(entry("c", &seen));

    let SubmitOutcome::Admitted { ticket } = first else {
        panic!("expected the first to be admitted");
    };
    assert!(scheduler.release(&ticket));

    let taken = seen.lock().unwrap();
    assert!(taken.contains(&"admitted:c".to_owned()));
    assert_eq!(scheduler.queue_length(), 0);
}

/// A slot released twice would let the cap drift upward without bound.
#[test]
fn a_ticket_released_twice_is_refused_the_second_time() {
    let clock = TestClock::new();
    let scheduler = Scheduler::start(limits(), clock, 5_000, 300_000, 750);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let outcome = scheduler.submit(entry("a", &seen));
    let SubmitOutcome::Admitted { ticket } = outcome else {
        panic!("expected admission");
    };

    assert!(scheduler.release(&ticket));
    assert!(!scheduler.release(&ticket));
    assert_eq!(scheduler.turns_in_flight(), 0);
}

#[test]
fn a_session_that_ends_takes_its_queued_prompts_with_it() {
    let clock = TestClock::new();
    let scheduler = Scheduler::start(limits(), clock, 5_000, 300_000, 750);
    let seen = Arc::new(Mutex::new(Vec::new()));
    scheduler.submit(entry("a", &seen));
    scheduler.submit(entry("b", &seen));
    scheduler.submit(entry("gone", &seen));
    scheduler.submit(entry("stays", &seen));

    assert_eq!(scheduler.cancel_session("gone"), 1);
    assert_eq!(scheduler.queue_length(), 1);
    assert!(
        seen.lock()
            .unwrap()
            .contains(&"position:stays:1".to_owned())
    );
}

#[test]
fn a_prompt_that_waited_too_long_expires_instead_of_being_sent() {
    let clock = TestClock::new();
    let scheduler = Scheduler::start(limits(), clock.clone(), 5_000, 300_000, 750);
    clock.bind(Arc::clone(&scheduler));
    let seen = Arc::new(Mutex::new(Vec::new()));
    scheduler.submit(entry("a", &seen));
    scheduler.submit(entry("b", &seen));
    scheduler.submit(entry("late", &seen));

    clock.advance(wait_ms() + 1);

    assert!(seen.lock().unwrap().contains(&"expired:late".to_owned()));
    assert_eq!(scheduler.queue_length(), 0);
}

/// The limit belongs to the account, so one session hitting it is information
/// about all of them.
#[test]
fn rate_limiting_pauses_admission_for_every_session_then_lifts() {
    let clock = TestClock::new();
    let scheduler = Scheduler::start(limits(), clock.clone(), 5_000, 300_000, 750);
    clock.bind(Arc::clone(&scheduler));
    let seen = Arc::new(Mutex::new(Vec::new()));

    scheduler.note_rate_limit();
    assert_eq!(scheduler.paused_because(), Some("provider backoff"));

    let held = scheduler.submit(entry("a", &seen));
    assert_eq!(status_of(&held), "queued");

    clock.advance(5_001);
    assert_eq!(scheduler.paused_because(), None);
    assert!(seen.lock().unwrap().contains(&"admitted:a".to_owned()));
}

#[test]
fn a_second_rate_limit_backs_off_further_and_success_decays_it() {
    let clock = TestClock::new();
    let scheduler = Scheduler::start(limits(), clock.clone(), 1_000, 60_000, 750);
    clock.bind(Arc::clone(&scheduler));

    scheduler.note_rate_limit();
    scheduler.note_rate_limit();
    assert_eq!(scheduler.backoff_remaining_ms(), 2_000);

    clock.advance(2_001);
    scheduler.note_success();
    scheduler.note_rate_limit();
    assert_eq!(scheduler.backoff_remaining_ms(), 1_000);
}

#[test]
fn live_sessions_are_capped_and_starts_are_spaced_apart() {
    let clock = TestClock::new();
    let scheduler = Scheduler::start(limits(), clock, 5_000, 300_000, 750);

    assert_eq!(scheduler.reserve_session(), Some(0));
    assert_eq!(scheduler.reserve_session(), Some(750));
    assert_eq!(scheduler.reserve_session(), Some(1_500));
    assert_eq!(scheduler.reserve_session(), None);

    scheduler.release_session();
    assert_eq!(scheduler.sessions(), 2);
    assert!(scheduler.reserve_session().is_some());
}

#[test]
fn shutdown_stops_the_timers_so_the_daemon_can_exit() {
    let clock = TestClock::new();
    let scheduler = Scheduler::start(limits(), clock.clone(), 5_000, 300_000, 750);
    clock.bind(Arc::clone(&scheduler));
    let seen = Arc::new(Mutex::new(Vec::new()));
    scheduler.submit(entry("a", &seen));
    scheduler.submit(entry("b", &seen));
    scheduler.submit(entry("c", &seen));

    scheduler.shutdown();
    clock.advance(wait_ms() * 10);

    assert_eq!(scheduler.queue_length(), 0);
    assert!(!seen.lock().unwrap().contains(&"expired:c".to_owned()));
}

#[test]
fn a_slot_can_be_taken_without_queueing_or_refused_outright() {
    let clock = TestClock::new();
    let scheduler = Scheduler::start(limits(), clock, 5_000, 300_000, 750);

    assert!(scheduler.try_admit("a").is_some());
    let second = scheduler.try_admit("a");
    assert!(second.is_some());
    assert!(scheduler.try_admit("a").is_none(), "the cap is reached");

    if let Some(ticket) = second {
        scheduler.release(&ticket);
    }
    assert!(scheduler.try_admit("a").is_some());
}

#[test]
fn no_slot_is_given_out_while_the_provider_is_being_backed_off() {
    let clock = TestClock::new();
    let scheduler = Scheduler::start(limits(), clock.clone(), 5_000, 300_000, 750);
    clock.bind(Arc::clone(&scheduler));

    scheduler.note_rate_limit();
    assert!(scheduler.try_admit("a").is_none());

    clock.advance(5_001);
    assert!(scheduler.try_admit("a").is_some());
}
