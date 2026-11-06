//! The connection: logging in, and turning library objects into the plain
//! shapes the rest of the daemon works with.
//!
//! Whether a message is acted on is decided in `inbound`, which is a pure
//! function and testable without a connection. This is the transport under
//! it, and the one place that touches the chat library's types.
//!
//! The chat library keeps the socket alive itself: it resumes after every
//! drop and re-sits a dead shard on a fixed five second pace, so errand
//! carries no backoff schedule of its own. What it still owns is the give-up:
//! a connection that stays away too long ends the daemon, because sessions
//! left running unattended are worse than none. The wait is timed rather
//! than counted, and [`GIVE_UP_AFTER_MS`] is how long.
//!
//! The wait starts from what the library reports, and it does not report
//! everything: a shard whose heartbeats go unanswered is replaced without a
//! stage event, so an outage that begins that way is timed only from when
//! the library next says where the shard is.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serenity::client::{Context, EventHandler};
use serenity::model::application::Interaction;
use serenity::model::channel::{Attachment, Channel, Message};
use serenity::model::gateway::Ready;
use serenity::model::id::ChannelId;
use serenity::model::sticker::{StickerFormatType, StickerItem};

use crate::chat::commands::{TranslatedCommand, acknowledge, answer_deferred, defer, translate};
use crate::chat::inbound::DeletionDecision;
use crate::chat::inbound::{
    InboundDecision, RawAttachment, RawDeletion, RawMessage, classify, classify_deletion,
    is_permitted, without_bot_mention,
};
use crate::config::schema::ChatConfig;
use crate::log::{LogValue, Logger, fields};

/// How long the connection may be away before the daemon gives up, because a
/// sandbox nobody can reach or stop is worse than none.
pub const GIVE_UP_AFTER_MS: u64 = 60_000;

/// How often the give-up timer is checked.
pub const GIVE_UP_POLL_MS: u64 = 5_000;

/// Whether the connection is up, and since when it has been what it is.
///
/// The `since` is when the current stretch began: a connection that came up
/// and stayed up is measured from when it came up, and one that went down
/// and is being worked on is measured from when it went down, whatever the
/// library does between its attempts.
#[derive(Debug)]
struct ConnectionState {
    up: bool,
    since: Instant,
}

/// Whether a failure to connect is one that serenity's own retrying could get
/// past.
///
/// Everything is, apart from what the service has already decided about this
/// bot: a rejected token and refused intents are answers, not outages, and
/// they will be the same answer in a minute. They arrive as the gateway
/// errors named here, rather than as the strings the TypeScript port matched.
pub fn is_permanent(error: &serenity::Error) -> bool {
    matches!(
        error,
        serenity::Error::Gateway(
            serenity::gateway::GatewayError::InvalidAuthentication
                | serenity::gateway::GatewayError::InvalidGatewayIntents
                | serenity::gateway::GatewayError::DisallowedGatewayIntents,
        )
    )
}

/// Reduces a library message to the facts the filter needs.
///
/// `parent_channel_id` is the channel a thread hangs off, which the message
/// itself does not carry; the caller reads it from the cache, falling back to
/// REST, so routing never depends on the cache being warm.
///
/// An author is always present on a gateway message, so unlike the TypeScript
/// port this refuses nothing: the authorless-partial case it guarded against
/// does not exist in this library.
pub fn to_raw(message: &Message, parent_channel_id: Option<ChannelId>) -> RawMessage {
    let mut attachments: Vec<RawAttachment> =
        message.attachments.iter().map(attachment_of).collect();
    attachments.extend(message.sticker_items.iter().filter_map(sticker_attachment));
    RawMessage {
        id: message.id.get().to_string(),
        author_id: message.author.id.get().to_string(),
        author_name: message
            .author
            .global_name
            .clone()
            .or_else(|| Some(message.author.name.clone())),
        author_is_bot: message.author.bot,
        channel_id: message.channel_id.get().to_string(),
        parent_channel_id: parent_channel_id.map(|channel| channel.get().to_string()),
        content: message.content.clone(),
        attachments,
    }
}

/// Reduces a library attachment to the facts the session needs.
fn attachment_of(file: &Attachment) -> RawAttachment {
    RawAttachment {
        id: file.id.get().to_string(),
        name: file.filename.clone(),
        url: file.url.clone(),
        size: u64::from(file.size),
        content_type: file.content_type.clone(),
    }
}

