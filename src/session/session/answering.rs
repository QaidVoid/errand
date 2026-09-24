//! What a `!` command does to the session it was typed in.
//!
//! The table of commands, who may run each, and what they say is
//! `crate::session::commands`. This is the half that acts on one.

use super::{Attached, IncomingMessage, Running, SessionTimer, display_name, end_reason_name};
use crate::chat::render::{bytes as byte_count, compaction_line, connection_line};
use crate::log::{LogValue, fields};
use std::collections::BTreeMap;

use crate::provider::models::AvailableModel;
use crate::session::commands::{
    COMMANDS, CommandAccess, Standing, help_text, may_run, parse_user_id,
};
use crate::session::event::{EndReason, ReactionOutcome, SessionEvent};
use crate::session::model::{expand_alias, split_level};

/// Which model a name picked out, when a name can pick out more than one.
pub(super) enum Chosen {
    /// Exactly one model answers to the name.
    One(AvailableModel),
    /// Nothing the host lists answers to it.
    None,
    /// Several providers serve a model of that name, qualified for reply.
    Several(Vec<String>),
}

/// Finds the model a name asks for, and the provider that serves it.
///
/// A bare id is looked for on the session's own provider first, because
/// staying where you are is the common case and needs no qualification. Only
/// when it is not there does the name range over every other provider, and a
/// name that several serve is refused rather than guessed at.
pub(super) fn choose(available: &[AvailableModel], wanted: &str, current: &str) -> Chosen {
    if available.is_empty() {
        return Chosen::One(AvailableModel {
            provider: current.to_owned(),
            id: wanted.to_owned(),
            default_level: None,
        });
    }

    if let Some((provider, id)) = wanted.split_once('/')
        && let Some(found) = available
            .iter()
            .find(|model| model.provider == provider && model.id == id)
    {
        return Chosen::One(found.clone());
    }

    if let Some(found) = available
        .iter()
        .find(|model| model.provider == current && model.id == wanted)
    {
        return Chosen::One(found.clone());
    }

    let elsewhere: Vec<&AvailableModel> = available
        .iter()
        .filter(|model| model.id == wanted)
        .collect();
    match elsewhere.as_slice() {
        [] => Chosen::None,
        [only] => Chosen::One((*only).clone()),
        several => Chosen::Several(several.iter().map(|model| model.qualified()).collect()),
    }
}

/// Every model, under a heading per provider, the session's own first.
///
/// A name somebody can type back is the point, so a short name is shown
/// beside the model it stands for, the one running is marked, and a level
/// that applies without being asked for is spelled out: a list that never
/// says `muse` cannot be used without reading the configuration first.
pub(super) fn grouped_by_provider(
    available: &[AvailableModel],
    current_provider: &str,
    current_model: &str,
    aliases: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut providers: Vec<&str> = Vec::new();
    for model in available {
        if !providers.contains(&model.provider.as_str()) {
            providers.push(&model.provider);
        }
    }
    providers.sort_by_key(|provider| (*provider != current_provider, *provider));

    let mut lines = Vec::new();
    for provider in providers {
        lines.push(format!("**{provider}**"));
        for model in available.iter().filter(|model| model.provider == provider) {
            let mut notes = Vec::new();
            if provider == current_provider && model.id == current_model {
                notes.push("running".to_owned());
            }
            notes.extend(short_names(aliases, model));
            if let Some(level) = &model.default_level {
                notes.push(format!("thinks {}", level.trim_start_matches(':')));
            }
            let said = if notes.is_empty() {
                String::new()
            } else {
                format!("  ({})", notes.join(", "))
            };
            lines.push(format!("  `{}`{said}", model.id));
        }
    }
    lines
}

/// The short names that stand for one model, in the order they were written.
///
/// A short name spelled the same as the model teaches nobody anything, so it
/// is left out rather than shown beside the name it repeats.
fn short_names(aliases: &BTreeMap<String, String>, model: &AvailableModel) -> Vec<String> {
    let qualified = model.qualified();
    aliases
        .iter()
        .filter(|(short, target)| {
            let bare = split_level(target).0;
            (bare == qualified || bare == model.id) && **short != model.id
        })
        .map(|(short, _)| format!("`{short}`"))
        .collect()
}

