//! Opening a pull request from what a session changed.
//!
//! The mechanics are `crate::session::pr`. This is the session half: who
//! asked, where the conversation is, and what the thread is told about it.

use super::{Asked, IncomingMessage, Running, reason};
use crate::chat::render::connection_line;
use crate::log::{LogValue, fields};
use crate::sandbox::backend::STATE_PATH;
use crate::sandbox::paths;
use crate::session::event::ReactionOutcome;
use crate::session::github::{
    ASKED_FILENAME, REQUEST_FILENAME, SessionLinks, thread_link, transcript_link,
};
use crate::session::pr;
use crate::session::record::{prepare_record_dir, record_dir};

impl Running {
    /// Who a pull request from this session is on behalf of.
    pub(super) fn requested_by(&self) -> String {
        if let Some(asked) = self.who_asked()
            && asked.id != self.options.owner_id
        {
            return self
                .display_name_of(&asked.id)
                .or_else(|| asked.name.clone())
                .unwrap_or(asked.id);
        }
        self.display_name_of(&self.options.owner_id)
            .or_else(|| self.options.owner_name.clone())
            .unwrap_or_else(|| self.options.owner_id.clone())
    }

    pub(super) fn display_name_of(&self, user_id: &str) -> Option<String> {
        self.options
            .memory
            .as_ref()?
            .display_name(user_id)
            .ok()
            .flatten()
    }

    /// Whoever asked for a pull request, which is not always whose session it
    /// is.
    pub(super) fn who_asked(&self) -> Option<Asked> {
        let contents = std::fs::read_to_string(
            std::path::Path::new(&record_dir(&self.options.state_dir)).join(ASKED_FILENAME),
        )
        .ok()?;
        let mut lines = contents.split('\n');
        let id = lines.next()?.trim();
        if id.is_empty() {
            return None;
        }
        Some(Asked {
            id: id.to_owned(),
            name: lines
                .next()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        })
    }

    /// Where this session can be read back, for a pull request to point at.
    pub(super) fn session_links(&self) -> SessionLinks {
        SessionLinks {
            thread: self
                .options
                .thread_id
                .as_ref()
                .zip(self.options.guild_id.as_ref())
                .map(|(thread, guild)| thread_link(guild, thread)),
            transcript: self
                .options
                .public_url
                .as_ref()
                .map(|url| transcript_link(url, &self.options.id)),
        }
    }

    /// Records that a pull request was asked for, and by whom, for a resume.
    pub(super) fn note_pull_request_asked(&self, message: &IncomingMessage) {
        let directory = prepare_record_dir(&self.options.state_dir);
        let written = std::fs::write(
            std::path::Path::new(&directory).join(ASKED_FILENAME),
            format!(
                "{}\n{}\n",
                message.author_id,
                message.author_name.clone().unwrap_or_default()
            ),
        );
        if let Err(error) = written {
            self.log.warn(
                "could not record that a pull request was asked for",
                &fields([("detail", LogValue::from(error.to_string()))]),
            );
        }
    }

    /// Opens the pull request the agent asked for, if it asked for one.
    pub(super) async fn open_requested_pull_request(&mut self) {
        let path = std::path::Path::new(&self.options.state_dir).join(REQUEST_FILENAME);
        let Ok(contents) = paths::read_beneath(&self.options.state_dir, REQUEST_FILENAME) else {
            return;
        };

        if let Err(error) = std::fs::remove_file(&path) {
            self.log.warn(
                "could not clear the pull request request",
                &fields([("detail", LogValue::from(error.to_string()))]),
            );
        }

        if self.who_asked().is_none() {
            self.log
                .warn("ignored a pull request nobody asked for", &fields([]));
            self.say(
                "a pull request was asked for by the agent, not by anyone here, so it was ignored. Ask for one and it will go through",
            )
            .await;
            return;
        }

        let mut lines = contents.split('\n');
        let title = lines.next().unwrap_or_default().trim().to_owned();
        let repository = lines
            .map(|line| line.trim().strip_prefix("repository:").map(str::trim))
            .find_map(|name| name.filter(|name| !name.is_empty()))
            .map(str::to_owned);

        if title.is_empty() {
            self.say("a pull request was asked for without a title, so none was opened")
                .await;
            return;
        }
        self.open_pull_request_now(&title, repository.as_deref())
            .await;
    }

    /// Opens one on request, reporting the outcome on the message that asked.
    pub(super) async fn open_pull_request_command(
        &mut self,
        title: &str,
        message: &IncomingMessage,
    ) {
        if self.options.config.github.is_none() {
            self.say("no GitHub identity is configured, so there is nowhere to open one")
                .await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        }
        if title.trim().is_empty() {
            self.say("say what to call it, as `!pr <title>`").await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        }

        self.react(&message.id, ReactionOutcome::Accepted).await;
        let opened = self.open_pull_request_now(title.trim(), None).await;
        self.react(
            &message.id,
            if opened {
                ReactionOutcome::Succeeded
            } else {
                ReactionOutcome::Failed
            },
        )
        .await;
    }

    /// Opens one, reporting either the address or why it did not.
    pub(super) async fn open_pull_request_now(
        &mut self,
        title: &str,
        repository: Option<&str>,
    ) -> bool {
        let (Some(github), Some(open)) = (
            self.options.config.github.clone(),
            self.options.open_pull_request.clone(),
        ) else {
            self.say("no GitHub identity is configured, so there is nowhere to open one")
                .await;
            return false;
        };

        match open(pr::Request {
            github,
            project_path: self.options.project.path.clone(),
            repository: repository.map(str::to_owned),
            title: title.to_owned(),
            requested_by: self.requested_by(),
            links: self.session_links(),
        })
        .await
        {
            Ok(url) => {
                self.say(&connection_line(&format!("opened {url}"))).await;
                self.pull_request_outcome = Some(format!(
                    "(From errand: the pull request was opened at {url}.)"
                ));
                true
            }
            Err(error) => {
                self.log.warn(
                    "could not open a pull request",
                    &fields([("detail", LogValue::from(error.to_string()))]),
                );
                self.say(&reason(&error)).await;
                self.pull_request_outcome = Some(format!(
                    "(From errand: the pull request was not opened: {}. To try again, \
                     put right what that says and write {STATE_PATH}/{REQUEST_FILENAME} again.)",
                    reason(&error).trim_end_matches('.')
                ));
                false
            }
        }
    }
}
