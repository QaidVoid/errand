//! Opens a chat thread for work asked for on GitHub, and keeps the two linked.
//!
//! A session asked for on an issue runs in a chat thread like any other, so
//! it is watched and steered where every other session is. The issue is told
//! each turn's answer beside it, and a comment there reaches the session in
//! the thread. Which thread answers which issue is written down, so a restart
//! finds both again.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use super::thread::{is_github_thread, issue_of};
use super::view::IssueThreads;
use crate::log::{LogValue, Logger, fields};
use crate::session::event::SessionEvent;
use crate::session::manager::{CreatedThread, FoundView, MadeThread, ThreadFactory};
use crate::session::session::IncomingMessage;
use crate::session::views::{SessionView, ViewError};

/// Where the links between issues and threads are kept, in the state
/// directory.
pub const LINKS_FILENAME: &str = "issue-threads.json";

/// The chat threads, with the issues some of them answer.
pub struct ByThread {
    chat: Arc<dyn ThreadFactory>,
    issues: Arc<IssueThreads>,
    /// The chat thread answering each issue, by the issue's thread name.
    links: Mutex<BTreeMap<String, String>>,
    path: PathBuf,
    log: Logger,
}

impl ByThread {
    /// A router over the chat threads, reading the links written last run.
    pub fn new(
        chat: Arc<dyn ThreadFactory>,
        issues: Arc<IssueThreads>,
        path: PathBuf,
        log: Logger,
    ) -> Self {
        let links = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            chat,
            issues,
            links: Mutex::new(links),
            path,
            log,
        }
    }

    /// The chat thread that answers an issue, when one does.
    pub fn thread_for(&self, issue: &str) -> Option<String> {
        self.links
            .lock()
            .expect("the issue links lock")
            .get(issue)
            .cloned()
    }

    fn issue_for(&self, thread_id: &str) -> Option<String> {
        self.links
            .lock()
            .expect("the issue links lock")
            .iter()
            .find(|(_, thread)| *thread == thread_id)
            .map(|(issue, _)| issue.clone())
    }

    /// Writes down that a thread answers an issue, replacing whichever did.
    fn link(&self, issue: &str, thread_id: &str) {
        let written = {
            let mut links = self.links.lock().expect("the issue links lock");
            links.insert(issue.to_owned(), thread_id.to_owned());
            serde_json::to_string_pretty(&*links).unwrap_or_default()
        };
        if let Err(error) = std::fs::write(&self.path, written) {
            self.log.warn(
                "the link between an issue and its thread could not be written, so a restart \
                 will not find it",
                &fields([
                    ("issue", LogValue::from(issue)),
                    ("detail", LogValue::from(error.to_string())),
                ]),
            );
        }
    }

    /// The thread's own view, told the issue's turns too when it answers one.
    fn answering(&self, chat: Arc<dyn SessionView>, issue: Option<&str>) -> Arc<dyn SessionView> {
        match issue.and_then(|issue| self.issues.view_for(issue)) {
            Some(issue) => Arc::new(Both { chat, issue }),
            None => chat,
        }
    }
}

impl ThreadFactory for ByThread {
    /// A message said on an issue opens a chat thread of its own, since it
    /// has no chat message to hang one off, and the thread answers the issue.
    fn create(self: Arc<Self>, message: IncomingMessage, name: String) -> MadeThread {
        if !is_github_thread(&message.channel_id) {
            return Arc::clone(&self.chat).create(message, name);
        }
        Box::pin(async move {
            let issue = message.channel_id.clone();
            let (repository, number) =
                issue_of(&issue).ok_or_else(|| format!("{issue} is not an issue"))?;
            let opener = format!(
                "{} asked on GitHub, on {repository}#{number}: https://github.com/{repository}/issues/{number}",
                message.author_name.as_deref().unwrap_or("somebody"),
            );
            let made = Arc::clone(&self.chat).open(name, opener).await?;
            self.link(&issue, &made.id);
            Ok(CreatedThread {
                view: self.answering(made.view, Some(&issue)),
                id: made.id,
            })
        })
    }

    fn open(self: Arc<Self>, name: String, opener: String) -> MadeThread {
        Arc::clone(&self.chat).open(name, opener)
    }

    fn port_for(self: Arc<Self>, thread_id: String) -> FoundView {
        Box::pin(async move {
            let chat = Arc::clone(&self.chat).port_for(thread_id.clone()).await?;
            Some(self.answering(chat, self.issue_for(&thread_id).as_deref()))
        })
    }

    fn release(&self, thread_id: &str) {
        self.chat.release(thread_id);
    }
}

/// A chat thread and the issue it answers, told the same things.
///
/// The thread is the one that counts. The issue says in the log when a
/// comment could not be posted there, and that does not stop the thread.
struct Both {
    chat: Arc<dyn SessionView>,
    issue: Arc<dyn SessionView>,
}

impl SessionView for Both {
    fn observe<'a>(
        &'a self,
        event: &'a SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), ViewError>> + Send + 'a>> {
        Box::pin(async move {
            let (chat, _) =
                futures_util::join!(self.chat.observe(event), self.issue.observe(event));
            chat
        })
    }
}

#[cfg(test)]
mod tests;
