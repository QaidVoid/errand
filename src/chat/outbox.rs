//! Per-thread ordered work queue that survives a disconnect.
//!
//! Acting straight from an event handler loses ordering the moment two
//! actions overlap, and loses output entirely across a reconnect. Everything
//! a session does to its thread goes through one of these instead: a single
//! chain per thread, buffered while the connection is down, and bounded so a
//! disconnected daemon cannot grow without limit.
//!
//! The queue holds tasks rather than strings because a thread both sends new
//! messages and edits the one it is building up, and those must stay in order
//! with respect to each other.
//!
//! Rate limiting is left to the chat library, which already queues and
//! respects the retry interval. This exists for ordering, for surviving a
//! disconnect, and for bounding memory.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio::sync::{Mutex as AsyncMutex, Notify, watch};

use crate::log::{LogValue, Logger, fields};

/// Why a task gave up, in words worth a log line.
pub type TaskError = Box<dyn std::error::Error + Send + Sync>;

/// One unit of work against a thread. A task that fails is kept so it can be
/// run again in place, so the closure is one that can be called many times.
pub type OutboxTask =
    Box<dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), TaskError>> + Send>> + Send>;

/// How many tasks a disconnected outbox holds before it starts dropping.
pub const DEFAULT_MAX_BUFFERED: usize = 500;

/// Announces a gap to the thread, so a truncated conversation is never passed
/// off as a complete one.
pub type AnnounceDrops =
    Arc<dyn Fn(usize) -> Pin<Box<dyn Future<Output = Result<(), TaskError>> + Send>> + Send + Sync>;

struct Inner {
    buffer: VecDeque<OutboxTask>,
    connected: bool,
    dropped: usize,
    announced: usize,
    closed: bool,
}

/// An ordered, buffered work queue for one thread.
pub struct Outbox {
    state: Arc<Mutex<Inner>>,
    /// Woken when the outbox closes, so a follower stops watching with it.
    closed_signal: Arc<Notify>,
    // Held while a drain runs, so a flush waits for one already in flight
    // rather than starting a second.
    draining: Arc<AsyncMutex<()>>,
    log: Logger,
    announce_drops: AnnounceDrops,
    max_buffered: usize,
}

impl Clone for Outbox {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
            closed_signal: Arc::clone(&self.closed_signal),
            draining: Arc::clone(&self.draining),
            log: self.log.clone(),
            announce_drops: Arc::clone(&self.announce_drops),
            max_buffered: self.max_buffered,
        }
    }
}