impl Running {
    pub(super) async fn run_command(&mut self, word: &str, rest: &str, message: IncomingMessage) {
        self.replying_to = Some(word.to_owned());
        self.answer_command(word, rest, &message).await;
        self.replying_to = None;
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per command keeps the switch readable, as the original's switch does"
    )]
    pub(super) async fn answer_command(
        &mut self,
        word: &str,
        rest: &str,
        message: &IncomingMessage,
    ) {
        let access = COMMANDS
            .iter()
            .find(|(name, _)| *name == word)
            .map_or(CommandAccess::Owner, |(_, meta)| meta.access);
        let standing = Standing {
            is_owner: self.may_control(&message.author_id),
            is_guest: self.guests.contains(&message.author_id),
        };
        if !may_run(access, standing) {
            let why = if access == CommandAccess::Owner {
                format!(
                    "only <@{}>, who started this session, can use {word}",
                    self.options.owner_id
                )
            } else {
                self.not_invited()
            };
            self.refuse(message, &why).await;
            return;
        }

        match word {
            "!stop" => {
                self.react(&message.id, ReactionOutcome::Accepted).await;
                self.note_command(message, "!stop").await;
                self.end_because(
                    EndReason::Stopped,
                    &format!(
                        "this session ended ({})",
                        end_reason_name(EndReason::Stopped)
                    ),
                )
                .await;
            }
            "!interrupt" => {
                if self.ticket.is_none() {
                    self.say("there is nothing running to interrupt").await;
                    return;
                }
                // Asking twice is asking for the same thing. Each ask used to
                // start its own wait, and the first of them to run out force
                // stopped the session, so hurrying it along was what ended it.
                if self.abort_in_flight {
                    self.react(&message.id, ReactionOutcome::Accepted).await;
                    self.say(
                        "already interrupting; waiting for the agent to confirm. `!stop` ends the session",
                    )
                    .await;
                    return;
                }
                self.aborting = true;
                self.react(&message.id, ReactionOutcome::Accepted).await;
                self.note_command(message, "!interrupt").await;
                self.abort();
            }
            "!allow" | "!deny" => {
                let target = parse_user_id(rest);
                let Some(target) = target else {
                    self.say(&format!("say who, as `{word} @user`")).await;
                    return;
                };
                if target == self.options.owner_id {
                    self.say("the owner already takes part in their own thread")
                        .await;
                    return;
                }

                if word == "!allow" {
                    self.guests.insert(target.clone());
                } else {
                    self.guests.remove(&target);
                }
                if let Some(changed) = &self.options.on_guests_changed {
                    let list = self.guests.iter().cloned().collect::<Vec<_>>();
                    changed(&list);
                }

                self.react(&message.id, ReactionOutcome::Accepted).await;
                self.say(&if word == "!allow" {
                    format!("<@{target}> can now prompt this session and read its project")
                } else {
                    format!("<@{target}> can no longer take part in this thread")
                })
                .await;
            }
            "!guests" => {
                let guests = self.guests.iter().cloned().collect::<Vec<_>>();
                self.say(&if guests.is_empty() {
                    format!(
                        "only <@{}> takes part in this thread",
                        self.options.owner_id
                    )
                } else {
                    format!(
                        "taking part: <@{}> and {}",
                        self.options.owner_id,
                        guests
                            .iter()
                            .map(|id| format!("<@{id}>"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .await;
            }
            "!facts" => self.report_facts(rest, message).await,
            "!forget" => self.forget_facts(rest, message).await,
            "!pwd" => {
                self.say(&format!(
                    "`{}` at `{}`",
                    self.options.project.name, self.options.project.path
                ))
                .await;
            }
            "!ls" | "!cat" => self.read_path(rest).await,
            "!file" => {
                self.react(&message.id, ReactionOutcome::Accepted).await;
                self.upload_file(rest).await;
            }
            "!pr" => {
                self.note_pull_request_asked(message);
                self.open_pull_request_command(rest, message).await;
            }
            "!compact" => self.compact_conversation(message).await,
            "!model" => self.switch_model(rest, message).await,
            "!help" => self.say(&help_text()).await,
            "!then" => {
                if rest.trim().is_empty() {
                    self.say("say what to hold, as `!then <instruction>`").await;
                    return;
                }
                // With nothing running there is nothing to wait for, so it
                // starts a turn rather than being refused for asking at the
                // wrong moment.
                self.submit_prompt(rest, message.clone(), true, Attached::default())
                    .await;
            }
            "!steer" => {
                if rest.trim().is_empty() {
                    self.say("say what to steer towards, as `!steer <instruction>`")
                        .await;
                    return;
                }
                if self.ticket.is_none() {
                    self.say("there is no running turn to steer; send it as an ordinary message")
                        .await;
                    return;
                }
                self.note_command(message, &format!("!steer {rest}")).await;
                if let Some(client) = &self.client {
                    client.steer(rest, None);
                }
                self.react(&message.id, ReactionOutcome::Accepted).await;
            }
            _ => {
                let said = self.describe_status();
                self.say(&said).await;
            }
        }
    }

    /// Says which model this session runs on and which it can switch to.
    async fn list_models(&mut self) {
        let available = &self.options.catalog.models();
        let running = self
            .running_model()
            .unwrap_or_else(|| "the provider default".to_owned());
        self.say(&if available.is_empty() {
            format!("this session runs on `{running}`; the host lists no others to switch to")
        } else {
            let mut lines = vec![format!(
                "this session runs on `{running}`. Switch with `!model <name>`:"
            )];
            lines.extend(grouped_by_provider(
                available,
                &self.provider(),
                &split_level(&running).0,
                &self.options.config.agent.aliases,
            ));
            lines.join("\n")
        })
        .await;
    }

    /// Shows which models this session can run on, or moves it to one.
    pub(super) async fn switch_model(&mut self, rest: &str, message: &IncomingMessage) {
        // A short name is what somebody types here too, so it stands for the
        // same model it would have at the start of a session.
        let expanded = expand_alias(rest.trim(), &self.options.config.agent.aliases);
        // The level is taken off before the name is looked up and put back
        // after: an alias may carry one, and `musecringe:max` is not the name
        // of anything the host lists.
        let (wanted, asked_level) = split_level(&expanded);
        let available = &self.options.catalog.models();

        if wanted.is_empty() {
            self.list_models().await;
            return;
        }

        if self.ticket.is_some() {
            self.say("a turn is running; wait for it, or stop it with `!interrupt`")
                .await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        }

        // Refused rather than passed through, so a typo becomes a message
        // here instead of a turn that fails against the provider later.
        //
        // The provider comes from the model rather than from whatever the
        // session is on: a model belongs to one provider, and sending its
        // name to a different one is how switching back used to fail.
        let chosen = match choose(available, &wanted, &self.provider()) {
            Chosen::One(model) => model,
            Chosen::None => {
                self.say(&format!(
                    "this host does not list a model called `{wanted}`"
                ))
                .await;
                self.react(&message.id, ReactionOutcome::Failed).await;
                return;
            }
            Chosen::Several(options) => {
                let mut lines = vec![format!(
                    "more than one provider serves `{wanted}`. Name one of these instead:"
                )];
                lines.extend(options.iter().map(|model| format!("  {model}")));
                self.say(&lines.join("\n")).await;
                self.react(&message.id, ReactionOutcome::Failed).await;
                return;
            }
        };
        // What is remembered and shown keeps the level, because resuming
        // passes it to `--model`, which does read one. The switch itself sends
        // the bare id and the level separately, because the agent's rpc does
        // not.
        let wanted = chosen.with_level(&asked_level);
        let level = split_level(&wanted).1;
        let provider = chosen.provider;

        self.log.debug(
            "switching the model",
            &fields([
                ("provider", LogValue::from(provider.as_str())),
                ("model", LogValue::from(wanted.as_str())),
            ]),
        );
        let Some(client) = self.client.clone() else {
            self.say("the agent is not accepting anything further; this session has ended")
                .await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        };
        let switched = client
            .set_model(
                &provider,
                &chosen.id,
                self.options.config.timeouts.question_ms,
            )
            .await;
        if let Err(error) = switched {
            self.log.warn(
                "the agent refused a model switch",
                &fields([
                    ("model", LogValue::from(format!("{provider}/{}", chosen.id))),
                    ("detail", LogValue::from(error.as_str())),
                ]),
            );
            self.say(&format!(
                "this session stays on `{}`: the agent refused `{provider}/{}`: {error}",
                self.running_model()
                    .unwrap_or_else(|| "the provider default".to_owned()),
                chosen.id
            ))
            .await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        }
        if let Some(level) = level.strip_prefix(':') {
            client.set_thinking_level(level);
        }

        self.switched = Some((provider.clone(), wanted.clone()));
        if let Some(changed) = &self.options.on_model_changed {
            changed(&provider, &wanted);
        }

        self.note_command(message, &format!("!model {wanted}"))
            .await;
        self.say(&connection_line(&format!(
            "this session now runs on `{wanted}`, keeping what was said"
        )))
        .await;
        self.react(&message.id, ReactionOutcome::Accepted).await;
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "token totals sit far below f64's exact range"
    )]
    pub(super) fn describe_status(&self) -> String {
        let mut lines = vec![
            format!("project: {}", self.options.project.name),
            format!(
                "state: {}",
                if self.ticket.is_some() {
                    "running a turn"
                } else {
                    "idle"
                }
            ),
            format!(
                "turns in flight across all sessions: {}",
                self.options.scheduler.turns_in_flight()
            ),
            format!("prompts waiting: {}", self.options.scheduler.queue_length()),
        ];

        if self.delegated_asked > 0 {
            lines.push(format!(
                "delegated: {} of {} asked, {} token(s) spent, {} kept out of this conversation",
                self.delegated_answered,
                self.delegated_asked,
                self.delegated_tokens,
                byte_count(self.delegated_kept_out as f64),
            ));
        }
        lines.join("\n")
    }

    /// Aborts the running turn, force stopping if the agent will not confirm.
    pub(super) fn abort(&mut self) {
        self.abort_in_flight = true;
        if let Some(client) = &self.client {
            client.abort();
        }
        self.abort_timer = Some(self.timers.set_timeout(
            SessionTimer::AbortDeadline,
            self.options.config.timeouts.abort_ms,
        ));
    }

    /// Records a command that changed the agent's course.
    pub(super) async fn note_command(&self, message: &IncomingMessage, text: &str) {
        self.note_prompt(&display_name(message), text, None).await;
    }

    /// Summarises the conversation so far, freeing context to carry on in.
    ///
    /// Refused while a turn is running: compacting underneath a turn would
    /// change the conversation the agent is part way through answering about.
    pub(super) async fn compact_conversation(&mut self, message: &IncomingMessage) {
        if self.ticket.is_some() {
            self.say("a turn is running; wait for it, or stop it with `!interrupt`")
                .await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        }

        let Some(client) = self.client.clone() else {
            self.say("this session has no agent to compact").await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        };

        self.react(&message.id, ReactionOutcome::Accepted).await;
        self.note_command(message, "!compact").await;
        match client
            .compact(self.options.config.timeouts.question_ms)
            .await
        {
            Ok(answer) => {
                self.say(&compaction_line(&answer)).await;
                self.react(&message.id, ReactionOutcome::Succeeded).await;
            }
            Err(error) => {
                self.log.warn(
                    "compaction failed",
                    &fields([("detail", LogValue::from(error.clone()))]),
                );
                self.say(&format!("compaction did not finish: {error}"))
                    .await;
                self.react(&message.id, ReactionOutcome::Failed).await;
            }
        }
    }

    /// Turns a message down, explaining the first time and reacting every
    /// time.
    ///
    /// Answered as a reply rather than said: a refusal is addressed to the
    /// person who tripped it, not to the session. Posting it would record it
    /// and show it in an interface as though the agent had said it, which is
    /// both untrue and noise in a conversation the refused message never
    /// joined.
    pub(super) async fn refuse(&mut self, message: &IncomingMessage, why: &str) {
        if !self.explained.contains(&message.author_id) {
            self.explained.insert(message.author_id.clone());
            self.views
                .send(SessionEvent::Reply {
                    text: why.to_owned(),
                    command: "refused".to_owned(),
                })
                .await;
        }
        self.react(&message.id, ReactionOutcome::Failed).await;
    }
}
