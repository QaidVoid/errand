/**
 * What to do with a message that arrived.
 *
 * A pure function over a minimal message shape, so the rule deciding whether a
 * message is acted on can be tested exhaustively without a connection. The
 * transport that feeds it is the only part that touches the chat library.
 */

import { ALLOW_EVERY_USER, type ChatConfig } from "../config/schema.ts";
import { isAddressedToBot } from "../session/commands.ts";

/** A file attached to a message, as the service describes it. */
export interface RawAttachment {
  id: string;
  /** The name the sender's file had. Never used as a path without checking. */
  name: string;
  /** Where to fetch it from, which the service signs and expires. */
  url: string;
  size: number;
  /** What the service believes it is, when it says. */
  contentType: string | undefined;
}

/** The only facts about a message the filter needs. */
export interface RawMessage {
  id: string;
  authorId: string;
  /** Display name, when the service gave one. Used only to address somebody. */
  authorName: string | undefined;
  authorIsBot: boolean;
  /** The channel or thread the message was posted in. */
  channelId: string;
  /** For a thread, the channel it hangs off. Otherwise undefined. */
  parentChannelId: string | undefined;
  content: string;
  /** Files attached to it, in the order they were attached. */
  attachments: RawAttachment[];
}

/**
 * Whether a message addresses the bot by name.
 *
 * Both spellings, because a client sends one and somebody typing by hand may
 * produce the other. The mention has to be there; where it is does not matter,
 * since people write "@errand look at this" and "look at this @errand" in
 * equal measure.
 */
export function mentionsBot(content: string, botId: string): boolean {
  return new RegExp(`<@!?${botId}>`).test(content);
}

/**
 * The message without the mention that summoned the bot.
 *
 * Every mention of it, not only the first: what is left is the prompt, and a
 * prompt that still says `<@1523363748427993218>` reads as noise to a model
 * and can be mistaken for a project name when it leads the line.
 */
export function withoutBotMention(content: string, botId: string): string {
  return content.replace(new RegExp(`<@!?${botId}>`, "g"), " ").replace(/\s+/g, " ").trim();
}

/** What should be done with an inbound message. */
export type InboundDecision =
  /** Do nothing at all, and send no reply. */
  | { kind: "ignore"; reason: string }
  /** A top-level message in the served channel: open a thread and a session. */
  | { kind: "start" }
  /** A reply inside a thread hanging off the served channel. */
  | { kind: "thread"; threadId: string };

/**
 * Whether an account is refused whatever else the configuration says.
 *
 * Ahead of the allowlist and of any session role, so excluding somebody is one
 * decision rather than an audit of every list they might appear on.
 */
export function isBlocked(config: ChatConfig, userId: string): boolean {
  return config.blockedUserIds.includes(userId);
}

/**
 * Whether an account may drive sessions at all.
 *
 * Blocking is checked first, so an account on both lists is refused.
 */
export function isPermitted(config: ChatConfig, userId: string): boolean {
  if (isBlocked(config, userId)) return false;
  return config.allowedUserIds.includes(ALLOW_EVERY_USER) ||
    config.allowedUserIds.includes(userId);
}

/**
 * A deletion, which carries far less than the message did.
 *
 * Discord reports a deletion by id. When the message is older than the cache,
 * that is nearly all it reports: no author, no text, and nothing to decide an
 * allowlist on. Where it was said is still known, which is enough to tell a
 * deletion in the served channel from one anywhere else.
 */
export interface RawDeletion {
  id: string;
  channelId: string;
  parentChannelId: string | undefined;
}

/** What to do about a deletion. */
export type DeletionDecision =
  | { kind: "ignore"; reason: string }
  | { kind: "withdraw"; messageId: string; threadId: string | undefined };

/**
 * Decides whether a deletion is one of ours.
 *
 * Deliberately not `classify`: that refuses a message it cannot attribute, and
 * a deletion usually cannot be attributed. Whoever was allowed to say it was
 * already judged when they said it, and a withdrawal only ever removes what is
 * already there, so there is nothing here an allowlist would protect.
 */
export function classifyDeletion(
  deletion: RawDeletion,
  config: ChatConfig,
): DeletionDecision {
  const inServedChannel = deletion.channelId === config.channelId;
  const inServedThread = deletion.parentChannelId === config.channelId;
  if (!inServedChannel && !inServedThread) {
    return { kind: "ignore", reason: "outside the served channel" };
  }
  return {
    kind: "withdraw",
    messageId: deletion.id,
    threadId: inServedThread ? deletion.channelId : undefined,
  };
}

/**
 * Decides what to do with a message.
 *
 * Every rejection is silent, and no reason names an account or a list. Telling
 * an unauthorised sender why they were ignored describes the allowlist to
 * exactly the person it exists to exclude.
 */
export function classify(
  message: RawMessage,
  config: ChatConfig,
  botId?: string,
): InboundDecision {
  const inServedChannel = message.channelId === config.channelId;
  const inServedThread = message.parentChannelId === config.channelId;

  if (message.authorIsBot) {
    // Never its own, whatever else is true: a daemon that answered itself
    // would keep answering.
    if (botId !== undefined && message.authorId === botId) {
      return { kind: "ignore", reason: "its own message" };
    }
    // Elsewhere a bot is ignored, so nothing automated can start a session or
    // talk in the channel. Inside a thread it is let through, because a thread
    // has an owner who decides who takes part: the session refuses anyone they
    // have not invited with `!allow`, and a bot is not a special case of that.
    if (!inServedThread) {
      return { kind: "ignore", reason: "authored by a bot" };
    }
  }
  if (!inServedChannel && !inServedThread) {
    return { kind: "ignore", reason: "outside the served channel" };
  }

  if (isBlocked(config, message.authorId)) {
    return { kind: "ignore", reason: "the author is blocked" };
  }
  if (!isPermitted(config, message.authorId)) {
    return { kind: "ignore", reason: "the author is not permitted" };
  }

  if (message.content.trim().length === 0 && message.attachments.length === 0) {
    return { kind: "ignore", reason: "nothing was said and nothing was attached" };
  }

  if (inServedThread) return { kind: "thread", threadId: message.channelId };

  // Only for starting something. Inside a thread the session is already the
  // conversation, so making every message name the bot would be tiresome.
  //
  // A message beginning with `!` is already addressed to a bot, so it is let
  // through whatever the setting says: `!usage` and `!help` are answered
  // without a session, and asking somebody to name the bot as well only makes
  // them vanish. It cannot start a session either way, since the daemon starts
  // nothing for a message addressed to a bot.
  if (config.startOnMention && !isAddressedToBot(message.content)) {
    if (botId === undefined) {
      return { kind: "ignore", reason: "the bot does not know its own name yet" };
    }
    if (!mentionsBot(message.content, botId)) {
      return { kind: "ignore", reason: "the channel message did not mention the bot" };
    }
  }

  return { kind: "start" };
}
