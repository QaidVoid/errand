//! Bounds concurrent turns and live sessions, and absorbs provider backoff.
//!
//! Two independent caps, because the costs are independent. A session with a
//! turn in flight costs provider load; a session that merely exists costs
//! memory. One number would force a bad trade in both directions.
//!
//! A slot covers a whole turn rather than an individual model request, because
//! from outside the agent a request inside a tool loop is not visible. A turn
//! is the coarsest unit that can be observed and the finest that can be
//! controlled, so it is the unit.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::schema::LimitsConfig;

/// What a fired timer asks the scheduler to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timer {
    /// The provider backoff has run out; try the queue again.
    Backoff,
    /// Sweep the queue for prompts that waited too long.
    Sweep,
}

/// Where time comes from. Injected so tests drive it rather than wait for it.
pub trait Clock: Send + Sync + 'static {
    /// Milliseconds since the epoch.
    fn now(&self) -> i64;
    /// Asks to be told when `ms` have passed.
    fn set_timeout(&self, action: Timer, ms: i64) -> u64;
    /// Cancels a timer that has not fired.
    fn clear_timeout(&self, handle: u64);
}

/// The clock the daemon runs on, whose timers run on the async runtime.
#[derive(Clone)]
pub struct SystemClock {
    on_fire: Arc<dyn Fn(Timer) + Send + Sync>,
}

impl SystemClock {
    /// A clock whose fired timers hand the action to `on_fire`, which is where
    /// the daemon wires the scheduler back in.
    pub fn new(on_fire: impl Fn(Timer) + Send + Sync + 'static) -> Self {
        Self {
            on_fire: Arc::new(on_fire),
        }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| {
                #[allow(clippy::cast_possible_truncation)]
                let millis = since.as_millis() as i64;
                millis
            })
    }

    fn set_timeout(&self, action: Timer, ms: i64) -> u64 {
        let on_fire = Arc::clone(&self.on_fire);
        tokio::spawn(async move {
            #[allow(clippy::cast_sign_loss)]
            let ms = ms.max(0) as u64;
            tokio::time::sleep(Duration::from_millis(ms)).await;
            on_fire(action);
        });
        0
    }

    fn clear_timeout(&self, _handle: u64) {
        // A spawned sleep cannot be cancelled through this trait, and does not
        // need to be: a fired timer asks, and an empty scheduler answers by
        // doing nothing.
    }
}

/// Held for the duration of one turn, and released exactly once.
#[derive(Debug)]
pub struct Ticket {
    /// The session the turn belongs to.
    pub session_id: String,
    released_at: Mutex<Option<i64>>,
}

impl Ticket {
    /// True once this ticket has been released.
    pub fn is_released(&self) -> bool {
        self.released_at.lock().expect("a ticket lock").is_some()
    }

    /// Marks the ticket spent. Returns false on a second release.
    fn mark_released(&self, at: i64) -> bool {
        let mut released = self.released_at.lock().expect("a ticket lock");
        if released.is_some() {
            return false;
        }
        *released = Some(at);
        true
    }
}

/// How a submitted prompt was disposed of.
#[derive(Debug)]
pub enum SubmitOutcome {
    /// A slot was free and is now held.
    Admitted { ticket: Ticket },
    /// The prompt waits, at the given position.
    Queued { position: usize },
    /// The queue was full, with words worth posting back.
    Rejected { reason: String },
}

/// A prompt waiting for a turn slot.
pub struct QueueEntry {
    /// The session the prompt belongs to.
    pub session_id: String,
    /// Called when a slot frees and the prompt should be sent.
    pub on_admitted: Box<dyn FnOnce(Ticket) + Send>,
    /// Called when the prompt waited longer than the configured maximum.
    pub on_expired: Box<dyn FnOnce() + Send>,
    /// Called when the prompt's place in the queue changes.
    pub on_position_changed: Option<Box<dyn Fn(usize) + Send>>,
}

struct PendingEntry {
    entry: QueueEntry,
    enqueued_at: i64,
    last_reported_position: usize,
}

struct SchedulerState {
    in_flight: usize,
    live_sessions: usize,
    queue: Vec<PendingEntry>,
    backoff_until: i64,
    backoff_ms: i64,
    backoff_timer: Option<u64>,
    last_start_at: i64,
    sweep_timer: Option<u64>,
}

/// Why the scheduler is currently refusing to admit work.
pub const PAUSED_REASON: &str = "provider backoff";

/// Bounds turns in flight, live sessions, and the queue between them.
pub struct Scheduler {
    limits: LimitsConfig,
    clock: Arc<dyn Clock>,
    base_backoff_ms: i64,
    max_backoff_ms: i64,
    start_interval_ms: i64,
    state: Mutex<SchedulerState>,
}

