/**
 * Wires configuration, backend, admission, sessions, and the connection
 * together.
 *
 * Kept apart from the entry point so the whole daemon can be built and
 * exercised in a test with no connection and no real sandbox.
 */

import { Scheduler } from "./admission/scheduler.ts";
import type { AgentImage } from "./agent/protocol.ts";
import type { InboundDecision, RawMessage } from "./chat/inbound.ts";
import type { TranslatedCommand } from "./chat/commands.ts";
import { redactConfig } from "./config/redact.ts";
import type { ChatConfig, Config } from "./config/schema.ts";
import { ALLOW_EVERY_USER, DEFAULTS } from "./config/schema.ts";
import type { Logger } from "./log.ts";
import type { MemoryStore } from "./memory/store.ts";
import { BaileySandbox } from "./sandbox/bailey.ts";
import type { CapabilityReport, Sandbox } from "./sandbox/backend.ts";
import { PodmanSandbox } from "./sandbox/podman.ts";
import { answerWithoutSession, firstWord, isAddressedToBot, isAside } from "./session/commands.ts";
import type { ThreadFactory } from "./session/manager.ts";
import { SessionManager } from "./session/manager.ts";
import { ThreadRegistry } from "./session/registry.ts";
import type { IncomingMessage, Timers } from "./session/session.ts";

/** Raised when the backend cannot enforce what configuration demands. */
export class EnforcementGapError extends Error {
  constructor(readonly gaps: readonly string[]) {
    super(
      `sandbox.requireFullEnforcement is set and the backend cannot enforce everything on this host:\n${
        gaps.map((gap) => `  - ${gap}`).join("\n")
      }`,
    );
    this.name = "EnforcementGapError";
  }
}

/** Builds the backend named in configuration. Never falls back to the other. */
export function createSandbox(config: Config, log: Logger): Sandbox {
  return config.sandbox.backend === "podman"
    ? new PodmanSandbox(config.sandbox, log)
    : new BaileySandbox(config.sandbox, log, config.stateDir);
}

/**
 * Settings the chosen backend will not read.
 *
 * A setting that does nothing should say so rather than sit there looking
 * effective. Only a value that differs from the default counts, since that is
 * the only evidence available that somebody chose it deliberately.
 */
export function inertSettings(config: Config): string[] {
  if (config.sandbox.backend !== "bailey") return [];
  return config.sandbox.image === DEFAULTS.sandbox.image
    ? []
    : ["sandbox.image is set but only the podman backend uses it"];
}

/**
 * Renders what the chosen backend can and cannot enforce here.
 *
 * A gap is always stated. Presenting a weaker boundary as if it were a
 * stronger one is worse than the weaker boundary itself, because it removes
 * the chance to decide about it.
 */
export function renderStartupReport(
  report: CapabilityReport,
  inert: readonly string[],
  chat?: ChatConfig,
): string[] {
  const lines: string[] = [];

  if (chat?.allowedUserIds.includes(ALLOW_EVERY_USER) === true) {
    lines.push(
      "ACCESS: the allowlist is open to everyone who can post in the served channel",
      "  anyone who can post there can run code in a sandbox with write access to the project root",
      "  set chat.allowedUserIds to specific account ids to close it",
    );
  }

  if (chat !== undefined && chat.blockedUserIds.length > 0) {
    lines.push(`ACCESS: ${chat.blockedUserIds.length} blocked, refused before every other rule`);
  }

  lines.push(`sandbox backend: ${report.backend}`);
  for (const note of report.notes) lines.push(`  ${note}`);

  if (report.gaps.length === 0) {
    lines.push("  this backend enforces every configured guarantee on this host");
  } else {
    lines.push(`  ${report.gaps.length} guarantee(s) cannot be enforced on this host:`);
    for (const gap of report.gaps) lines.push(`    - ${gap}`);
  }

  for (const setting of inert) lines.push(`  ${setting}`);
  return lines;
}

/**
 * Checks the backend and reports what it can enforce on this host.
 *
 * Standalone so it can run before the connection is made: a missing image or
 * an unenforceable guarantee should fail immediately, not after a login round
 * trip.
 *
 * @throws SandboxUnavailableError when the backend cannot run here.
 * @throws EnforcementGapError when gaps exist and configuration forbids them.
 */
export async function probeSandbox(
  sandbox: Sandbox,
  config: Config,
  log: Logger,
): Promise<CapabilityReport> {
  const report = await sandbox.probe();
  for (const line of renderStartupReport(report, inertSettings(config), config.chat)) {
    log.info(line);
  }

  if (report.gaps.length > 0 && config.sandbox.requireFullEnforcement) {
    throw new EnforcementGapError(report.gaps);
  }
  return report;
}

