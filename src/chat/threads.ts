/**
 * Thread creation, output, and the one case where a thread is closed.
 *
 * The only place that turns chat library objects into the {@link ThreadPort} a
 * session speaks to, which is what keeps the session layer free of library
 * types and testable without a connection.
 */

import { Buffer } from "node:buffer";
import {
  AttachmentBuilder,
  ChannelType,
  type Client,
  type Message,
  MessageFlags,
  type ThreadChannel,
} from "discord.js";
import type { Logger } from "../log.ts";
import type {
  Delegated,
  EndReason,
  ReactionOutcome,
  ThreadPort,
  ToolResult,
} from "../session/port.ts";
import type { ThreadFactory } from "../session/manager.ts";
import type { IncomingMessage } from "../session/session.ts";
import { reaction } from "./chars.ts";
import { renderDiff } from "./diff.ts";
import { Outbox } from "./outbox.ts";
import { delegationLine, MESSAGE_LIMIT, splitMessage } from "./render.ts";

/**
 * Message options for everything this system sends.
 *
 * No message gets a link preview. A coding agent quotes URLs constantly, in
 * commands and in its own prose, and a preview card for each one buries the
 * conversation. This is the only way messages are built, so a new call site
 * cannot reintroduce embeds by forgetting a flag.
 */
export function plain(content: string): {
  content: string;
  flags: MessageFlags.SuppressEmbeds;
} {
  return { content, flags: MessageFlags.SuppressEmbeds };
}

/** How long the service keeps a thread open with no activity, in minutes. */
const AUTO_ARCHIVE_MINUTES = 1440;

/** The service expires a typing indicator after about ten seconds. */
const TYPING_REFRESH_MS = 8_000;

/**
 * A thread a session posts to.
 *
 * Reactions are replaced rather than accumulated: a scrolled-back thread
 * should read as final state, not as a history of transitions.
 */
export class ChatThread implements ThreadPort {
  private readonly outbox: Outbox;
  private waitingMessageId: string | null = null;
  private readonly reactions = new Map<string, string>();
  private typingTimer: ReturnType<typeof setInterval> | null = null;
  private activityMessage: Message | null = null;
  private activityText = "";

  constructor(
    private readonly thread: ThreadChannel,
    private readonly log: Logger,
    /** Whether what tools produce is posted into the thread. */
    private readonly forwardToolOutput = false,
  ) {
    this.outbox = new Outbox(log, async (count) => {
      await this.send(`[${count} earlier message(s) dropped while disconnected]`);
    });
  }

  /** The thread's own id. */
  get id(): string {
    return this.thread.id;
  }

  /**
   * True once the thread has been closed and will send nothing further.
   *
   * Closing closes the outbox, which is correct for a session that is over and
   * fatal for one that is starting: a reused closed port accepts posts and
   * silently drops every one of them.
   */
  get isClosed(): boolean {
    return this.outbox.isClosed;
  }

  /**
   * Shows a typing indicator while the agent is working.
   *
   * The service expires the indicator after about ten seconds, so holding it
   * means repeating it. This is the only signal during a long tool loop that
   * the session is alive rather than stuck.
   */
  setBusy(busy: boolean): void {
    if (busy) {
      if (this.typingTimer !== null) return;
      void this.thread.sendTyping().catch(() => undefined);
      this.typingTimer = setInterval(() => {
        void this.thread.sendTyping().catch(() => undefined);
      }, TYPING_REFRESH_MS);
      return;
    }

    if (this.typingTimer === null) return;
    clearInterval(this.typingTimer);
    this.typingTimer = null;
  }

  private send(content: string): Promise<Message> {
    return this.thread.send(plain(content));
  }

  post(text: string): Promise<void> {
    // The agent speaking ends the current run of tool calls, so the next one
    // starts a fresh block rather than being appended below prose.
    this.outbox.enqueue(() => {
      this.activityMessage = null;
      this.activityText = "";
      return Promise.resolve();
    });

    for (const chunk of splitMessage(text, MESSAGE_LIMIT)) {
      this.outbox.enqueue(async () => {
        await this.send(chunk);
      });
    }
    return Promise.resolve();
  }

  /** A thread has one voice, so a notice is posted like anything else. */
  postNotice(text: string): Promise<void> {
    return this.post(text);
  }

  /** The command is already in the thread above its reply. */
  postReply(text: string): Promise<void> {
    return this.post(text);
  }

  /** Renders a change as a fenced diff, which is all a thread can show. */
  postDiff(path: string, added: number, removed: number, body: string): Promise<void> {
    return this.post(renderDiff(path, { empty: false, added, removed, body }));
  }

