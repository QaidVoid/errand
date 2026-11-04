//! Asks the forge what was said to the bot, and turns it into messages.
//!
//! The bot's own notifications are the queue. An unread one is activity the
//! daemon has not handled yet. On GitHub, `last_read_at` says where the
//! previous reading of that issue stopped, in GitHub's clock rather than
//! this host's; Forgejo says nothing of the sort, so the poller keeps that
//! place itself, per issue. Each notification is marked read once what it
//! holds has been handed on, and left unread when it could not be read, so
//! it is tried again rather than lost.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::Value;

use super::thread::{account_id, thread_id};
use crate::chat::inbound::{InboundDecision, RawMessage};
use crate::config::schema::ForgeKind;
use crate::forge;
use crate::log::{LogValue, Logger, fields};
use crate::session::pr::{Api, ApiCall};

/// Something said to the bot on a forge, ready to hand to the daemon.
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
    /// Calls the forge's API.
    pub api: Api,
    /// Which forge is called.
    pub kind: ForgeKind,
    /// The token it is called with.
    pub token: String,
    /// The bot's own login, which is what a mention names.
    pub bot: String,
    /// The logins that are heard. Anybody else is not.
    pub allowed: Vec<String>,
    /// The repositories listened to, as `owner/repo` or `owner/*`. Empty
    /// means every one.
    pub repositories: Vec<String>,
    /// Whether naming the bot starts a session.
    pub on_mention: bool,
    /// Whether assigning to the bot starts one.
    pub on_assign: bool,
    /// When the daemon started. Nothing said before it is acted on, as a
    /// chat message sent while the daemon was down is not either.
    pub since: jiff::Timestamp,
    /// Where each issue's reading stopped, for a forge whose notifications
    /// do not say so themselves. Forgejo floors are in memory only and are
    /// bounded below by `since`, including after a restart. An unread
    /// notification left by a failed read before restart is therefore not a
    /// durable retry: activity at or before the new start is skipped. If its
    /// notification was not updated after the start, it is marked read without
    /// fetching the issue. A later update only allows newer activity through.
    pub floors: BTreeMap<String, jiff::Timestamp>,
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
    ///
    /// Takes the poller mutably, since a forge whose notifications do not
    /// say where the last reading stopped is remembered here, between polls.
    pub async fn poll(&mut self) -> Result<Vec<Heard>, String> {
        let name = forge::display(self.kind);
        let answer = match self.kind {
            // GitHub asks for no more than a page at a time, and takes
            // `participating` to keep the answer to threads the bot is part
            // of. Forgejo lists what the account was notified of, which is
            // that already, and pages by `limit`.
            ForgeKind::Github => {
                self.get("/notifications?participating=true&per_page=50")
                    .await?
            }
            // `status-types=unread` keeps read-but-pinned threads out of
            // every poll, since the instance lists those by default and
            // marks none of them read again.
            ForgeKind::Forgejo => {
                self.get("/notifications?limit=50&status-types=unread")
                    .await?
            }
        };
        let Value::Array(notifications) = answer else {
            return Err(format!(
                "{name} answered the notifications with something that is not a list"
            ));
        };

        let mut heard = Vec::new();
        for notification in &notifications {
            match self.read(notification).await {
                Ok(found) => {
                    heard.extend(found);
                    self.mark_read(notification).await;
                }
                Err(why) => self.log.warn(
                    "a notification could not be read; it is tried again next time",
                    &fields([("detail", LogValue::from(why))]),
                ),
            }
        }
        Ok(heard)
    }

    /// What one notification holds that the daemon should hear.
    ///
    /// Takes the poller mutably for the place the next reading of this issue
    /// starts from, which a Forgejo notification does not say itself.
    async fn read(&mut self, notification: &Value) -> Result<Vec<Heard>, String> {
        let subject = &notification["subject"];
        // GitHub spells the subject's kind `Issue` and `PullRequest`; Forgejo
        // spells the same idea `issue` and `pull`. Read either.
        let named = subject["type"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let kind = match named.as_str() {
            "issue" => "Issue",
            "pull" | "pullrequest" => "PullRequest",
            _ => return Ok(Vec::new()),
        };
        let repository = notification["repository"]["full_name"]
            .as_str()
            .ok_or("a notification names no repository")?;
        if !self.listens_in(repository) {
            return Ok(Vec::new());
        }
        let number: u64 = subject["url"]
            .as_str()
            .and_then(|url| url.rsplit('/').next())
            .and_then(|number| number.parse().ok())
            .ok_or("a notification names no issue")?;
        let updated = timestamp(&notification["updated_at"])?;
        // Where the last reading of this issue stopped. GitHub says so on the
        // notification; Forgejo does not, so the poller remembers, and only a
        // remembered place older than the daemon's own start is dismissed.
        let floor = match self.kind {
            ForgeKind::Github => notification["last_read_at"]
                .as_str()
                .and_then(|at| at.parse::<jiff::Timestamp>().ok())
                .map_or(self.since, |read| read.max(self.since)),
            ForgeKind::Forgejo => self
                .floors
                .get(&format!("{repository}#{number}"))
                .copied()
                .map_or(self.since, |read| read.max(self.since)),
        };
        // Nothing has happened on it since the floor, so nothing in it can be
        // new. Not asked about, which is most of what an account that has
        // been mentioned for a while has waiting when the daemon starts.
        if updated <= floor {
            return Ok(Vec::new());
        }

        let issue = self.issue(repository, number, kind).await?;
        let mut said = Vec::new();
        let prefix = forge::thread_prefix(self.kind).trim_end_matches(':');
        if issue.at > floor {
            said.push(Said {
                id: format!("{prefix}-issue:{repository}#{number}"),
                login: issue.login.clone(),
                text: issue.body.clone(),
                at: issue.at,
                mentions: self.on_mention && mentions(&issue.body, &self.bot, self.kind),
            });
        }
        said.extend(self.comments(repository, number, floor, prefix).await?);
        if self.on_assign && self.assignable(notification) {
            said.extend(
                self.assignment(repository, number, &issue, floor, prefix)
                    .await?,
            );
        }
        said.sort_by_key(|said| said.at);

        // Everything on the issue up to here has been handed on or judged,
        // so the next reading of it starts after this, whether or not the
        // notification is marked read.
        if self.kind == ForgeKind::Forgejo {
            self.floors
                .entry(format!("{repository}#{number}"))
                .and_modify(|seen| *seen = (*seen).max(updated))
                .or_insert(updated);
        }

        let thread = thread_id(repository, number, self.kind);
        Ok(said
            .into_iter()
            .filter(|said| said.at > floor && self.hears(&said.login))
            .map(|said| {
                let text = as_said(&said.text, &self.bot, self.kind);
                Heard {
                    thread_id: thread.clone(),
                    mentions: said.mentions,
                    opening: opening(
                        repository,
                        number,
                        &issue,
                        &text,
                        said.id.starts_with(&format!("{prefix}-comment:")),
                        forge::display(self.kind),
                    ),
                    message: RawMessage {
                        id: said.id,
                        author_id: account_id(&said.login, self.kind),
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

    /// Whether a repository is one the daemon listens in.
    fn listens_in(&self, repository: &str) -> bool {
        self.repositories.is_empty()
            || self.repositories.iter().any(|listened| {
                listened.eq_ignore_ascii_case(repository)
                    || listened.strip_suffix("/*").is_some_and(|owner| {
                        repository
                            .split_once('/')
                            .is_some_and(|(of, _)| of.eq_ignore_ascii_case(owner))
                    })
            })
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
        prefix: &str,
    ) -> Result<Vec<Said>, String> {
        let path = format!("/repos/{repository}/issues/{number}/comments?since={floor}");
        let comments = match self.kind {
            ForgeKind::Github => self.github_pages(&format!("{path}&per_page=100")).await?,
            ForgeKind::Forgejo => {
                let answer = self.get(&path).await?;
                answer
                    .as_array()
                    .ok_or("Forgejo answered the comments with something that is not a list")?
                    .clone()
            }
        };
        comments
            .iter()
            .map(|comment| {
                let text = comment["body"].as_str().unwrap_or_default().to_owned();
                Ok(Said {
                    id: format!("{prefix}-comment:{}", comment["id"]),
                    login: comment["user"]["login"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    mentions: self.on_mention && mentions(&text, &self.bot, self.kind),
                    text,
                    at: timestamp(&comment["created_at"])?,
                })
            })
            .collect()
    }

    /// Whether this notification may carry an assignment worth looking for.
    ///
    /// GitHub says why it notified, which narrows the ask to the one
    /// notification an assignment made; Forgejo says nothing of the sort, so
    /// every issue is looked at when assignments are listened for.
    fn assignable(&self, notification: &Value) -> bool {
        match self.kind {
            ForgeKind::Github => notification["reason"].as_str() == Some("assign"),
            ForgeKind::Forgejo => true,
        }
    }

    /// The issue as a request, when somebody heard assigned it to the bot.
    ///
    /// Who assigned it is read off the issue's history, since the notification
    /// does not say, and an assignment is only as good as who made it. GitHub
    /// keeps that history as events; Forgejo keeps it as a timeline, whose
    /// entries name the assignee directly and say whether one was removed.
    /// Forgejo team assignments are not expanded into their members, so they
    /// do not trigger a session even when the bot belongs to the team.
    async fn assignment(
        &self,
        repository: &str,
        number: u64,
        issue: &Issue,
        floor: jiff::Timestamp,
        prefix: &str,
    ) -> Result<Option<Said>, String> {
        let listed = match self.kind {
            ForgeKind::Github => {
                let events = self
                    .github_pages(&format!(
                        "/repos/{repository}/issues/{number}/events?per_page=100"
                    ))
                    .await?;
                events
                    .iter()
                    .filter_map(|event| {
                        let is_assign = event["event"].as_str() == Some("assigned")
                            && event["assignee"]["login"]
                                .as_str()
                                .is_some_and(|login| login.eq_ignore_ascii_case(&self.bot));
                        if !is_assign {
                            return None;
                        }
                        Some((
                            timestamp(&event["created_at"]).ok()?,
                            event["actor"]["login"]
                                .as_str()
                                .unwrap_or_default()
                                .to_owned(),
                            event["id"].as_i64().unwrap_or_default().to_string(),
                            false,
                        ))
                    })
                    .collect::<Vec<_>>()
            }
            ForgeKind::Forgejo => {
                const MAX_TIMELINE_PAGES: u32 = 100;
                let mut listed = Vec::new();
                let mut complete = false;
                for page in 1..=MAX_TIMELINE_PAGES {
                    let answer = self
                        .get(&format!(
                            "/repos/{repository}/issues/{number}/timeline?since={floor}&limit=100&page={page}"
                        ))
                        .await?;
                    let entries = answer
                        .as_array()
                        .ok_or("Forgejo answered the timeline with something that is not a list")?;
                    // An instance may cap the page size below the requested limit.
                    if entries.is_empty() {
                        complete = true;
                        break;
                    }
                    listed.extend(entries.iter().filter_map(|entry| {
                        // Forgejo names this event after the comment kind it
                        // stores it as, which is `assignees`, not `assign`.
                        let is_assign = entry["type"].as_str() == Some("assignees")
                            && entry["assignee"]["login"]
                                .as_str()
                                .is_some_and(|login| login.eq_ignore_ascii_case(&self.bot));
                        if !is_assign {
                            return None;
                        }
                        Some((
                            timestamp(&entry["created_at"]).ok()?,
                            entry["user"]["login"]
                                .as_str()
                                .unwrap_or_default()
                                .to_owned(),
                            entry["id"].as_i64().unwrap_or_default().to_string(),
                            entry["removed_assignee"].as_bool() == Some(true),
                        ))
                    }));
                }
                if !complete {
                    return Err("Forgejo timeline exceeded the page limit".to_owned());
                }
                listed
            }
        };
        // Forgejo removals cancel earlier additions. Judge the latest change
        // before checking who made it, so an untrusted actor cannot revive an
        // earlier trusted assignment.
        let Some((at, actor, id, removed)) = listed.into_iter().max_by_key(|(at, _, _, _)| *at)
        else {
            return Ok(None);
        };
        if removed || at <= floor {
            return Ok(None);
        }
        Ok(Some(Said {
            id: format!("{prefix}-assign:{id}"),
            login: actor,
            text: issue.body.clone(),
            at,
            mentions: true,
        }))
    }

    async fn github_pages(&self, path: &str) -> Result<Vec<Value>, String> {
        const PAGE_SIZE: usize = 100;
        const MAX_PAGES: u32 = 100;
        let mut items = Vec::new();
        for page in 1..=MAX_PAGES {
            let page_path = if page == 1 {
                path.to_owned()
            } else {
                format!("{path}&page={page}")
            };
            let answer = self.get(&page_path).await?;
            let entries = answer.as_array().ok_or_else(|| {
                format!("GitHub answered {page_path} with something that is not a list")
            })?;
            items.extend(entries.iter().cloned());
            if entries.len() < PAGE_SIZE {
                return Ok(items);
            }
        }
        Err(format!("GitHub listing {path} exceeded the page limit"))
    }

    async fn mark_read(&self, notification: &Value) {
        // GitHub names a thread with a string id; Forgejo with a number.
        // Either is a path segment once read out.
        let id = match &notification["id"] {
            Value::String(text) => text.clone(),
            Value::Number(number) => number.to_string(),
            _ => return,
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
                "a notification could not be marked read, so it may be heard twice",
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
                "{} answered {} for {path}: {}",
                forge::display(self.kind),
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
        .ok_or_else(|| format!("the forge gave a time that is not one: {value}"))
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
pub fn mentions(text: &str, bot: &str, kind: ForgeKind) -> bool {
    logins_named(text, kind)
        .iter()
        .any(|(_, _, login)| login.eq_ignore_ascii_case(bot))
}

/// The text as a session is told it.
///
/// The bot's own mention is taken out, since it summoned the bot and is not
/// part of what was asked. Anybody else named is written as the chat names an
/// account, `<@forge:login>` with the forge's own prefix, so `!allow @login`
/// reaches that person as `!allow @somebody` does in a thread. The rest is
/// kept as written, lines and all.
pub fn as_said(text: &str, bot: &str, kind: ForgeKind) -> String {
    let mut said = String::with_capacity(text.len());
    let mut from = 0;
    for (start, end, login) in logins_named(text, kind) {
        said.push_str(&text[from..start]);
        if !login.eq_ignore_ascii_case(bot) {
            let _ = write!(said, "<@{}>", account_id(login, kind));
        }
        from = end;
    }
    said.push_str(&text[from..]);
    said.trim().to_owned()
}

/// Every `@login` in text that stands on its own, with where it sits.
///
/// Not one inside an address, `me@example.com`, or a longer login, which a
/// login's own characters would continue. The forge defines which login
/// characters and lengths are accepted.
fn logins_named(text: &str, kind: ForgeKind) -> Vec<(usize, usize, &str)> {
    let bytes = text.as_bytes();
    let part_of_login = |byte: u8| forge::login_character(kind, byte);
    let mut named = Vec::new();
    for (at, _) in text.match_indices('@') {
        if at > 0 && (part_of_login(bytes[at - 1]) || matches!(bytes[at - 1], b'.' | b'_' | b'/')) {
            continue;
        }
        let length = bytes[at + 1..]
            .iter()
            .take_while(|byte| part_of_login(**byte))
            .count();
        let login = &text[at + 1..at + 1 + length];
        // A final period is sentence punctuation, not part of a dotted login.
        let login = login.trim_end_matches('.');
        if bytes.get(at + 1 + length) == Some(&b'_') {
            continue;
        }
        if forge::valid_login(kind, login) {
            named.push((at, at + 1 + login.len(), login));
        }
    }
    named
}

/// What a session started on an issue is first told.
///
/// The words asked, then where they were asked. A session started here has
/// seen nothing of the issue, so its title, link, and, when the asking was a
/// comment, what the issue itself says, come with them.
fn opening(
    repository: &str,
    number: u64,
    issue: &Issue,
    text: &str,
    from_comment: bool,
    name: &str,
) -> String {
    let mut opening = format!(
        "{text}\n\n(Asked on {name}, on {} {repository}#{number}, \"{}\": {}. The last thing \
         you say each turn is posted there as a comment, so write it for the people reading the \
         {0}, and do not comment there yourself.)",
        issue.kind, issue.title, issue.link
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