impl Outbox {
    /// An outbox for one thread.
    pub fn new(log: Logger, announce_drops: AnnounceDrops, max_buffered: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(Inner {
                buffer: VecDeque::new(),
                connected: true,
                dropped: 0,
                announced: 0,
                closed: false,
            })),
            closed_signal: Arc::new(Notify::new()),
            draining: Arc::new(AsyncMutex::new(())),
            log,
            announce_drops,
            max_buffered,
        }
    }

    /// Tasks waiting to run.
    #[allow(
        dead_code,
        reason = "read by this module's tests, which assert on state the daemon never asks for"
    )]
    pub fn pending(&self) -> usize {
        self.state.lock().expect("the outbox lock").buffer.len()
    }

    /// True once closed, after which nothing more will ever be sent.
    pub fn is_closed(&self) -> bool {
        self.state.lock().expect("the outbox lock").closed
    }

    /// Tasks discarded because the buffer was full.
    #[allow(
        dead_code,
        reason = "read by this module's tests, which assert on state the daemon never asks for"
    )]
    pub fn dropped_count(&self) -> usize {
        self.state.lock().expect("the outbox lock").dropped
    }

    /// Queues a task. Order is preserved against every other queued task.
    pub fn enqueue(&self, task: OutboxTask) {
        let mut state = self.state.lock().expect("the outbox lock");
        if state.closed {
            return;
        }

        // Drop oldest: the newest output is the part a reader still cares
        // about. The count is kept outside the buffer so announcing a gap can
        // never itself push the buffer over its bound.
        while state.buffer.len() >= self.max_buffered {
            state.buffer.pop_front();
            state.dropped += 1;
        }

        state.buffer.push_back(task);
        drop(state);
        self.drain_soon();
    }

    /// Marks the gateway up or down. While down, tasks buffer instead of
    /// being attempted, and the order they were queued in is preserved for
    /// the flush.
    pub fn set_connected(&self, connected: bool) {
        let was_connected = {
            let mut state = self.state.lock().expect("the outbox lock");
            std::mem::replace(&mut state.connected, connected)
        };
        if connected && !was_connected {
            self.drain_soon();
        }
    }

    /// Follows a connection, buffering while it is down and draining when it
    /// comes back.
    ///
    /// A failed post turns this outbox off on its own, and only a connection
    /// coming back turns it on again, so an outbox that follows nothing stops
    /// posting for good after one failure. The watch is the fan-out: no
    /// register of live threads has to be kept, and the follower ends with
    /// the outbox rather than outliving it.
    pub fn follow(&self, mut connection: watch::Receiver<bool>) {
        let this = self.clone();
        let closed = Arc::clone(&self.closed_signal);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = closed.notified() => return,
                    changed = connection.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                }
                if this.is_closed() {
                    return;
                }
                let connected = *connection.borrow();
                this.set_connected(connected);
            }
        });
    }

    /// Waits for everything queued so far to run.
    ///
    /// Waits for a drain already in progress rather than starting over it, so
    /// a caller cannot observe an empty-looking outbox while a drain is
    /// mid-flight.
    pub async fn flush(&self) {
        let _guard = self.draining.lock().await;
        self.drain().await;
    }

    /// Stops accepting work and discards anything still queued.
    pub fn close(&self) {
        {
            let mut state = self.state.lock().expect("the outbox lock");
            state.closed = true;
            state.buffer.clear();
        }
        self.closed_signal.notify_waiters();
    }

    /// Starts a drain in the background, if one is not already running.
    fn drain_soon(&self) {
        if !self.state.lock().expect("the outbox lock").connected {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let _guard = this.draining.lock().await;
            this.drain().await;
        });
    }

    /// Runs queued tasks until the buffer empties, the connection drops, or
    /// the queue is closed.
    ///
    /// The connection is turned off inside the loop as a failed announcement
    /// or task reports it; a later `set_connected(true)` picks the queue back
    /// up with its order intact.
    async fn drain(&self) {
        loop {
            let gap = {
                let state = self.state.lock().expect("the outbox lock");
                if state.closed || !state.connected {
                    return;
                }
                if state.dropped > state.announced {
                    Some(state.dropped - state.announced)
                } else {
                    None
                }
            };

            if let Some(gap) = gap {
                match (self.announce_drops)(gap).await {
                    Ok(()) => {
                        let mut state = self.state.lock().expect("the outbox lock");
                        state.announced = state.dropped;
                    }
                    Err(error) => {
                        self.log.warn(
                            "reporting dropped messages failed",
                            &fields([("detail", LogValue::from(error.to_string()))]),
                        );
                        self.state.lock().expect("the outbox lock").connected = false;
                    }
                }
                continue;
            }

            let head = self
                .state
                .lock()
                .expect("the outbox lock")
                .buffer
                .pop_front();
            let Some(task) = head else { return };
            if task().await.is_ok() {
                continue;
            }
            // Left at the head so ordering holds when the gateway returns.
            self.log.warn("a thread action failed", &fields([]));
            {
                let mut state = self.state.lock().expect("the outbox lock");
                state.connected = false;
                state.buffer.push_front(task);
            }
            return;
        }
    }
}

#[cfg(test)]
#[path = "outbox/tests.rs"]
mod tests;
