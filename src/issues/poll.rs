//! Asks GitHub what was said to the bot, and turns it into messages.
//!
//! The bot's own notifications are the queue. An unread one is activity the
//! daemon has not handled yet, and `last_read_at` says where the previous
//! reading of that issue stopped, in GitHub's clock rather than this host's.
//! Each is marked read once what it holds has been handed on, and left unread
//! when it could not be read, so it is tried again rather than lost.

use std::fmt::Write as _;

use serde_json::Value;

use super::thread::{account_id, thread_id};
use crate::chat::inbound::{InboundDecision, RawMessage};
use crate::log::{LogValue, Logger, fields};
use crate::session::pr::{Api, ApiCall};

/// Something said to the bot on GitHub, ready to hand to the daemon.
#[derive(Debug, Clone, PartialEq)]
pub struct Heard {
    /// The issue or pull request it was said on, as a thread name.
    pub thread_id: String,
    /// Whether it names the bot, which is what may start a session.
    pub mentions: bool,
    /// What was said, as a message continuing the thread.
    pub message: RawMessage,
    /// What starts a session with it: the project, the words, and where they
    /// came from, since a session started here has seen nothing else.
    pub opening: String,
}

/// Reads the bot's notifications as whoever the token belongs to.
pub struct Poller {
    /// Calls the GitHub API.
    pub api: Api,
    /// The token it is called with.
    pub token: String,
    /// The bot's own login, which is what a mention names.
    pub bot: String,
    /// The logins that are heard. Anybody else is not.
    pub allowed: Vec<String>,
    /// When the daemon started. Nothing said before it is acted on, as a
    /// chat message sent while the daemon was down is not either.
    pub since: jiff::Timestamp,
    /// Where what could not be read is reported.
    pub log: Logger,
}

/// One thing said on an issue, before it is judged.
struct Said {
    id: String,
    login: String,
    text: String,
    at: jiff::Timestamp,
    mentions: bool,
}

/// What an issue or pull request is, for the opening of a session.
struct Issue {
    title: String,
    body: String,
    link: String,
    kind: &'static str,
    at: jiff::Timestamp,
    login: String,
}

/// The most of an issue's own text carried into the opening of a session.
const ISSUE_TEXT_LIMIT: usize = 4_000;

impl Poller {
    /// Reads every unread notification on an issue or pull request, and
    /// returns what was said there by somebody heard, oldest first.
    pub async fn poll(&self) -> Result<Vec<Heard>, String> {
        let answer = self
            .get("/notifications?participating=true&per_page=50")
            .await?;
        let Value::Array(notifications) = answer else {
            return Err(
                "GitHub answered the notifications with something that is not a list".to_owned(),
            );
        };

        let mut heard = Vec::new();
        for notification in &notifications {
            match self.read(notification).await {
                Ok(found) => {
                    heard.extend(found);
                    self.mark_read(notification).await;
                }
                Err(why) => self.log.warn(
                    "a GitHub notification could not be read; it is tried again next time",
                    &fields([("detail", LogValue::from(why))]),
                ),
            }
        }
        Ok(heard)
    }

    /// What one notification holds that the daemon should hear.
    async fn read(&self, notification: &Value) -> Result<Vec<Heard>, String> {
        let subject = &notification["subject"];
        let kind = subject["type"].as_str().unwrap_or_default();
        if kind != "Issue" && kind != "PullRequest" {
            return Ok(Vec::new());
        }
        let repository = notification["repository"]["full_name"]
            .as_str()
            .ok_or("a notification names no repository")?;
        let number: u64 = subject["url"]
            .as_str()
            .and_then(|url| url.rsplit('/').next())
            .and_then(|number| number.parse().ok())
            .ok_or("a notification names no issue")?;
        let last_read = notification["last_read_at"]
            .as_str()
            .and_then(|at| at.parse::<jiff::Timestamp>().ok());
        let floor = last_read.map_or(self.since, |read| read.max(self.since));
        // Nothing has happened on it since the floor, so nothing in it can be
        // new. Not asked about, which is most of what an account that has
        // been mentioned for a while has waiting when the daemon starts.
        if timestamp(&notification["updated_at"]).is_ok_and(|updated| updated <= floor) {
            return Ok(Vec::new());
        }

        let issue = self.issue(repository, number, kind).await?;
        let mut said = Vec::new();
        if issue.at > floor {
            said.push(Said {
                id: format!("github-issue:{repository}#{number}"),
                login: issue.login.clone(),
                text: issue.body.clone(),
                at: issue.at,
                mentions: mentions(&issue.body, &self.bot),
            });
        }
        said.extend(self.comments(repository, number, floor).await?);
        if notification["reason"].as_str() == Some("assign") {
            said.extend(self.assignment(repository, number, &issue, floor).await?);
        }
        said.sort_by_key(|said| said.at);

        let thread = thread_id(repository, number);
        Ok(said
            .into_iter()
            .filter(|said| said.at > floor && self.hears(&said.login))
            .map(|said| {
                let text = as_said(&said.text, &self.bot);
                Heard {
                    thread_id: thread.clone(),
                    mentions: said.mentions,
                    opening: opening(
                        repository,
                        number,
                        &issue,
                        &text,
                        said.id.starts_with("github-comment:"),
                    ),
                    message: RawMessage {
                        id: said.id,
                        author_id: account_id(&said.login),
                        author_name: Some(said.login),
                        author_is_bot: false,
                        channel_id: thread.clone(),
                        parent_channel_id: None,
                        content: text,
                        attachments: Vec::new(),
                    },
                }
            })
            .collect())
    }