  /**
   * Does nothing: the message that started the turn is already in the thread,
   * posted by the person who wrote it. Echoing it would show it twice.
   */
  notePrompt(): Promise<void> {
    return Promise.resolve();
  }

  /** Already in the thread, posted by whoever said it, and plainly an aside. */
  noteAside(): Promise<void> {
    return Promise.resolve();
  }

  /** Not shown in a thread, which is a conversation rather than a dashboard. */
  setUsage(): void {}

  /** Not shown: reasoning is long, and a thread is a conversation. */
  noteThinking(): void {}

  /**
   * A thread reads in order, so where one turn ends and the next begins is
   * already plain from the messages themselves.
   */
  beginTurn(): void {}

  /**
   * Posts what a tool produced, when the thread is configured to forward it.
   *
   * A thread cannot attach output to the call above it, so forwarding is the
   * only way to show it there, and it stays off by default because it is a lot
   * of text in a conversation.
   */
  /**
   * Shows that a cheaper model was asked something.
   *
   * One line, not the answer: the answer goes to the agent, and what a reader
   * needs is that part of this turn was not the session's own model.
   */
  noteDelegation(delegated: Delegated): void {
    void this.post(delegationLine(delegated));
  }

  noteToolResult(result: ToolResult): void {
    if (!this.forwardToolOutput) return;
    const body = result.output.trim();
    if (body.length === 0) return;
    void this.post(`\`\`\`\n${body}\n\`\`\``);
  }

  /**
   * Adds a line of tool activity, extending the current block when there is
   * one.
   *
   * A run of tool calls is one thing the agent is doing, not ten. Editing one
   * message keeps it as one block, and keeps a busy turn from pushing the
   * agent's own words off the screen.
   */
  appendActivity(line: string): Promise<void> {
    this.outbox.enqueue(async () => {
      const extended = this.activityText.length === 0 ? line : `${this.activityText}\n${line}`;

      if (this.activityMessage === null || extended.length > MESSAGE_LIMIT) {
        // One entry can exceed a whole message on its own. Nothing is dropped:
        // it is split, and the block continues from the final piece.
        const pieces = splitMessage(line, MESSAGE_LIMIT);
        let last: Message | null = null;
        for (const piece of pieces) last = await this.send(piece);
        this.activityMessage = last;
        this.activityText = pieces[pieces.length - 1] ?? "";
        return;
      }

      await this.activityMessage.edit(plain(extended));
      this.activityText = extended;
    });
    return Promise.resolve();
  }

  /** Marks the outbox connected or buffering, following the gateway. */
  setConnected(connected: boolean): void {
    this.outbox.setConnected(connected);
  }

  async setWaiting(text: string | null): Promise<void> {
    try {
      if (text === null) {
        if (this.waitingMessageId !== null) {
          await this.thread.messages.delete(this.waitingMessageId).catch(() => undefined);
          this.waitingMessageId = null;
        }
        return;
      }

      if (this.waitingMessageId === null) {
        this.waitingMessageId = (await this.send(`[${text}]`)).id;
        return;
      }

      const existing = await this.thread.messages.fetch(this.waitingMessageId);
      await existing.edit(plain(`[${text}]`));
    } catch (error) {
      // A missing waiting message is cosmetic; the session continues.
      this.log.warn("could not update the waiting message", { detail: String(error) });
      this.waitingMessageId = null;
    }
  }

  async setReaction(messageId: string, outcome: ReactionOutcome): Promise<void> {
    const glyph = reaction(outcome);
    const previous = this.reactions.get(messageId);
    if (previous === glyph) return;

    try {
      const message = await this.thread.messages.fetch(messageId).catch(() => null);
      const target: Message | null = message ?? (await this.fetchFromParent(messageId));
      if (target === null) return;

      if (previous !== undefined) {
        await target.reactions.cache.get(previous)?.users.remove(target.client.user?.id);
      }
      await target.react(glyph);
      this.reactions.set(messageId, glyph);
    } catch (error) {
      // A reaction is an acknowledgement, not the work. Losing one must not
      // disturb the session it was acknowledging.
      this.log.warn("could not set a reaction", { detail: String(error) });
    }
  }

  private fetchFromParent(messageId: string): Promise<Message | null> {
    const parent = this.thread.parent;
    if (parent === null || !parent.isTextBased()) return Promise.resolve(null);
    return parent.messages.fetch(messageId).catch(() => null);
  }

  upload(name: string, bytes: Uint8Array, caption: string): Promise<void> {
    this.outbox.enqueue(async () => {
      this.activityMessage = null;
      this.activityText = "";
      await this.thread.send({
        ...plain(caption),
        files: [new AttachmentBuilder(Buffer.from(bytes), { name })],
      });
    });
    return Promise.resolve();
  }