/// Reduces a sticker to an attachment, so a sticker rides the same path a
/// file does: fetched, saved under the project, and shown to the model.
///
/// None when Discord names no fetchable format for it. The size is unknown
/// until the fetch, which is where the limit is checked against the bytes
/// that actually arrived.
fn sticker_attachment(item: &StickerItem) -> Option<RawAttachment> {
    let url = item.image_url()?;
    let (extension, content_type) = match item.format_type {
        StickerFormatType::Gif => ("gif", "image/gif"),
        StickerFormatType::Lottie => ("json", "application/json"),
        _ => ("png", "image/png"),
    };
    Some(RawAttachment {
        id: item.id.get().to_string(),
        name: format!("{}.{extension}", item.name),
        url,
        size: 0,
        content_type: Some(content_type.to_owned()),
    })
}

#[cfg(test)]
mod tests;

/// One callback on the daemon, from the gateway.
pub type OnMessage = Arc<dyn Fn(RawMessage, InboundDecision) + Send + Sync>;

/// One callback on the daemon, from the gateway.
pub type OnCommand = Arc<dyn Fn(TranslatedCommand, Arc<dyn Fn(&str) + Send + Sync>) + Send + Sync>;

/// What the gateway reports to the daemon.
///
/// Every callback receives plain shapes; the chat library's types stop here.
/// The names carry the same `on` prefix the original's handlers do, because
/// they are read as a table rather than as individual fields.
#[expect(clippy::struct_field_names)]
pub struct GatewayHandlers {
    /// A message was said in the served channel or one of its threads.
    pub on_message: OnMessage,
    /// A slash command was used, already translated to its text form. The
    /// second callback acknowledges the interaction privately, so the service
    /// does not report the command as failed.
    pub on_command: OnCommand,
    /// A thread bound to a session was archived or deleted from outside.
    pub on_thread_closed: Arc<dyn Fn(String) + Send + Sync>,
    /// A message in the served channel or one of its threads was deleted.
    pub on_withdrawn: Arc<dyn Fn(String, Option<String>) + Send + Sync>,
    /// The connection came up.
    pub on_connected: Arc<dyn Fn() + Send + Sync>,
    /// The connection went down; serenity is already re-sitting it.
    pub on_disconnected: Arc<dyn Fn() + Send + Sync>,
    /// The connection has been away too long; the daemon gives up.
    pub on_gave_up: Arc<dyn Fn() + Send + Sync>,
}

/// Owns the single connection and dispatches filtered messages.
pub struct Gateway {
    config: ChatConfig,
    handlers: Arc<GatewayHandlers>,
    log: Logger,
    connection: std::sync::Mutex<ConnectionState>,
    /// The bot's own account id, once the service has said.
    bot_id: std::sync::Mutex<Option<String>>,
    /// The shard messenger, captured on ready, for presence updates.
    shard: std::sync::Mutex<Option<serenity::gateway::ShardMessenger>>,
    /// Signalled once the connection is ready to use.
    ready: tokio::sync::watch::Receiver<bool>,
    /// The other end of `ready`; held so the channel cannot die first.
    ready_sender: tokio::sync::watch::Sender<bool>,
}

impl Gateway {
    /// A gateway on the served channel, reporting through `handlers`.
    pub fn new(config: ChatConfig, handlers: GatewayHandlers, log: Logger) -> Arc<Self> {
        let (ready_sender, ready) = tokio::sync::watch::channel(false);
        Arc::new(Self {
            config,
            handlers: Arc::new(handlers),
            log,
            connection: std::sync::Mutex::new(ConnectionState {
                up: false,
                since: Instant::now(),
            }),
            bot_id: std::sync::Mutex::new(None),
            shard: std::sync::Mutex::new(None),
            ready,
            ready_sender,
        })
    }

    /// Gives up when the connection has been away too long, reporting it
    /// once through the `on_gave_up` handler.
    ///
    /// The chat library works through a drop on its own and can go silent
    /// for whole stretches of one outage, so the wait is timed rather than
    /// counted: however many attempts it makes, what matters is how long
    /// nobody has been home.
    pub async fn watch_connection(&self, after: Duration, every: Duration) {
        loop {
            tokio::time::sleep(every).await;
            let away = {
                let state = self.connection.lock().expect("the connection lock");
                if state.up {
                    continue;
                }
                state.since.elapsed()
            };
            if away >= after {
                let away_ms = u64::try_from(away.as_millis()).unwrap_or(u64::MAX);
                self.log.error(
                    "giving up on the connection",
                    &fields([("awayMs", LogValue::from(away_ms))]),
                );
                (self.handlers.on_gave_up)();
                return;
            }
        }
    }

