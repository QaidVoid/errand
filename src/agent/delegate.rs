//! Running the delegations of one turn.
//!
//! Held per turn rather than per session, so the count of what a turn has
//! spent lives and dies with the turn and there is no table to clean up.
//!
//! Every refusal here is ordinary. A delegation that cannot run leaves the
//! work with the session's own model, which is slower and dearer and correct.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::admission::scheduler::Scheduler;
use crate::agent::delegation::{Outcome, Refused, Sources, parse_delegation, resolve_source};
use crate::provider::ask::{Endpoint, Question, Sender, ask};

/// An answer, and what it is an answer about.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// What the model said.
    pub text: String,
    /// The model that said it, so the answer can be attributed.
    pub model: String,
    /// What it was shown, so the answer can say what it describes.
    pub describes: String,
    /// Tokens the delegated model was charged, when the provider said.
    pub tokens: Option<i64>,
    /// Characters kept out of the session's context by asking instead of
    /// reading.
    pub kept_out: usize,
}

/// What one delegation answered or refused.
pub type DelegationOutcome = Outcome<Answer>;

/// What a turn's delegations need in order to run.
pub struct TurnDelegations<S: Sources, P: Sender> {
    endpoint: Endpoint,
    scheduler: Arc<Scheduler>,
    sources: Arc<S>,
    deadline_ms: u64,
    per_turn: usize,
    used: usize,
    send: Arc<P>,
}

impl<S: Sources + 'static, P: Sender> TurnDelegations<S, P> {
    /// The delegations of one turn.
    pub fn new(
        session_id: &str,
        endpoint: Endpoint,
        scheduler: Arc<Scheduler>,
        sources: Arc<S>,
        deadline_ms: u64,
        per_turn: usize,
        send: Arc<P>,
    ) -> Self {
        let _ = session_id;
        Self {
            endpoint,
            scheduler,
            sources,
            deadline_ms,
            per_turn,
            used: 0,
            send,
        }
    }

    /// How many delegations this turn has left.
    pub fn remaining(&self) -> usize {
        self.per_turn.saturating_sub(self.used)
    }

    /// Runs one delegation, or says why it did not.
    ///
    /// A refusal is returned rather than thrown: every caller continues either
    /// way, and an exception would invite one of them not to.
    pub async fn run(&mut self, raw: &Value) -> DelegationOutcome {
        if self.remaining() == 0 {
            return Outcome::Refused(Refused {
                refused: format!("this turn has already delegated {} times", self.per_turn),
            });
        }

        let delegation = match parse_delegation(raw) {
            Outcome::Ready(delegation) => delegation,
            Outcome::Refused(refused) => return Outcome::Refused(refused),
        };

        let resolved = match resolve_source(&delegation, self.sources.as_ref()).await {
            Outcome::Ready(resolved) => resolved,
            Outcome::Refused(refused) => return Outcome::Refused(refused),
        };

        // Counted once it is going to be sent, so a malformed request does not
        // spend the turn's allowance.
        self.used += 1;

        let Some(ticket) = self.scheduler.try_admit() else {
            return Outcome::Refused(Refused {
                refused: if self.scheduler.paused_because().is_none() {
                    "there was no free slot to ask a second model".to_owned()
                } else {
                    "the provider is being backed off, so nothing was asked of it".to_owned()
                },
            });
        };

        let asked = Question {
            question: resolved.question,
            describes: resolved.describes.clone(),
            content: resolved.content.clone(),
        };
        let cancel = tokio::time::sleep(Duration::from_millis(self.deadline_ms));
        tokio::pin!(cancel);
        let given = ask(&self.endpoint, &asked, cancel, self.send.as_ref()).await;
        self.scheduler.release(&ticket);

        match given {
            Ok(given) => Outcome::Ready(Answer {
                text: given.text,
                model: self.endpoint.model.clone(),
                describes: resolved.describes,
                tokens: given.tokens,
                kept_out: resolved.content.chars().count(),
            }),
            Err(failed) => Outcome::Refused(Refused { refused: failed.0 }),
        }
    }
}

#[allow(dead_code)]
fn _answer_shape(answer: &Answer) -> Value {
    serde_json::json!({
        "text": answer.text,
        "model": answer.model,
    })
}

#[cfg(test)]
mod tests;