    /// Whether a login is one the daemon listens to, and not the bot itself.
    fn hears(&self, login: &str) -> bool {
        !login.eq_ignore_ascii_case(&self.bot)
            && self
                .allowed
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(login))
    }

    async fn issue(&self, repository: &str, number: u64, kind: &str) -> Result<Issue, String> {
        let issue = self
            .get(&format!("/repos/{repository}/issues/{number}"))
            .await?;
        Ok(Issue {
            title: issue["title"].as_str().unwrap_or_default().to_owned(),
            body: issue["body"].as_str().unwrap_or_default().to_owned(),
            link: issue["html_url"].as_str().unwrap_or_default().to_owned(),
            kind: if kind == "PullRequest" {
                "pull request"
            } else {
                "issue"
            },
            at: timestamp(&issue["created_at"])?,
            login: issue["user"]["login"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        })
    }

    async fn comments(
        &self,
        repository: &str,
        number: u64,
        floor: jiff::Timestamp,
    ) -> Result<Vec<Said>, String> {
        let comments = self
            .get(&format!(
                "/repos/{repository}/issues/{number}/comments?since={floor}&per_page=100"
            ))
            .await?;
        comments
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|comment| {
                let text = comment["body"].as_str().unwrap_or_default().to_owned();
                Ok(Said {
                    id: format!("github-comment:{}", comment["id"]),
                    login: comment["user"]["login"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    mentions: mentions(&text, &self.bot),
                    text,
                    at: timestamp(&comment["created_at"])?,
                })
            })
            .collect()
    }

    /// The issue as a request, when somebody heard assigned it to the bot.
    ///
    /// Who assigned it is read from the issue's events, since the notification
    /// does not say, and an assignment is only as good as who made it.
    async fn assignment(
        &self,
        repository: &str,
        number: u64,
        issue: &Issue,
        floor: jiff::Timestamp,
    ) -> Result<Option<Said>, String> {
        let events = self
            .get(&format!(
                "/repos/{repository}/issues/{number}/events?per_page=100"
            ))
            .await?;
        let assigned = events
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .rev()
            .find(|event| {
                event["event"].as_str() == Some("assigned")
                    && event["assignee"]["login"]
                        .as_str()
                        .is_some_and(|login| login.eq_ignore_ascii_case(&self.bot))
            });
        let Some(assigned) = assigned else {
            return Ok(None);
        };
        let at = timestamp(&assigned["created_at"])?;
        if at <= floor {
            return Ok(None);
        }
        Ok(Some(Said {
            id: format!("github-assign:{}", assigned["id"]),
            login: assigned["actor"]["login"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            text: issue.body.clone(),
            at,
            mentions: true,
        }))
    }

    async fn mark_read(&self, notification: &Value) {
        let Some(id) = notification["id"].as_str() else {
            return;
        };
        let answer = (self.api)(
            format!("/notifications/threads/{id}"),
            ApiCall {
                method: "PATCH".to_owned(),
                token: self.token.clone(),
                body: None,
            },
        )
        .await;
        if !(200..300).contains(&answer.status) {
            self.log.warn(
                "a GitHub notification could not be marked read, so it may be heard twice",
                &fields([
                    ("status", LogValue::from(i64::from(answer.status))),
                    (
                        "detail",
                        LogValue::from(
                            answer.body["message"].as_str().unwrap_or("no reason given"),
                        ),
                    ),
                ]),
            );
        }
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        let answer = (self.api)(
            path.to_owned(),
            ApiCall {
                method: "GET".to_owned(),
                token: self.token.clone(),
                body: None,
            },
        )
        .await;
        if answer.status == 200 {
            Ok(answer.body)
        } else {
            Err(format!(
                "GitHub answered {} for {path}: {}",
                answer.status,
                answer.body["message"].as_str().unwrap_or("no reason given")
            ))
        }
    }
}

fn timestamp(value: &Value) -> Result<jiff::Timestamp, String> {
    value
        .as_str()
        .and_then(|at| at.parse().ok())
        .ok_or_else(|| format!("GitHub gave a time that is not one: {value}"))
}

/// What the daemon is handed for something heard on an issue.
///
/// An issue answered by a thread whose session is running, or waiting to be
/// resumed, hears every comment from somebody heard, as the thread hears every
/// reply. One with none starts a session only when the bot was named, and that
/// session is first told where it was asked. Anything else is left be.
pub fn decide(heard: Heard, answered_by: Option<String>) -> Option<(RawMessage, InboundDecision)> {
    if let Some(thread_id) = answered_by {
        return Some((heard.message, InboundDecision::Thread { thread_id }));
    }
    if !heard.mentions {
        return None;
    }
    let mut message = heard.message;
    message.content = heard.opening;
    Some((message, InboundDecision::Start))
}

/// Whether text names the bot, as `@login` standing on its own.
pub fn mentions(text: &str, bot: &str) -> bool {
    logins_named(text)
        .iter()
        .any(|(_, _, login)| login.eq_ignore_ascii_case(bot))
}

/// The text as a session is told it.
///
/// The bot's own mention is taken out, since it summoned the bot and is not
/// part of what was asked. Anybody else named is written as the chat names an
/// account, `<@github:login>`, so `!allow @login` reaches that person as
/// `!allow @somebody` does in a thread. The rest is kept as written, lines
/// and all.
pub fn as_said(text: &str, bot: &str) -> String {
    let mut said = String::with_capacity(text.len());
    let mut from = 0;
    for (start, end, login) in logins_named(text) {
        said.push_str(&text[from..start]);
        if !login.eq_ignore_ascii_case(bot) {
            let _ = write!(said, "<@{}>", account_id(login));
        }
        from = end;
    }
    said.push_str(&text[from..]);
    said.trim().to_owned()
}

/// Every `@login` in text that stands on its own, with where it sits.
///
/// Not one inside an address, `me@example.com`, or a longer login, which a
/// login's own characters would continue. A login is letters, digits, and
/// single hyphens, at most 39 of them.
fn logins_named(text: &str) -> Vec<(usize, usize, &str)> {
    let bytes = text.as_bytes();
    let part_of_login = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'-';
    let mut named = Vec::new();
    for (at, _) in text.match_indices('@') {
        if at > 0 && (part_of_login(bytes[at - 1]) || matches!(bytes[at - 1], b'.' | b'_' | b'/')) {
            continue;
        }
        let length = bytes[at + 1..]
            .iter()
            .take_while(|byte| part_of_login(**byte))
            .count();
        if (1..=39).contains(&length) {
            named.push((at, at + 1 + length, &text[at + 1..at + 1 + length]));
        }
    }
    named
}

/// The project a session started on an issue works in.
///
/// One per issue or pull request, named after the repository and number, so
/// two issues in one repository are worked on side by side and coming back to
/// the same issue reaches the same directory.
pub fn project_name(repository: &str, number: u64) -> String {
    let name = repository.rsplit('/').next().unwrap_or(repository);
    let mut project: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    if !project.starts_with(|character: char| character.is_ascii_alphanumeric()) {
        project.insert(0, 'r');
    }
    let suffix = format!("-{number}");
    project.truncate(64 - suffix.len());
    project + &suffix
}

/// What a session started on an issue is first told.
///
/// The words asked, then where they were asked. A session started here has
/// seen nothing of the issue, so its title, link, and, when the asking was a
/// comment, what the issue itself says, come with them.
fn opening(repository: &str, number: u64, issue: &Issue, text: &str, from_comment: bool) -> String {
    let mut opening = format!(
        "{}: {text}\n\n(Asked on GitHub, on {} {repository}#{number}, \"{}\": {}. Your answer is \
         posted there as a comment.)",
        project_name(repository, number),
        issue.kind,
        issue.title,
        issue.link
    );
    if from_comment && !issue.body.trim().is_empty() {
        let body: String = issue.body.chars().take(ISSUE_TEXT_LIMIT).collect();
        let cut = if body.len() < issue.body.len() {
            "\n(cut short)"
        } else {
            ""
        };
        let _ = write!(opening, "\n\nThe {} says:\n\n{body}{cut}", issue.kind);
    }
    opening
}

#[cfg(test)]
mod tests;