    /// Waits for the connection to answer its handshake, no longer than the
    /// timeout given.
    pub async fn wait_ready(&self, timeout_ms: u64) -> Result<(), String> {
        let mut ready = self.ready.clone();
        let wait = async {
            loop {
                if *ready.borrow() {
                    return Ok(());
                }
                if ready.changed().await.is_err() {
                    return Err("the connection was closed before it was ready".to_owned());
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), wait)
            .await
            .map_err(|_| format!("the connection was not ready in {timeout_ms}ms"))?
    }

    /// The intents the daemon needs, no more than that.
    pub fn intents() -> serenity::model::gateway::GatewayIntents {
        serenity::model::gateway::GatewayIntents::GUILDS
            | serenity::model::gateway::GatewayIntents::GUILD_MESSAGES
            | serenity::model::gateway::GatewayIntents::MESSAGE_CONTENT
    }

    /// Whether a login failure is one no amount of retrying fixes, in which
    /// case the daemon says what to change rather than waiting.
    pub fn login_is_permanent(error: &serenity::Error) -> bool {
        is_permanent(error)
    }
}

#[serenity::async_trait]
impl EventHandler for Gateway {
    async fn message(&self, ctx: Context, message: Message) {
        let parent = parent_of(&ctx, message.channel_id).await;
        let raw = to_raw(&message, parent);
        let own = self.bot_id.lock().expect("the bot id lock").clone();
        let decision = classify(&raw, &self.config, own.as_deref());
        let InboundDecision::Ignore { reason } = &decision else {
            self.log.debug(
                "accepted a message",
                &fields([
                    ("decision", LogValue::from(format!("{decision:?}"))),
                    ("message", LogValue::from(raw.id.as_str())),
                    ("user", LogValue::from(raw.author_id.as_str())),
                ]),
            );
            // The mention summoned the bot; it is not part of what was asked.
            // Taken out here, where the bot's own name is known, so nothing
            // downstream has to know it has one.
            let asked = if decision == InboundDecision::Start
                && self.config.start_on_mention
                && let Some(own) = &own
            {
                let mut without = raw.clone();
                without.content = without_bot_mention(&raw.content, own);
                without
            } else {
                raw
            };
            (self.handlers.on_message)(asked, decision);
            return;
        };
        self.log.info(
            "ignored a message",
            &fields([("reason", LogValue::from(*reason))]),
        );
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Some(command) = interaction.as_command().cloned() else {
            return;
        };

        // The same rule governs slash commands as messages. A blocked account
        // is answered exactly as an unauthorised one, so the two are
        // indistinguishable from outside. An interaction has to be answered
        // at all, or the service reports the bot as broken.
        if !is_permitted(&self.config, command.user.id.get().to_string().as_str()) {
            acknowledge(&ctx, &command, "you are not permitted to use this bot").await;
            return;
        }

        let is_thread = command.channel.as_ref().is_some_and(|channel| {
            matches!(
                channel.kind,
                serenity::model::channel::ChannelType::PublicThread
                    | serenity::model::channel::ChannelType::PrivateThread
                    | serenity::model::channel::ChannelType::NewsThread
            )
        });
        let user_name = command
            .user
            .global_name
            .clone()
            .or_else(|| Some(command.user.name.clone()))
            .unwrap_or_default();
        let translated = translate(
            &command.data.name,
            &command.data.options,
            command.channel_id.get().to_string().as_str(),
            is_thread,
            command.user.id.get().to_string().as_str(),
            user_name.as_str(),
        );
        // Claimed before the command runs. The daemon may take longer than
        // the service allows an unanswered interaction, and it is the claim
        // that keeps a slow answer from looking like a broken bot. The
        // permission refusal above is the one answer sent directly, since it
        // is decided here and needs no waiting on.
        defer(&ctx, &command).await;
        let ack: Arc<dyn Fn(&str) + Send + Sync> = {
            let ctx = ctx.clone();
            Arc::new(move |text: &str| {
                // Fire and forget: a failure to answer is a service report of
                // failure, which is what it already is.
                let ctx = ctx.clone();
                let command = command.clone();
                let text = text.to_owned();
                tokio::spawn(async move {
                    answer_deferred(&ctx, &command, &text).await;
                });
            })
        };
        (self.handlers.on_command)(translated, ack);
    }

