/**
 * What to do with a message that arrived.
 *
 * A pure function over a minimal message shape, so the rule deciding whether a
 * message is acted on can be tested exhaustively without a connection. The
 * transport that feeds it is the only part that touches the chat library.
 */

import { ALLOW_EVERY_USER, type ChatConfig } from "../config/schema.ts";

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
  if (message.authorIsBot) return { kind: "ignore", reason: "authored by a bot" };

  const inServedChannel = message.channelId === config.channelId;
  const inServedThread = message.parentChannelId === config.channelId;
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
  if (config.startOnMention) {
    if (botId === undefined) {
      return { kind: "ignore", reason: "the bot does not know its own name yet" };
    }
    if (!mentionsBot(message.content, botId)) {
      return { kind: "ignore", reason: "the channel message did not mention the bot" };
    }
  }

  return { kind: "start" };
}