impl Scheduler {
    /// A scheduler over `limits`, with its timers on `clock`.
    ///
    /// Returns it behind an arc, because the timers it schedules call back
    /// into it when they fire.
    pub fn start(
        limits: LimitsConfig,
        clock: Arc<dyn Clock>,
        base_backoff_ms: i64,
        max_backoff_ms: i64,
        start_interval_ms: i64,
    ) -> Arc<Self> {
        Arc::new(Self {
            limits,
            clock,
            base_backoff_ms,
            max_backoff_ms,
            start_interval_ms,
            state: Mutex::new(SchedulerState {
                in_flight: 0,
                live_sessions: 0,
                queue: Vec::new(),
                backoff_until: 0,
                backoff_ms: base_backoff_ms,
                backoff_timer: None,
                last_start_at: 0,
                sweep_timer: None,
            }),
        })
    }

    /// Sessions currently holding a slot for a running turn.
    pub fn turns_in_flight(&self) -> usize {
        self.state.lock().expect("the scheduler lock").in_flight
    }

    /// Prompts currently waiting for a slot.
    pub fn queue_length(&self) -> usize {
        self.state.lock().expect("the scheduler lock").queue.len()
    }

    /// Sessions that exist, running a turn or not.
    pub fn sessions(&self) -> usize {
        self.state.lock().expect("the scheduler lock").live_sessions
    }