    async fn message_delete(
        &self,
        ctx: Context,
        channel_id: ChannelId,
        deleted_message_id: serenity::model::id::MessageId,
        _guild_id: Option<serenity::model::id::GuildId>,
    ) {
        // A deletion arrives by id, and for anything older than the cache
        // that is nearly all it carries. `to_raw` is no use here: it is for
        // messages, and a deletion usually cannot be attributed.
        let parent = parent_of(&ctx, channel_id).await;
        let decision = classify_deletion(
            &RawDeletion {
                id: deleted_message_id.get().to_string(),
                channel_id: channel_id.get().to_string(),
                parent_channel_id: parent.map(|channel| channel.get().to_string()),
            },
            &self.config,
        );
        match decision {
            DeletionDecision::Ignore { .. } => {}
            DeletionDecision::Withdraw {
                message_id,
                thread_id,
            } => {
                self.log.info(
                    "a message was withdrawn",
                    &fields([
                        ("messageId", LogValue::from(message_id.as_str())),
                        (
                            "threadId",
                            LogValue::from(thread_id.clone().unwrap_or_default()),
                        ),
                    ]),
                );
                (self.handlers.on_withdrawn)(message_id, thread_id);
            }
        }
    }

    async fn thread_update(
        &self,
        _ctx: Context,
        _old: Option<serenity::model::channel::GuildChannel>,
        new: serenity::model::channel::GuildChannel,
    ) {
        if new.parent_id != Some(ChannelId::new(self.config.channel_id.parse().unwrap_or(0))) {
            return;
        }
        let archived = new
            .thread_metadata
            .as_ref()
            .is_some_and(|metadata| metadata.archived);
        if !archived {
            return;
        }
        (self.handlers.on_thread_closed)(new.id.get().to_string());
    }

    async fn thread_delete(
        &self,
        _ctx: Context,
        thread: serenity::model::channel::PartialGuildChannel,
        _full: Option<serenity::model::channel::GuildChannel>,
    ) {
        if thread.parent_id.get().to_string() != self.config.channel_id {
            return;
        }
        (self.handlers.on_thread_closed)(thread.id.get().to_string());
    }

    async fn shard_stage_update(
        &self,
        _ctx: Context,
        event: serenity::gateway::ShardStageUpdateEvent,
    ) {
        self.stage_changed(event.new);
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        *self.bot_id.lock().expect("the bot id lock") = Some(ready.user.id.get().to_string());
        *self.shard.lock().expect("the shard lock") = Some(ctx.shard);
        self.ready_sender.send_replace(true);
    }
}

/// The channel a thread hangs off, from the cache with a REST fallback, so
/// routing never depends on the cache being warm.
async fn parent_of(ctx: &Context, channel_id: ChannelId) -> Option<ChannelId> {
    match channel_id.to_channel(ctx).await {
        // Threads are guild channels with thread metadata, whose `parent_id`
        // is the channel they hang off.
        Ok(Channel::Guild(channel)) => channel.parent_id,
        _ => None,
    }
}

impl Gateway {
    /// Applies a shard's stage transition to the connection's state.
    ///
    /// The library never reports the shard as `Disconnected`: it works a
    /// drop through `Resuming` and `Connecting` and goes silent while the
    /// manager replaces a hopeless shard, so every stage that is not
    /// `Connected` means the connection is away. The stretch is measured
    /// from the first such landing, and the notice rides on the fall from a
    /// working connection alone, since a retry dying is the outage going on
    /// rather than it starting.
    fn stage_changed(&self, new: serenity::gateway::ConnectionStage) {
        use serenity::gateway::ConnectionStage;
        if new == ConnectionStage::Connected {
            let mut state = self.connection.lock().expect("the connection lock");
            state.up = true;
            state.since = Instant::now();
            drop(state);
            (self.handlers.on_connected)();
            return;
        }
        let fell = {
            let mut state = self.connection.lock().expect("the connection lock");
            let fell = state.up;
            state.up = false;
            if fell {
                state.since = Instant::now();
            }
            fell
        };
        if fell {
            (self.handlers.on_disconnected)();
        }
    }

    /// Sets the line under the bot's name, or clears it when given nothing.
    ///
    /// Custom is the only activity type that shows the text alone, with no
    /// verb in front of it; the name the API requires is not displayed, which
    /// `ActivityData::custom` fills for us.
    ///
    /// Never fails the caller: this is decoration on a connection that may be
    /// down, and a status that failed to set is not a reason to fail the
    /// thing that asked.
    pub fn set_status(&self, text: Option<&str>) {
        let Some(shard) = self.shard.lock().expect("the shard lock").clone() else {
            return;
        };
        let activity = text.map(serenity::gateway::ActivityData::custom);
        shard.set_presence(activity, serenity::model::user::OnlineStatus::Online);
    }
}
