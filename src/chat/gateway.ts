/**
 * The connection: logging in, reconnecting, and turning library objects into
 * the plain shapes the rest of the daemon works with.
 *
 * Whether a message is acted on is decided in `inbound.ts`, which is a pure
 * function and testable without a connection. This is the transport under it,
 * and the only place that listens to the chat library's events.
 */

import {
  type ChatInputCommandInteraction,
  Client,
  Events,
  GatewayIntentBits,
  type Message,
  type PartialMessage,
  Partials,
} from "discord.js";
import type { ChatConfig } from "../config/schema.ts";
import type { Logger } from "../log.ts";
import { acknowledge, translate, type TranslatedCommand } from "./commands.ts";
import {
  classify,
  type InboundDecision,
  isPermitted,
  type RawMessage,
  withoutBotMention,
} from "./inbound.ts";

/** Reconnection schedule: growing delay, then give up rather than hang on. */
export interface ReconnectPolicy {
  baseDelayMs: number;
  maxDelayMs: number;
  maxAttempts: number;
}

export const DEFAULT_RECONNECT: ReconnectPolicy = {
  baseDelayMs: 2_000,
  maxDelayMs: 60_000,
  maxAttempts: 10,
};

/** The delay before a given reconnection attempt, one-based. */
export function reconnectDelayMs(attempt: number, policy: ReconnectPolicy): number {
  return Math.min(policy.baseDelayMs * 2 ** (attempt - 1), policy.maxDelayMs);
}

/** How long the service has to answer a login before it is a failure. */
const READY_TIMEOUT_MS = 30_000;

/** What the gateway reports to the daemon. */
export interface GatewayHandlers {
  onMessage(message: RawMessage, decision: InboundDecision): Promise<void>;
  /** A slash command was used, already translated to its text form. */
  onCommand(command: TranslatedCommand, interaction: ChatInputCommandInteraction): Promise<void>;
  /** A thread bound to a session was archived or deleted from outside. */
  onThreadClosed(threadId: string): Promise<void>;
  onConnected(): void;
  onDisconnected(): void;
  /** Reconnection has failed for good; running sessions must be shut down. */
  onGaveUp(attempts: number): void;
}

/** Reduces a library message to the facts the filter needs. */
export function toRaw(message: Message | PartialMessage): RawMessage | null {
  if (message.author === null) return null;
  const channel = message.channel;
  const parentId = "parentId" in channel ? (channel.parentId ?? undefined) : undefined;

  return {
    id: message.id,
    authorId: message.author.id,
    authorName: message.author.displayName ?? message.author.username,
    authorIsBot: message.author.bot,
    channelId: message.channelId,
    parentChannelId: parentId,
    content: message.content ?? "",
    attachments: [...message.attachments.values()].map((file) => ({
      id: file.id,
      name: file.name,
      url: file.url,
      size: file.size,
      contentType: file.contentType ?? undefined,
    })),
  };
}

/** Owns the single connection and dispatches filtered messages. */
export class Gateway {
  private client: Client | null = null;
  private attempt = 0;
  private stopping = false;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    private readonly config: ChatConfig,
    private readonly handlers: GatewayHandlers,
    private readonly log: Logger,
    private readonly policy: ReconnectPolicy = DEFAULT_RECONNECT,
  ) {}

  /** The connected client, for the thread factory to create threads with. */
  get connection(): Client {
    if (this.client === null) throw new Error("the gateway is not connected");
    return this.client;
  }

  /** Connects and waits until the connection is ready. */
  async connect(): Promise<void> {
    const client = new Client({
      intents: [
        GatewayIntentBits.Guilds,
        GatewayIntentBits.GuildMessages,
        GatewayIntentBits.MessageContent,
      ],
      partials: [Partials.Channel, Partials.Message],
    });

    this.listen(client);
    this.client = client;

    await client.login(this.config.token);
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error(`the connection was not ready in ${READY_TIMEOUT_MS}ms`)),
        READY_TIMEOUT_MS,
      );
      client.once(Events.ClientReady, () => {
        clearTimeout(timer);
        resolve();
      });
    });
  }

  private listen(client: Client): void {
    client.on(Events.MessageCreate, (message) => {
      const raw = toRaw(message);
      if (raw === null) return;

      const own = client.user?.id;
      const decision = classify(raw, this.config, own);
      if (decision.kind === "ignore") {
        this.log.info("ignored a message", { reason: decision.reason });
        return;
      }

      // The mention summoned the bot; it is not part of what was asked. Taken
      // out here, where the bot's own name is known, so nothing downstream has
      // to know it has one.
      const asked = decision.kind === "start" && this.config.startOnMention && own !== undefined
        ? { ...raw, content: withoutBotMention(raw.content, own) }
        : raw;

      void this.handlers.onMessage(asked, decision).catch((error: unknown) => {
        this.log.error("handling a message failed", { detail: String(error) });
      });
    });

    client.on(Events.InteractionCreate, (interaction) => {
      if (!interaction.isChatInputCommand()) return;

      // The same rule governs slash commands as messages. A blocked account is
      // answered exactly as an unauthorised one, so the two are
      // indistinguishable from outside. An interaction has to be answered at
      // all, or the service reports the bot as broken.
      if (!isPermitted(this.config, interaction.user.id)) {
        void acknowledge(interaction, "you are not permitted to use this bot");
        return;
      }
      void this.handlers.onCommand(translate(interaction), interaction).catch((error: unknown) => {
        this.log.error("handling a slash command failed", { detail: String(error) });
      });
    });

    client.on(Events.ThreadUpdate, (_old, updated) => {
      if (updated.parentId !== this.config.channelId) return;
      if (!updated.archived) return;
      void this.handlers.onThreadClosed(updated.id);
    });

    client.on(Events.ThreadDelete, (thread) => {
      if (thread.parentId !== this.config.channelId) return;
      void this.handlers.onThreadClosed(thread.id);
    });

    client.on(Events.Error, (error) => {
      this.log.error("connection error", { detail: String(error) });
    });

    client.on(Events.ShardDisconnect, () => {
      if (this.stopping) return;
      this.handlers.onDisconnected();
      this.scheduleReconnect();
    });

    client.on(Events.ShardReady, () => {
      this.attempt = 0;
      this.handlers.onConnected();
    });
  }

  private scheduleReconnect(): void {
    if (this.reconnectTimer !== null) clearTimeout(this.reconnectTimer);
    this.attempt += 1;

    if (this.attempt > this.policy.maxAttempts) {
      // A sandbox nobody can reach or stop is worse than no session, so the
      // daemon gives up rather than leaving agents running unattended.
      this.log.error("giving up on reconnecting", { attempts: this.attempt - 1 });
      this.handlers.onGaveUp(this.attempt - 1);
      return;
    }

    const delay = reconnectDelayMs(this.attempt, this.policy);
    this.log.warn("reconnecting", { attempt: this.attempt, delayMs: delay });
    this.reconnectTimer = setTimeout(() => {
      if (this.stopping) return;
      void this.client?.login(this.config.token).catch((error: unknown) => {
        this.log.warn("reconnection attempt failed", { detail: String(error) });
        this.scheduleReconnect();
      });
    }, delay);
  }

  /** Disconnects without triggering reconnection. */
  async close(): Promise<void> {
    this.stopping = true;
    if (this.reconnectTimer !== null) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
    await this.client?.destroy().catch(() => undefined);
    this.client = null;
  }
}