    /// Set while the scheduler is refusing to admit work, with the reason.
    pub fn paused_because(&self) -> Option<&'static str> {
        let state = self.state.lock().expect("the scheduler lock");
        (self.clock.now() < state.backoff_until).then_some(PAUSED_REASON)
    }

    /// How long the current backoff will last, for reporting.
    pub fn backoff_remaining_ms(&self) -> i64 {
        let state = self.state.lock().expect("the scheduler lock");
        (state.backoff_until - self.clock.now()).max(0)
    }

    /// Reserves capacity for a new session, returning how long to wait before
    /// starting, or nothing when the cap is reached.
    pub fn reserve_session(&self) -> Option<i64> {
        let mut state = self.state.lock().expect("the scheduler lock");
        if state.live_sessions >= self.limits.max_live_sessions as usize {
            return None;
        }
        state.live_sessions += 1;

        let now = self.clock.now();
        let earliest = state.last_start_at + self.start_interval_ms;
        let delay = (earliest - now).max(0);
        state.last_start_at = now.max(earliest);
        Some(delay)
    }

    /// Releases a session's reservation when it ends.
    pub fn release_session(&self) {
        let mut state = self.state.lock().expect("the scheduler lock");
        if state.live_sessions > 0 {
            state.live_sessions -= 1;
        }
    }

    /// Why a session was refused, in words worth posting back to the channel.
    pub fn session_refused_reason(&self) -> String {
        format!(
            "the session limit of {} is reached, so this message did not start one",
            self.limits.max_live_sessions
        )
    }

    /// Takes a slot if one is free now, and does not queue when none is.
    ///
    /// For work that is worth doing only immediately: a delegated subtask
    /// waiting behind a queue would stall the turn it was meant to make
    /// cheaper, so it is given up on instead.
    pub fn try_admit(self: &Arc<Self>, session_id: &str) -> Option<Ticket> {
        if !self.can_admit_now() {
            return None;
        }
        self.state.lock().expect("the scheduler lock").in_flight += 1;
        Some(Ticket {
            session_id: session_id.to_owned(),
            released_at: Mutex::new(None),
        })
    }

    /// Submits a prompt, admitting it now or queueing it behind the cap.
    pub fn submit(self: &Arc<Self>, entry: QueueEntry) -> SubmitOutcome {
        if let Some(ticket) = self.try_admit(&entry.session_id) {
            return SubmitOutcome::Admitted { ticket };
        }

        let mut state = self.state.lock().expect("the scheduler lock");
        if state.queue.len() >= self.limits.max_queue_length as usize {
            return SubmitOutcome::Rejected {
                reason: format!(
                    "the queue is full at {} waiting prompts, so this message was not accepted",
                    self.limits.max_queue_length
                ),
            };
        }

        let position = state.queue.len() + 1;
        state.queue.push(PendingEntry {
            entry,
            enqueued_at: self.clock.now(),
            last_reported_position: position,
        });
        drop(state);
        self.schedule_sweep();
        SubmitOutcome::Queued { position }
    }

    /// Releases a turn slot. Every exit path calls this: completion, abort,
    /// error, sandbox death, and session termination.
    ///
    /// Returns false when the ticket was already released, which is a bug in
    /// the caller rather than something to absorb silently.
    pub fn release(self: &Arc<Self>, ticket: &Ticket) -> bool {
        if !ticket.mark_released(self.clock.now()) {
            return false;
        }
        {
            let mut state = self.state.lock().expect("the scheduler lock");
            if state.in_flight > 0 {
                state.in_flight -= 1;
            }
        }
        self.pump();
        true
    }

    /// Drops a session's queued prompts when the session ends.
    pub fn cancel_session(&self, session_id: &str) -> usize {
        let mut state = self.state.lock().expect("the scheduler lock");
        let before = state.queue.len();
        state
            .queue
            .retain(|pending| pending.entry.session_id != session_id);
        let removed = before - state.queue.len();
        if removed > 0 {
            drop(state);
            self.report_positions();
        }
        removed
    }

    /// Records that the provider signalled rate limiting.
    ///
    /// Admission stops globally, not just for the session that noticed. The
    /// limit is per account, so a limit one session hit is information about
    /// all of them, and backing off only the one that noticed leaves the rest
    /// pushing into the same wall.
    pub fn note_rate_limit(self: &Arc<Self>) {
        let backoff_ms = {
            let mut state = self.state.lock().expect("the scheduler lock");
            let now = self.clock.now();
            if now < state.backoff_until {
                state.backoff_ms = (state.backoff_ms * 2).min(self.max_backoff_ms);
            }
            state.backoff_until = now + state.backoff_ms;
            state.backoff_ms
        };

        let previous = {
            let mut state = self.state.lock().expect("the scheduler lock");
            state
                .backoff_timer
                .replace(self.clock.set_timeout(Timer::Backoff, backoff_ms))
        };
        if let Some(handle) = previous {
            self.clock.clear_timeout(handle);
        }
    }

    /// Records a turn that completed without rate limiting, decaying the pause.
    pub fn note_success(&self) {
        let mut state = self.state.lock().expect("the scheduler lock");
        if self.clock.now() < state.backoff_until {
            return;
        }
        state.backoff_ms = self.base_backoff_ms.max(state.backoff_ms / 2);
    }

    /// Drops prompts that have waited longer than the configured maximum.
    pub fn expire_stale(&self) -> usize {
        let expired: Vec<PendingEntry>;
        {
            let mut state = self.state.lock().expect("the scheduler lock");
            #[allow(clippy::cast_possible_wrap)]
            let wait = self.limits.max_queue_wait_ms as i64;
            let cutoff = self.clock.now() - wait;
            let due: Vec<usize> = state
                .queue
                .iter()
                .enumerate()
                .filter(|(_, pending)| pending.enqueued_at <= cutoff)
                .map(|(index, _)| index)
                .collect();
            // Removed newest first, which is the order they are told about it.
            expired = due
                .into_iter()
                .rev()
                .map(|index| state.queue.remove(index))
                .collect();
            if expired.is_empty() {
                return 0;
            }
        }
        let count = expired.len();
        for pending in expired {
            (pending.entry.on_expired)();
        }
        self.report_positions();
        count
    }

    /// Stops every timer, so the daemon can exit.
    pub fn shutdown(&self) {
        let mut state = self.state.lock().expect("the scheduler lock");
        if let Some(handle) = state.backoff_timer.take() {
            self.clock.clear_timeout(handle);
        }
        if let Some(handle) = state.sweep_timer.take() {
            self.clock.clear_timeout(handle);
        }
        state.queue.clear();
    }

    /// The timer the backoff asked for has run out; the queue may go again.
    pub fn timer_fired(self: &Arc<Self>, action: Timer) {
        match action {
            Timer::Backoff => {
                {
                    let mut state = self.state.lock().expect("the scheduler lock");
                    state.backoff_timer = None;
                }
                self.pump();
            }
            Timer::Sweep => {
                {
                    let mut state = self.state.lock().expect("the scheduler lock");
                    state.sweep_timer = None;
                }
                self.expire_stale();
                let waiting = self.state.lock().expect("the scheduler lock").queue.len();
                if waiting > 0 {
                    self.schedule_sweep();
                }
            }
        }
    }

    fn can_admit_now(&self) -> bool {
        // One lock: the backoff check reads the same state the cap reads.
        let state = self.state.lock().expect("the scheduler lock");
        state.in_flight < self.limits.max_concurrent_turns as usize
            && self.clock.now() >= state.backoff_until
    }

    fn pump(self: &Arc<Self>) {
        loop {
            let next = {
                let mut state = self.state.lock().expect("the scheduler lock");
                let backoff_over = self.clock.now() >= state.backoff_until;
                if state.queue.is_empty()
                    || state.in_flight >= self.limits.max_concurrent_turns as usize
                    || !backoff_over
                {
                    break;
                }
                let next = state.queue.remove(0);
                state.in_flight += 1;
                next
            };
            (next.entry.on_admitted)(Ticket {
                session_id: next.entry.session_id,
                released_at: Mutex::new(None),
            });
        }
        self.report_positions();
    }

    /// Tells each waiting prompt its position, but only when it changed, so a
    /// thread gets one message updated rather than a message per shuffle.
    fn report_positions(&self) {
        let mut state = self.state.lock().expect("the scheduler lock");
        for (index, pending) in state.queue.iter_mut().enumerate() {
            let position = index + 1;
            if pending.last_reported_position == position {
                continue;
            }
            pending.last_reported_position = position;
            if let Some(report) = &mut pending.entry.on_position_changed {
                report(position);
            }
        }
    }

    fn schedule_sweep(self: &Arc<Self>) {
        let mut state = self.state.lock().expect("the scheduler lock");
        if state.sweep_timer.is_some() {
            return;
        }
        state.sweep_timer = Some(self.clock.set_timeout(Timer::Sweep, self.sweep_interval()));
    }

    fn sweep_interval(&self) -> i64 {
        {
            #[allow(clippy::cast_possible_wrap)]
            let interval = (self.limits.max_queue_wait_ms / 4) as i64;
            interval.max(1)
        }
    }
}
