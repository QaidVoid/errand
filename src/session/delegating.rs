//! The daemon's half of a delegation: reading requests and writing answers.
//!
//! The agent writes a request into the one directory both sides can reach, and
//! this picks it up, runs it, and writes the answer beside it. Polling rather
//! than watching, because a request arrives at most a few times a turn and a
//! watch on a directory inside a sandbox is more machinery than that is worth.
//!
//! Nothing here can fail a turn. A request that cannot be answered is refused
//! in words the agent reads, and the work stays with the session's own model.

use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::agent::delegate::{Answer, DelegationOutcome, TurnDelegations};
use crate::agent::delegation::Refused;
use crate::agent::requests::DELEGATE_DIR;
use crate::log::{LogValue, Logger, fields};
use crate::provider::ask::Sender;

/// How often the directory is looked at while a session is running.
pub const POLL_MS: u64 = 200;

/// What a delegation produced, for whoever is showing the session.
#[derive(Debug, Clone, PartialEq)]
pub struct Reported {
    /// What was asked, as far as the request could be read.
    pub asked: String,
    /// What came of asking.
    pub outcome: DelegationOutcome,
}

/// The answer as the agent reads it.
///
/// Labelled, and naming the model. An agent that forgets it did not read the
/// thing itself starts asserting a description as an observation, which is the
/// one failure this whole shape exists to avoid.
pub fn labelled(answer: &Answer) -> String {
    [
        format!(
            "{} was asked about {} and said the following.",
            answer.model, answer.describes
        ),
        "It is a description rather than the thing itself, so check it before you".to_owned(),
        "rely on it.".to_owned(),
        String::new(),
        answer.text.clone(),
        String::new(),
    ]
    .join("\n")
}

/// Picks up delegation requests for one session and answers them.
///
/// The polling itself belongs to the session's own loop; what lives here is
/// the sweep and the writing of answers, so a test need not wait on a timer.
pub struct Delegating {
    directory: PathBuf,
    log: Logger,
    report: Arc<dyn Fn(Reported) + Send + Sync>,
}

impl Delegating {
    /// Watches one session's exchange directory, reporting to `report`.
    pub fn new(state_dir: &Path, log: Logger, report: Arc<dyn Fn(Reported) + Send + Sync>) -> Self {
        Self {
            directory: state_dir.join(DELEGATE_DIR),
            log,
            report,
        }
    }

    /// Answers whatever is waiting, through the turn's delegations when a turn
    /// is running and with a refusal when there is none.
    pub async fn sweep<S, P>(&self, mut turn: Option<&mut TurnDelegations<S, P>>)
    where
        S: crate::agent::delegation::Sources + 'static,
        P: Sender,
    {
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            // No directory yet, which is every session that has not delegated.
            return;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".request"))
            .collect();
        names.sort();

        for name in names {
            self.answer(&name, turn.as_deref_mut()).await;
        }
    }

    async fn answer<S, P>(&self, name: &str, turn: Option<&mut TurnDelegations<S, P>>)
    where
        S: crate::agent::delegation::Sources + 'static,
        P: Sender,
    {
        let id = name.strip_suffix(".request").unwrap_or(name).to_owned();
        let path = self.directory.join(name);

        let raw = std::fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|text| {
                serde_json::from_str::<Value>(&text).map_err(|error| error.to_string())
            });
        let raw = match raw {
            Ok(raw) => raw,
            Err(error) => {
                self.write(
                    &id,
                    "refused",
                    "that delegation could not be read as a request",
                );
                self.warn("a delegation request could not be read", &error);
                let _ = std::fs::remove_file(&path);
                return;
            }
        };
        // Removed before it is run, so a request cannot be answered twice if
        // running it takes longer than the next sweep.
        let _ = std::fs::remove_file(&path);

        let asked = raw
            .get("question")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();

        let Some(turn) = turn else {
            let refused = "there is no turn running to delegate from";
            (self.report)(Reported {
                asked,
                outcome: DelegationOutcome::Refused(Refused {
                    refused: refused.to_owned(),
                }),
            });
            self.write(&id, "refused", refused);
            return;
        };

        let outcome = turn.run(&raw).await;
        match &outcome {
            DelegationOutcome::Refused(refused) => {
                (self.report)(Reported {
                    asked,
                    outcome: outcome.clone(),
                });
                self.write(
                    &id,
                    "refused",
                    &format!("{}; carry on yourself", refused.refused),
                );
            }
            DelegationOutcome::Ready(answer) => {
                (self.report)(Reported {
                    asked,
                    outcome: outcome.clone(),
                });
                self.write(&id, "answer", &labelled(answer));
            }
        }
    }

    /// Writes an answer where the agent is waiting for it.
    ///
    /// Under a temporary name and then renamed, so the agent cannot read half
    /// of one and treat it as the whole answer.
    fn write(&self, id: &str, kind: &str, body: &str) {
        let target = self.directory.join(format!("{id}.{kind}"));
        let writing = self.directory.join(format!("{id}.{kind}.writing"));
        let body = if body.ends_with('\n') {
            body.to_owned()
        } else {
            format!("{body}\n")
        };
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&writing)
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(body.as_bytes())
            })
            .and_then(|()| std::fs::rename(&writing, &target));
        if let Err(error) = written {
            self.warn(
                "a delegation answer could not be written",
                &error.to_string(),
            );
        }
    }

    fn warn(&self, message: &str, detail: &str) {
        self.log
            .warn(message, &fields([("detail", LogValue::from(detail))]));
    }
}

#[cfg(test)]
#[path = "delegating/tests.rs"]
mod tests;