/** Everything the daemon needs, with the transport injected for testability. */
export interface DaemonOptions {
  config: Config;
  sandbox: Sandbox;
  threads: ThreadFactory;
  log: Logger;
  /** Posts a refusal back to the channel, outside any thread. */
  replyInChannel: (message: IncomingMessage, text: string) => Promise<void>;
  /** Memory, or undefined when it is switched off. */
  memory?: MemoryStore | undefined;
  /**
   * Describes an attached image, for a model that cannot be shown one.
   *
   * Resolved at startup from the provider's own model list, and absent when
   * the configured model can see images or the provider has none that can.
   */
  describeImages?: ((images: AgentImage[], question: string) => Promise<string>) | undefined;
  /**
   * Why nothing can run yet, or undefined when it can.
   *
   * Injected so the daemon knows nothing about which provider is in use or how
   * it reports a spent window.
   */
  unavailable?: (() => Promise<string | undefined>) | undefined;
  /**
   * Powers off the host. Injected so that nothing under test can turn a
   * machine off, and so the daemon does not decide how it is done.
   */
  powerOff?: (() => Promise<string | undefined>) | undefined;
  /**
   * What is left of the provider's usage window, in words.
   *
   * Answered by the daemon rather than by a session: it is about the account
   * the whole host shares, and asking should not need a session running.
   * Absent when the provider does not meter one.
   */
  describeUsage?: (() => Promise<string>) | undefined;
  /** Where the interface is published, when it is. */
  publicUrl?: string | undefined;
  /**
   * Who may control any session.
   *
   * Separate from the configured list so a surface can be included without
   * being written into anyone's configuration file.
   */
  operatorIds?: readonly string[] | undefined;
  /** Injected so session deadlines can be driven in tests. */
  timers?: Timers | undefined;
}

/** The running daemon. */
export class Daemon {
  readonly scheduler: Scheduler;
  readonly sessions: SessionManager;
  private accepting = false;
  private readonly registry: ThreadRegistry;

  constructor(private readonly options: DaemonOptions) {
    this.scheduler = new Scheduler(options.config.limits);
    this.registry = new ThreadRegistry(
      ThreadRegistry.pathFor(options.config.stateDir),
      options.log,
    );
    this.sessions = new SessionManager({
      config: options.config,
      sandbox: options.sandbox,
      scheduler: this.scheduler,
      threads: options.threads,
      registry: this.registry,
      log: options.log,
      memory: options.memory,
      publicUrl: options.publicUrl,
      ...(options.operatorIds === undefined ? {} : { operatorIds: options.operatorIds }),
      unavailable: options.unavailable,
      ...(options.timers === undefined ? {} : { timers: options.timers }),
      ...(options.describeImages === undefined ? {} : { describeImages: options.describeImages }),
    });
  }

  /** True once startup finished and messages may be acted on. */
  get isAccepting(): boolean {
    return this.accepting;
  }

  /**
   * Checks the backend and reports what it can enforce here.
   *
   * @throws SandboxUnavailableError when the backend cannot run here.
   * @throws EnforcementGapError when gaps exist and configuration forbids them.
   */
  probe(): Promise<CapabilityReport> {
    return probeSandbox(this.options.sandbox, this.options.config, this.options.log);
  }

  /**
   * Probes the backend, loads the thread index, and sweeps orphans.
   *
   * Nothing is accepted until this finishes, so a crashed previous daemon
   * cannot leave sandboxes running against a project while new ones start.
   */
  async start(probed?: CapabilityReport): Promise<CapabilityReport> {
    Deno.mkdirSync(this.options.config.stateDir, { recursive: true });
    this.options.log.info("effective configuration", {
      config: JSON.stringify(redactConfig(this.options.config)),
    });

    const report = probed ?? (await this.probe());
    this.registry.load();
    this.options.log.info("thread index loaded", { threads: this.registry.size });
    await this.sessions.sweepOrphans();
    this.accepting = true;
    return report;
  }

  /**
   * Turns the host off, if this person may.
   *
   * Answered here rather than in a session: whoever starts a thread owns it,
   * and owning a thread is no reason to be able to turn the computer off. The
   * only list that counts is the daemon's own.
   *
   * @returns what to say, or undefined when the message was not this command.
   */
  private async powerOffHost(content: string, authorId: string): Promise<string | undefined> {
    if (firstWord(content) !== "!shutdown") return undefined;

    const allowed = this.options.config.shutdown.allowedUserIds;
    if (allowed.length === 0) {
      return "nobody may power off this host; set shutdown.allowedUserIds to change that";
    }
    if (!allowed.includes(authorId)) {
      return "you are not on the list of accounts that may power off this host";
    }
    if (this.options.powerOff === undefined) {
      return "this daemon cannot power off the host";
    }

    this.options.log.warn("powering off on request", { user: authorId });
    return (await this.options.powerOff()) ?? "powering off now";
  }

