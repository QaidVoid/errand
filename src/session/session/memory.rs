//! What a session remembers between conversations, and what it forgets.
//!
//! The store itself is `crate::memory::store`. This is the session half: the
//! block a sandbox is given at launch, the facts harvested out of a turn, and
//! the commands a person uses to read or clear them.

use super::{IncomingMessage, Running};
use crate::agent::requests::delegate_instructions;
use crate::log::{LogValue, fields, now_ms};
use crate::memory::store::{
    BLOCK_FILENAME, DEFAULT_MEMORY_BUDGET, MemoryStore, NOTES_FILENAME, PROJECT_NOTES_FILENAME,
    Scope, memory_instructions, parse_notes,
};
use crate::sandbox::backend::STATE_PATH;
use crate::sandbox::paths;
use crate::session::commands::parse_user_id;
use crate::session::event::ReactionOutcome;
use crate::session::github::review_instructions;

impl Running {
    /// Writes what the agent should know before it starts, as its system
    /// prompt. Returns the path, or none when there is nothing to say and it
    /// would cost context for no benefit.
    pub(super) fn write_memory_block(&self) -> Option<String> {
        let memory = self.options.memory.as_ref()?;

        let about = [
            memory
                .render(&self.options.owner_id, DEFAULT_MEMORY_BUDGET)
                .unwrap_or_default(),
            memory
                .render_project(&self.options.project.name, DEFAULT_MEMORY_BUDGET)
                .unwrap_or_default(),
        ]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

        // Only when the agent can actually push. Telling it how to attribute
        // a pull request it has no credential to open is instruction for its
        // own sake.
        let attribution = self
            .options
            .config
            .github
            .as_ref()
            .map(|github| review_instructions(github, &self.requested_by(), &self.session_links()))
            .unwrap_or_default();

        let delegating = self
            .options
            .config
            .agent
            .delegate
            .as_ref()
            .map(|delegate| delegate_instructions(&delegate.model, delegate.per_turn as usize))
            .unwrap_or_default();

        let contents = format!(
            "{}{}{}{attribution}{delegating}",
            self.house_rules().unwrap_or_default(),
            about,
            memory_instructions(
                &format!("{STATE_PATH}/{NOTES_FILENAME}"),
                &format!("{STATE_PATH}/{PROJECT_NOTES_FILENAME}"),
            )
        );

        let path = std::path::Path::new(&self.options.state_dir).join(BLOCK_FILENAME);
        // Written before the sandbox is launched, so nothing else has had
        // reason to create the directory yet. The notes files are created
        // empty so the agent appends to a file it can see exists.
        let written = std::fs::create_dir_all(&self.options.state_dir)
            .and_then(|()| {
                // Through the state directory rather than at it: a session
                // writes this tree, so the name can have become a link.
                paths::write_beneath(
                    &self.options.state_dir,
                    BLOCK_FILENAME,
                    format!("{contents}\n").as_bytes(),
                )
            })
            .and_then(|()| {
                for notes in [NOTES_FILENAME, PROJECT_NOTES_FILENAME] {
                    match paths::open_beneath(
                        &self.options.state_dir,
                        notes,
                        &paths::OpenOptions::create_new(),
                    ) {
                        Ok(_) => {}
                        // Already there is the ordinary state of a resumed
                        // session: the file was made when it first started.
                        // Treating it as a failure loses the whole block,
                        // and with it the house rules and the memory.
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error),
                    }
                }
                Ok(())
            });
        if let Err(error) = written {
            self.log.warn(
                "could not write the memory block",
                &fields([("detail", LogValue::from(error.to_string()))]),
            );
            return None;
        }
        Some(path.display().to_string())
    }

    /// Prefixes a speaker's memory to their first message in this session.
    pub(super) fn introduce(&mut self, message: &IncomingMessage, content: &str) -> String {
        let Some(memory) = &self.options.memory else {
            return content.to_owned();
        };
        if self.introduced.contains(&message.author_id) {
            return content.to_owned();
        }

        self.introduced.insert(message.author_id.clone());
        if let Some(name) = &message.author_name {
            let _ = memory.remember_user(&message.author_id, name, now_ms());
        }

        // The owner was already introduced through the system prompt.
        if message.author_id == self.options.owner_id {
            return content.to_owned();
        }

        let block = memory
            .render_for_speaker(&message.author_id, DEFAULT_MEMORY_BUDGET)
            .unwrap_or_default();
        if block.is_empty() {
            content.to_owned()
        } else {
            format!("{block}\n\n{content}")
        }
    }

    /// Reads anything the agent wrote to its notes files and stores it.
    pub(super) fn harvest_memory(&mut self) {
        let Some(memory) = self.options.memory.clone() else {
            return;
        };

        // Attributed to whoever spoke this turn, not to the session's owner:
        // in a shared thread the facts being recorded are about the person
        // talking.
        let about = self
            .last_speaker_id
            .clone()
            .unwrap_or_else(|| self.options.owner_id.clone());
        let stored = self.harvest(&memory, NOTES_FILENAME, true, &about)
            + self.harvest(
                &memory,
                PROJECT_NOTES_FILENAME,
                false,
                &self.options.project.name,
            );

        if stored > 0 {
            self.log.info(
                "recorded facts from a turn",
                &fields([
                    ("stored", LogValue::from(stored)),
                    ("about", LogValue::from(about.as_str())),
                ]),
            );
        }
    }

    /// Reads one notes file and stores what it holds.
    ///
    /// The file is emptied rather than deleted, so the agent's next append
    /// lands in a file it already knows exists and no line is ever ingested
    /// twice.
    pub(super) fn harvest(
        &self,
        memory: &MemoryStore,
        filename: &str,
        user_scope: bool,
        subject: &str,
    ) -> usize {
        let Ok(contents) = paths::read_beneath(&self.options.state_dir, filename) else {
            return 0;
        };

        let mut stored = 0;
        for fact in parse_notes(&contents) {
            let scope = if user_scope {
                Scope::User
            } else {
                Scope::Project
            };
            if memory
                .remember(scope, subject, &fact, &self.options.id, now_ms())
                .unwrap_or(false)
            {
                stored += 1;
            }
        }
        // A fact offered again is deduplicated, so a failure to empty the
        // file is not worth failing on.
        let _ = paths::truncate_beneath(&self.options.state_dir, filename);
        stored
    }

    /// Who or what a memory command was aimed at.
    pub(super) fn memory_subject(
        &self,
        rest: &str,
        asked_by: &str,
    ) -> Option<(bool, String, String)> {
        let trimmed = rest.trim();
        if trimmed.is_empty() {
            return Some((true, asked_by.to_owned(), format!("<@{asked_by}>")));
        }
        if trimmed.to_lowercase() == "project" {
            return Some((
                false,
                self.options.project.name.clone(),
                format!("`{}`", self.options.project.name),
            ));
        }
        parse_user_id(trimmed).map(|id| (true, id.clone(), format!("<@{id}>")))
    }

    /// Reads back what the agent is told before it answers.
    pub(super) async fn report_facts(&mut self, rest: &str, message: &IncomingMessage) {
        let Some(memory) = self.options.memory.as_ref() else {
            self.say("nothing is remembered on this host").await;
            return;
        };
        let Some((user_scope, subject, label)) = self.memory_subject(rest, &message.author_id)
        else {
            self.refuse(
                message,
                "say who, as `!facts @somebody`, or `!facts project`",
            )
            .await;
            return;
        };

        let scope = if user_scope {
            Scope::User
        } else {
            Scope::Project
        };
        let facts = memory
            .facts_for(scope, &subject, i64::MAX)
            .unwrap_or_default();
        if facts.is_empty() {
            self.say(&format!("nothing is remembered about {label}"))
                .await;
            return;
        }
        // Numbered the way they are held, so what a person reads back is what
        // the agent was given, newest first.
        let lines = facts
            .iter()
            .map(|fact| format!("- {}", fact.fact))
            .collect::<Vec<_>>()
            .join("\n");
        self.say(&format!("remembered about {label}:\n{lines}"))
            .await;
    }

    /// Drops what is remembered, which changes every later session too.
    pub(super) async fn forget_facts(&mut self, rest: &str, message: &IncomingMessage) {
        let Some(memory) = self.options.memory.as_ref() else {
            self.say("nothing is remembered on this host").await;
            return;
        };
        if rest.trim().is_empty() {
            self.refuse(
                message,
                "say who, as `!forget @somebody`, or `!forget project`",
            )
            .await;
            return;
        }
        let Some((user_scope, subject, label)) = self.memory_subject(rest, &message.author_id)
        else {
            self.refuse(
                message,
                "say who, as `!forget @somebody`, or `!forget project`",
            )
            .await;
            return;
        };

        let scope = if user_scope {
            Scope::User
        } else {
            Scope::Project
        };
        let gone = memory.forget(scope, &subject).unwrap_or(0);
        self.react(&message.id, ReactionOutcome::Accepted).await;
        self.say(&if gone == 0 {
            format!("nothing was remembered about {label}")
        } else {
            format!(
                "forgot {gone} fact{} about {label}",
                if gone == 1 { "" } else { "s" }
            )
        })
        .await;
    }
}
