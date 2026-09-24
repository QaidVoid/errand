//! What an issue is told of the session answering it.
//!
//! The thread shows a turn as it happens, a message at a time. An issue is
//! read by people watching a tracker, who are notified of every comment, so it
//! is told only what the turn came to: the agent's last message, or why the
//! turn failed when it said nothing. The working, the commands, and the
//! numbers stay in the thread.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::thread::issue_of;
use crate::log::{LogValue, Logger, fields};
use crate::session::event::{NoticeLevel, SessionEvent};
use crate::session::pr::{Api, ApiCall};
use crate::session::views::{SessionView, ViewError};

/// The most a comment carries. GitHub refuses one past 65,536 characters.
const COMMENT_LIMIT: usize = 60_000;

/// Makes the views that answer issues and pull requests.
pub struct IssueThreads {
    /// Calls the GitHub API.
    pub api: Api,
    /// The token it is called with.
    pub token: String,
    /// Where a comment that could not be posted is reported.
    pub log: Logger,
}

impl IssueThreads {
    /// The view that answers the issue a thread name names.
    pub fn view_for(&self, thread_id: &str) -> Option<Arc<IssueView>> {
        let (repository, number) = issue_of(thread_id)?;
        Some(Arc::new(IssueView {
            api: Arc::clone(&self.api),
            token: self.token.clone(),
            repository: repository.to_owned(),
            number,
            turn: Mutex::new(Turn::default()),
            log: self.log.clone(),
        }))
    }
}

/// One issue, written to a turn at a time.
pub struct IssueView {
    api: Api,
    token: String,
    repository: String,
    number: u64,
    /// What the turn came to so far, posted when it ends.
    turn: Mutex<Turn>,
    log: Logger,
}

/// What one turn came to, as far as the issue is concerned.
#[derive(Default)]
struct Turn {
    /// The agent's latest message, which is its answer once the turn ends.
    answer: Option<String>,
    /// Why the turn failed, for a turn that said nothing.
    failure: Option<String>,
}

impl IssueView {
    async fn flush(&self) -> Result<(), ViewError> {
        let turn = std::mem::take(&mut *self.turn.lock().expect("the issue turn lock"));
        match turn.answer.or(turn.failure) {
            Some(said) => self.post(&said).await,
            None => Ok(()),
        }
    }

    async fn post(&self, text: &str) -> Result<(), ViewError> {
        let mut body: String = for_github(text).chars().take(COMMENT_LIMIT).collect();
        if body.chars().count() == COMMENT_LIMIT {
            body.push_str("\n\n(cut short; the rest is in the session's transcript)");
        }
        let answer = (self.api)(
            format!("/repos/{}/issues/{}/comments", self.repository, self.number),
            ApiCall {
                method: "POST".to_owned(),
                token: self.token.clone(),
                body: Some(json!({ "body": body })),
            },
        )
        .await;
        if answer.status == 201 {
            return Ok(());
        }
        let why = format!(
            "GitHub answered {} to a comment on {}#{}: {}",
            answer.status,
            self.repository,
            self.number,
            answer.body["message"].as_str().unwrap_or("no reason given")
        );
        self.log.warn(
            "a turn could not be posted to its issue",
            &fields([("detail", LogValue::from(why.as_str()))]),
        );
        Err(why.into())
    }
}

impl SessionView for IssueView {
    fn observe<'a>(
        &'a self,
        event: &'a SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), ViewError>> + Send + 'a>> {
        Box::pin(async move {
            match event {
                SessionEvent::Post { text } => {
                    self.turn.lock().expect("the issue turn lock").answer = Some(text.clone());
                }
                SessionEvent::Notice {
                    text,
                    level: NoticeLevel::Warning,
                } => {
                    self.turn.lock().expect("the issue turn lock").failure = Some(text.clone());
                }
                SessionEvent::Busy { busy: false } | SessionEvent::Close { .. } => {
                    return self.flush().await;
                }
                _ => {}
            }
            Ok(())
        })
    }
}

/// Says in GitHub's terms what was written for the chat service.
///
/// A chat mention of a GitHub account, `<@github:login>`, becomes `@login`,
/// and a chat timestamp, `<t:seconds:R>`, becomes the moment in UTC, since
/// GitHub renders neither.
pub fn for_github(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail.find('>') else {
            out.push_str(tail);
            return out;
        };
        let inside = &tail[1..end];
        match translated(inside) {
            Some(said) => out.push_str(&said),
            None => out.push_str(&tail[..=end]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

/// One chat markup token in GitHub's terms, or nothing to leave it alone.
fn translated(inside: &str) -> Option<String> {
    if let Some(login) = inside.strip_prefix("@github:") {
        return Some(format!("@{login}"));
    }
    let seconds = inside
        .strip_prefix("t:")?
        .split(':')
        .next()?
        .parse::<i64>()
        .ok()?;
    let at = jiff::Timestamp::from_second(seconds).ok()?;
    Some(at.strftime("%Y-%m-%d %H:%M UTC").to_string())
}

#[cfg(test)]
mod tests;
