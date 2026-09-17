//! What to do with a message that arrived.
//!
//! A pure function over a minimal message shape, so the rule deciding whether
//! a message is acted on can be tested exhaustively without a connection. The
//! transport that feeds it is the only part that touches the chat library.

use serde::{Deserialize, Serialize};

use crate::config::schema::{ALLOW_EVERY_USER, ChatConfig};
use crate::session::commands::is_addressed_to_bot;

/// A file attached to a message, as the service describes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawAttachment {
    /// The service's own id for the file.
    pub id: String,
    /// The name the sender's file had. Never used as a path without checking.
    pub name: String,
    /// Where to fetch it from, which the service signs and expires.
    pub url: String,
    /// How large the service says it is, in bytes.
    pub size: u64,
    /// What the service believes it is, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

/// The only facts about a message the filter needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawMessage {
    /// The service's own id for the message.
    pub id: String,
    /// Who posted it.
    pub author_id: String,
    /// Display name, when the service gave one. Used only to address somebody.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    #[serde(default)]
    /// Whether the service marks the author as automated.
    pub author_is_bot: bool,
    /// The channel or thread the message was posted in.
    pub channel_id: String,
    /// For a thread, the channel it hangs off. Otherwise none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_channel_id: Option<String>,
    #[serde(default)]
    /// What was said, which may be empty when only files were sent.
    pub content: String,
    /// Files attached to it, in the order they were attached.
    #[serde(default)]
    pub attachments: Vec<RawAttachment>,
}

/// A deletion, which carries far less than the message did.
///
/// Discord reports a deletion by id. When the message is older than the
/// cache, that is nearly all it reports: no author, no text, and nothing to
/// decide an allowlist on. Where it was said is still known, which is enough
/// to tell a deletion in the served channel from one anywhere else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawDeletion {
    /// The message that was deleted.
    pub id: String,
    /// The channel or thread it was deleted from.
    pub channel_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// For a thread, the channel it hangs off. Otherwise none.
    pub parent_channel_id: Option<String>,
}

/// What should be done with an inbound message.
#[derive(Debug, Clone, PartialEq)]
pub enum InboundDecision {
    /// Do nothing at all, and send no reply.
    Ignore {
        /// Why, in words that never name an account or a list.
        reason: &'static str,
    },
    /// A top-level message in the served channel: open a thread and a session.
    Start,
    /// A reply inside a thread hanging off the served channel.
    Thread { thread_id: String },
}

