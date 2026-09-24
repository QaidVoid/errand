//! Sends each thread to the surface it lives on.
//!
//! A session's thread is a chat thread or an issue, and which is read off its
//! name, so the manager keeps asking one factory and never needs to know that
//! there are two.

use std::sync::Arc;

use super::thread::is_github_thread;
use super::view::IssueThreads;
use crate::session::manager::{FoundView, MadeThread, ThreadFactory};
use crate::session::session::IncomingMessage;

/// The chat threads and the issues, behind the one factory the manager asks.
pub struct ByThread {
    /// Where every thread that is not an issue lives.
    pub chat: Arc<dyn ThreadFactory>,
    /// Where the issues and pull requests live.
    pub issues: Arc<IssueThreads>,
}

impl ThreadFactory for ByThread {
    fn create(self: Arc<Self>, message: IncomingMessage, name: String) -> MadeThread {
        if is_github_thread(&message.channel_id) {
            Arc::clone(&self.issues).create(message, name)
        } else {
            Arc::clone(&self.chat).create(message, name)
        }
    }

    /// Opened from the interface, which only ever opens a chat thread.
    fn open(self: Arc<Self>, name: String, opener: String) -> MadeThread {
        Arc::clone(&self.chat).open(name, opener)
    }

    fn port_for(self: Arc<Self>, thread_id: String) -> FoundView {
        if is_github_thread(&thread_id) {
            Arc::clone(&self.issues).port_for(thread_id)
        } else {
            Arc::clone(&self.chat).port_for(thread_id)
        }
    }

    fn release(&self, thread_id: &str) {
        if !is_github_thread(thread_id) {
            self.chat.release(thread_id);
        }
    }
}