  /**
   * Finishes with the thread, archiving it only when somebody said to.
   *
   * A thread archived because its session idled out drops off the sidebar, and
   * the people who were in it have to go hunting for it. Every reason but a
   * deliberate stop can also be resumed, so the thread is left where it is and
   * the last notice in it says what happened.
   */
  async close(reason: EndReason): Promise<void> {
    this.setBusy(false);
    this.activityMessage = null;
    await this.outbox.flush();
    this.outbox.close();

    if (reason !== "stopped") return;
    await this.thread.setArchived(true).catch((error: unknown) => {
      this.log.warn("could not archive the thread", { detail: String(error) });
    });
  }

  /** Waits for queued work to run. Used by tests, which have no connection. */
  flushForTest(): Promise<void> {
    return this.outbox.flush();
  }
}

/** Creates threads on the served channel, one per session. */
export class ChatThreadFactory implements ThreadFactory {
  private readonly live = new Map<string, ChatThread>();

  constructor(
    private readonly client: Client,
    private readonly channelId: string,
    private readonly log: Logger,
    /** Whether a thread shows what tools produced. Off unless configured. */
    private readonly forwardToolOutput = false,
  ) {}

  async create(message: IncomingMessage, name: string): Promise<{ id: string; port: ThreadPort }> {
    const channel = await this.client.channels.fetch(this.channelId);
    if (channel === null || channel.type !== ChannelType.GuildText) {
      throw new Error(`channel ${this.channelId} is not a text channel that can host threads`);
    }

    const starter = await channel.messages.fetch(message.id);
    const thread = await starter.startThread({ name, autoArchiveDuration: AUTO_ARCHIVE_MINUTES });

    const port = new ChatThread(thread, this.log, this.forwardToolOutput);
    this.live.set(thread.id, port);
    return { id: thread.id, port };
  }

  /**
   * Opens a thread with no message to hang it on.
   *
   * A thread hangs off a message, so one is posted first. That message is also
   * what tells the channel that work has started somewhere else.
   */
  async open(name: string, opener: string): Promise<{ id: string; port: ThreadPort }> {
    const channel = await this.client.channels.fetch(this.channelId);
    if (channel === null || channel.type !== ChannelType.GuildText) {
      throw new Error(`channel ${this.channelId} is not a text channel that can host threads`);
    }

    const starter = await channel.send(plain(opener));
    const thread = await starter.startThread({ name, autoArchiveDuration: AUTO_ARCHIVE_MINUTES });

    const port = new ChatThread(thread, this.log, this.forwardToolOutput);
    this.live.set(thread.id, port);
    return { id: thread.id, port };
  }

  /**
   * A port for a thread that already exists.
   *
   * A resumed thread was created by a previous run of the daemon, so it is not
   * in the live map and has to be adopted by id.
   */
  async portFor(threadId: string): Promise<ThreadPort | undefined> {
    const existing = this.live.get(threadId);
    // A port whose session has ended has a closed outbox and would swallow
    // everything the resumed session says, so it is replaced, not reused.
    if (existing !== undefined && !existing.isClosed) return existing;
    this.live.delete(threadId);

    // Fetched rather than read from cache: after a restart the cache is cold,
    // and the thread being resumed was created by a previous run.
    const channel = await this.client.channels.fetch(threadId).catch(() => null);
    if (channel === null || !channel.isThread()) return undefined;

    // A thread archived by the service's own inactivity is reopened, since
    // somebody writing in it is asking for exactly that.
    if (channel.archived === true) {
      await channel.setArchived(false).catch(() => undefined);
    }

    const port = new ChatThread(channel, this.log, this.forwardToolOutput);
    this.live.set(threadId, port);
    return port;
  }

  /** Follows the gateway, so every thread buffers while it is down. */
  setConnected(connected: boolean): void {
    for (const thread of this.live.values()) thread.setConnected(connected);
  }

  /** Forgets a thread once its session has ended, so ports do not accumulate. */
  release(threadId: string): void {
    this.live.delete(threadId);
  }
}

/**
 * Confirms the configured channel exists and can host threads, before the
 * daemon accepts anything.
 */
export async function assertChannelUsable(client: Client, channelId: string): Promise<void> {
  const channel = await client.channels.fetch(channelId).catch(() => null);
  if (channel === null) {
    throw new Error(`channel ${channelId} could not be fetched; check the id and the bot's access`);
  }
  if (channel.type !== ChannelType.GuildText) {
    throw new Error(
      `channel ${channelId} is not a guild text channel, so it cannot host public threads`,
    );
  }
}