/// What to do about a deletion.
#[derive(Debug, Clone, PartialEq)]
pub enum DeletionDecision {
    /// Not one of ours, wherever it was said.
    Ignore { reason: &'static str },
    /// A message in the served channel or one of its threads was taken back.
    Withdraw {
        message_id: String,
        /// The thread it was said in, or none for the channel itself.
        thread_id: Option<String>,
    },
}

/// Whether a message addresses the bot by name.
///
/// Both spellings, because a client sends one and somebody typing by hand may
/// produce the other. The mention has to be there; where it is does not
/// matter, since people write "@errand look at this" and "look at this
/// @errand" in equal measure.
pub fn mentions_bot(content: &str, bot_id: &str) -> bool {
    let both = [format!("<@{bot_id}>"), format!("<@!{bot_id}>")];
    both.iter()
        .any(|mention| content.contains(mention.as_str()))
}

/// The message without the mention that summoned the bot.
///
/// Every mention of it, not only the first: what is left is the prompt, and a
/// prompt that still says `<@1523363748427993218>` reads as noise to a model
/// and can be mistaken for a project name when it leads the line.
pub fn without_bot_mention(content: &str, bot_id: &str) -> String {
    let stripped = content
        .replace(&format!("<@!{bot_id}>"), " ")
        .replace(&format!("<@{bot_id}>"), " ");
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether an account is refused whatever else the configuration says.
///
/// Ahead of the allowlist and of any session role, so excluding somebody is
/// one decision rather than an audit of every list they might appear on.
pub fn is_blocked(config: &ChatConfig, user_id: &str) -> bool {
    config
        .blocked_user_ids
        .iter()
        .any(|blocked| blocked == user_id)
}

/// Whether an account may drive sessions at all.
///
/// Blocking is checked first, so an account on both lists is refused.
pub fn is_permitted(config: &ChatConfig, user_id: &str) -> bool {
    if is_blocked(config, user_id) {
        return false;
    }
    config
        .allowed_user_ids
        .iter()
        .any(|allowed| allowed == ALLOW_EVERY_USER || allowed == user_id)
}

/// Decides whether a deletion is one of ours.
///
/// Deliberately not [`classify`]: that refuses a message it cannot attribute,
/// and a deletion usually cannot be attributed. Whoever was allowed to say it
/// was already judged when they said it, and a withdrawal only ever removes
/// what is already there, so there is nothing here an allowlist would protect.
pub fn classify_deletion(deletion: &RawDeletion, config: &ChatConfig) -> DeletionDecision {
    let in_served_channel = deletion.channel_id == config.channel_id;
    let in_served_thread =
        deletion.parent_channel_id.as_deref() == Some(config.channel_id.as_str());
    if !in_served_channel && !in_served_thread {
        return DeletionDecision::Ignore {
            reason: "outside the served channel",
        };
    }
    DeletionDecision::Withdraw {
        message_id: deletion.id.clone(),
        thread_id: if in_served_thread {
            Some(deletion.channel_id.clone())
        } else {
            None
        },
    }
}

/// Decides what to do with a message.
///
/// Every rejection is silent, and no reason names an account or a list.
/// Telling an unauthorised sender why they were ignored describes the
/// allowlist to exactly the person it exists to exclude.
pub fn classify(
    message: &RawMessage,
    config: &ChatConfig,
    bot_id: Option<&str>,
) -> InboundDecision {
    let in_served_channel = message.channel_id == config.channel_id;
    let in_served_thread = message.parent_channel_id.as_deref() == Some(config.channel_id.as_str());

    if message.author_is_bot {
        // Never its own, whatever else is true: a daemon that answered itself
        // would keep answering.
        if let Some(bot_id) = bot_id
            && message.author_id == bot_id
        {
            return InboundDecision::Ignore {
                reason: "its own message",
            };
        }
        // Elsewhere a bot is ignored, so nothing automated can start a
        // session or talk in the channel. Inside a thread it is let through,
        // because a thread has an owner who decides who takes part: the
        // session refuses anyone they have not invited with `!allow`, and a
        // bot is not a special case of that.
        if !in_served_thread {
            return InboundDecision::Ignore {
                reason: "authored by a bot",
            };
        }
    }
    if !in_served_channel && !in_served_thread {
        return InboundDecision::Ignore {
            reason: "outside the served channel",
        };
    }

    if is_blocked(config, &message.author_id) {
        return InboundDecision::Ignore {
            reason: "the author is blocked",
        };
    }
    if !is_permitted(config, &message.author_id) {
        return InboundDecision::Ignore {
            reason: "the author is not permitted",
        };
    }

    if message.content.trim().is_empty() && message.attachments.is_empty() {
        return InboundDecision::Ignore {
            reason: "nothing was said and nothing was attached",
        };
    }

    if in_served_thread {
        return InboundDecision::Thread {
            thread_id: message.channel_id.clone(),
        };
    }

    // Only for starting something. Inside a thread the session is already the
    // conversation, so making every message name the bot would be tiresome.
    //
    // A message beginning with `!` is already addressed to a bot, so it is let
    // through whatever the setting says: `!usage` and `!help` are answered
    // without a session, and asking somebody to name the bot as well only
    // makes them vanish. It cannot start a session either way, since the
    // daemon starts nothing for a message addressed to a bot.
    if config.start_on_mention && !is_addressed_to_bot(&message.content) {
        let Some(bot_id) = bot_id else {
            return InboundDecision::Ignore {
                reason: "the bot does not know its own name yet",
            };
        };
        if !mentions_bot(&message.content, bot_id) {
            return InboundDecision::Ignore {
                reason: "the channel message did not mention the bot",
            };
        }
    }

    InboundDecision::Start
}

#[cfg(test)]
#[path = "inbound/tests.rs"]
mod tests;