  /**
   * Says what is left of the provider's usage window.
   *
   * @returns what to say, or undefined when the message was not this command.
   */
  private async describeUsage(content: string): Promise<string | undefined> {
    if (firstWord(content) !== "!usage") return undefined;
    if (this.options.describeUsage === undefined) {
      return "this provider does not report a usage window";
    }
    return await this.options.describeUsage();
  }

  /** Whatever the daemon answers itself, wherever it was typed. */
  private async answerAsDaemon(content: string, authorId: string): Promise<string | undefined> {
    return (await this.powerOffHost(content, authorId)) ?? (await this.describeUsage(content));
  }

  /** Acts on a message the gateway has already filtered. */
  async handle(raw: RawMessage, decision: InboundDecision): Promise<void> {
    if (!this.accepting) {
      this.options.log.warn("a message arrived before startup finished and was not acted on");
      return;
    }

    const message: IncomingMessage = {
      id: raw.id,
      authorId: raw.authorId,
      authorName: raw.authorName,
      content: raw.content,
      attachments: raw.attachments,
    };

    // Before anything is routed. These are not session commands, and being in
    // a thread is not a reason to be allowed to run one.
    const answered = await this.answerAsDaemon(message.content, message.authorId);
    if (answered !== undefined) {
      await this.options.replyInChannel(message, answered);
      return;
    }

    if (decision.kind === "thread") {
      await this.deliverToThread(decision.threadId, message);
      return;
    }

    await this.startFromChannel(message);
  }

  private async deliverToThread(threadId: string, message: IncomingMessage): Promise<void> {
    if (await this.sessions.deliver(threadId, message)) return;

    // A thread with no live session may still be resumable: the agent's
    // history outlives the sandbox, so a restart does not end a conversation.
    if (this.sessions.canResume(threadId)) {
      const outcome = await this.sessions.resume(threadId, message);
      if (outcome.status === "refused") {
        await this.options.replyInChannel(message, outcome.reason);
      }
      return;
    }

    if (this.sessions.isFinishedThread(threadId)) {
      await this.options.replyInChannel(
        message,
        "this session has ended; post in the channel to start a new one",
      );
    }
  }

  private async startFromChannel(message: IncomingMessage): Promise<void> {
    // An aside is people talking, not work to start. The channel is where they
    // talk, so one here is left alone entirely: no session, no thread, and no
    // reply, which would itself be noise in the conversation it was
    // deliberately kept out of.
    if (isAside(message.content)) return;

    // Answered here rather than by starting a session: a command that only
    // describes the system needs nothing running, and opening a thread and a
    // sandbox to print a list is not an answer.
    const listed = answerWithoutSession(message.content);
    if (listed !== undefined) {
      await this.options.replyInChannel(message, listed);
      return;
    }

    // Another bot's command, or one of this system's that needs a session.
    // Either way it is not work to start, and the channel is shared, so it is
    // left alone rather than answered.
    if (isAddressedToBot(message.content)) return;

    const outcome = await this.sessions.start(message);
    if (outcome.status === "refused") {
      await this.options.replyInChannel(message, outcome.reason);
    }
  }

  /**
   * Runs a slash command, which is the same command an in-thread message runs.
   *
   * @returns a short line to acknowledge the interaction with.
   */
  async runCommand(command: TranslatedCommand): Promise<string> {
    if (!this.accepting) return "the daemon is still starting up";

    const answered = await this.answerAsDaemon(command.content, command.userId);
    if (answered !== undefined) return answered;

    // Some commands describe the system rather than act on a session, so they
    // answer anywhere. The answer is the acknowledgement: it goes back to the
    // person who ran it and nowhere else, which is where a help listing wants
    // to be rather than posted into a channel everyone is reading.
    const listed = answerWithoutSession(command.content);
    if (listed !== undefined) return listed;

    if (command.threadId === undefined) {
      return "use this inside a session thread; post in the channel to start one";
    }

    const delivered = await this.sessions.deliver(command.threadId, {
      id: `slash-${command.threadId}`,
      authorId: command.userId,
      authorName: command.userName,
      content: command.content,
    });
    if (delivered) return `ran ${firstWord(command.content)}`;

    if (this.sessions.canResume(command.threadId)) {
      return "this thread is asleep; post a message in it to wake the session first";
    }
    return "this thread has no session";
  }

  /**
   * Records which guild the served channel is in.
   *
   * Discovered after construction, because it takes a round trip and the
   * daemon is built before the connection has answered anything.
   */
  setGuild(guildId: string): void {
    this.sessions.setGuild(guildId);
  }

  /** Ends the session bound to a thread that was closed from outside. */
  async threadClosed(threadId: string): Promise<void> {
    await this.sessions.endThread(threadId, "thread archived");
  }

  /** Stops accepting, ends every session, and stops every timer. */
  async shutdown(): Promise<void> {
    this.accepting = false;
    await this.sessions.shutdown();
    this.scheduler.shutdown();
    this.options.memory?.close();
  }
}
