/**
 * Session registry and lifecycle across the whole daemon.
 *
 * Owns the thread-to-session binding and guarantees it is one to one: a thread
 * belongs to exactly one session for its lifetime and is never reused, so a
 * message can only ever reach the session it was written to.
 */

import { join } from "@std/path";
import type { Scheduler } from "../admission/scheduler.ts";
import type { AgentImage } from "../agent/protocol.ts";
import { threadName } from "../chat/render.ts";
import { secretValues } from "../config/redact.ts";
import type { Config } from "../config/schema.ts";
import type { Logger } from "../log.ts";
import type { MemoryStore } from "../memory/store.ts";
import type { Sandbox } from "../sandbox/backend.ts";
import type { EndReason, ThreadPort } from "./port.ts";
import { callApi, openPullRequest, runCommand } from "./pr.ts";
import { ensureProjectDirectory, type ProjectSelection, selectProject } from "./projects.ts";
import { redacting } from "./redacted.ts";
import type { ThreadRecord, ThreadRegistry } from "./registry.ts";
import { type IncomingMessage, Session, type SessionOptions, type Timers } from "./session.ts";
import { Transcript, TRANSCRIPT_FILENAME } from "./transcript.ts";
import { ViewFanOut } from "./views.ts";

/** How a request to start a session turned out. */
export type StartOutcome =
  | { status: "started"; session: Session }
  | { status: "refused"; reason: string };

/** What the manager needs in order to create a session's thread. */
export interface ThreadFactory {
  /**
   * Creates the thread for a session.
   *
   * @throws when the chat service refuses, in which case no sandbox is
   *   started: an agent nobody can see or stop is worse than no agent.
   */
  create(message: IncomingMessage, name: string): Promise<{ id: string; port: ThreadPort }>;

  /**
   * Creates a thread with no message to hang it on, by posting one first.
   *
   * A session started from the interface still gets a thread, so a turn
   * finishing still reaches a phone wherever the work began.
   */
  open(name: string, opener: string): Promise<{ id: string; port: ThreadPort }>;

  /**
   * A port for a thread that already exists, so a session can be resumed into
   * it after a restart.
   *
   * @returns undefined when the thread cannot be reached.
   */
  portFor(threadId: string): Promise<ThreadPort | undefined>;

  /** Forgets a thread once its session has ended. */
  release?(threadId: string): void;
}

/** Options for the session manager. */
export interface ManagerOptions {
  config: Config;
  sandbox: Sandbox;
  scheduler: Scheduler;
  threads: ThreadFactory;
  registry: ThreadRegistry;
  log: Logger;
  timers?: Timers;
  /** Injected so identifiers are predictable in tests. */
  makeId?: () => string;
  /**
   * Why a prompt cannot run yet, or undefined when it can.
   *
   * Held here rather than by a session so one answer serves every session, and
   * so a refusal happens before a thread is opened.
   */
  unavailable?: (() => Promise<string | undefined>) | undefined;
  /**
   * Who may control any session.
   *
   * Separate from the configured list so a surface can be included without
   * being written into anyone's configuration file.
   */
  operatorIds?: readonly string[];
  /** Memory, or undefined when it is switched off. */
  memory?: MemoryStore | undefined;
  /**
   * Describes an image for a session whose model cannot be shown one.
   *
   * Resolved once at startup from the provider's own model list, and absent
   * when the configured model can see or the provider has nothing that can.
   */
  describeImages?: ((images: AgentImage[], question: string) => Promise<string>) | undefined;
  /** Where the interface is published, when it is. */
  publicUrl?: string | undefined;
  /** Injected so record timestamps are predictable in tests. */
  now?: () => number;
}

/** The part of a session's options that does not depend on which session it is. */
type SharedOptions = Omit<
  SessionOptions,
  | "id"
  | "project"
  | "stateDir"
  | "ownerId"
  | "ownerName"
  | "threadId"
  | "startTurn"
  | "guestIds"
  | "resume"
  | "onGuestsChanged"
  | "onEnded"
>;

let counter = 0;

function defaultId(): string {
  counter += 1;
  return `${counter.toString(36)}${Math.random().toString(36).slice(2, 8)}`;
}

/** Owns every live session and the mapping from threads to them. */
export class SessionManager {
  private readonly byThread = new Map<string, Session>();
  private readonly endedThreads = new Set<string>();
  /**
   * The fan out for each live session, so a view can be attached after the
   * session has started. Keyed by session, because that is what a view asks
   * for; the thread is only one of the surfaces showing it.
   */
  private readonly views = new Map<string, ViewFanOut>();
  /**
   * The values scrubbed from everything a session reports.
   *
   * Computed once: they do not change while the daemon runs, and reading them
   * per post would put a credential on a hot path for no reason.
   */
  private readonly secrets: readonly string[];
  /**
   * The guild the served channel is in, once the service has said.
   *
   * Only used to link back to a thread, so a session started before it is
   * known names who asked without pointing at where they asked.
   */
  private guildId: string | undefined;

  constructor(private readonly options: ManagerOptions) {
    this.secrets = secretValues(options.config);
  }

  /** Records the guild, once the gateway has resolved it. */
  setGuild(guildId: string): void {
    this.guildId = guildId;
  }

  /** Why nothing can run yet, or undefined when it can. */
  unavailable(): Promise<string | undefined> {
    return this.options.unavailable?.() ?? Promise.resolve(undefined);
  }

  /** Everyone who may control any session, configured or built in. */
  private get operatorIds(): readonly string[] {
    return this.options.operatorIds ?? this.options.config.chat.operatorUserIds;
  }

  /** Every live session. */
  get sessions(): Session[] {
    return [...this.byThread.values()];
  }

  /** The session bound to a thread, if it is still live. */
  forThread(threadId: string): Session | undefined {
    return this.byThread.get(threadId);
  }

  /** The session with this identifier, if it is live. */
  forSession(sessionId: string): Session | undefined {
    return this.sessions.find((session) => session.id === sessionId);
  }

  /** True when the thread once held a session that has since ended. */
  isFinishedThread(threadId: string): boolean {
    return this.endedThreads.has(threadId);
  }

  /**
   * Removes sandboxes left behind by a previous run.
   *
   * Runs before the gateway accepts anything, so a crashed daemon cannot leave
   * containers running against a project while a new daemon starts more.
   */
  async sweepOrphans(): Promise<number> {
    const orphans = await this.options.sandbox.listOrphans();
    if (orphans.length === 0) return 0;
    const removed = await this.options.sandbox.removeOrphans(orphans);
    this.options.log.info("removed sandboxes left by a previous run", {
      found: orphans.length,
      removed,
    });
    return removed;
  }

  /**
   * Starts a session for a message in the served channel.
   *
   * Capacity is reserved before the thread is created and released again on
   * every failure path, so a refused or failed start cannot consume a slot.
   */
  start(message: IncomingMessage): Promise<StartOutcome> {
    const id = (this.options.makeId ?? defaultId)();

    // A named session reaches the same directory every time the name is used.
    // An unnamed one works in a directory of its own, named after the session.
    const project = selectProject(message.content, this.options.config.projectRoot, id);
    return this.launch(id, project, message, (name) => this.options.threads.create(message, name));
  }

  /**
   * Starts a session that no message created.
   *
   * Used by the interface. The thread is opened rather than hung off an
   * existing message, so a session started at a keyboard is still announced in
   * the channel and still notifies a phone when it finishes.
   */
  startDetached(request: {
    project: string;
    prompt: string;
    ownerId: string;
    ownerName?: string;
  }): Promise<StartOutcome> {
    const id = (this.options.makeId ?? defaultId)();
    const named = request.project.trim().length > 0 ? `${request.project.trim()}: ` : "";
    const message: IncomingMessage = {
      id: `web-${id}`,
      authorId: request.ownerId,
      ...(request.ownerName === undefined ? {} : { authorName: request.ownerName }),
      content: request.prompt,
    };

    const project = selectProject(`${named}${request.prompt}`, this.options.config.projectRoot, id);
    return this.launch(
      id,
      project,
      message,
      (name) => this.options.threads.open(name, `Session started from the interface: ${name}`),
    );
  }

  private async launch(
    id: string,
    project: ProjectSelection,
    message: IncomingMessage,
    createThread: (name: string) => Promise<{ id: string; port: ThreadPort }>,
  ): Promise<StartOutcome> {
    // Before anything is reserved or created, so a window that is already
    // spent does not open a thread and start a sandbox only to fail on its
    // first turn.
    const spent = await this.unavailable();
    if (spent !== undefined) return { status: "refused", reason: spent };

    // Refused before anything is reserved or created, so a project only ever
    // has one agent writing to it. Two would be editing one working tree with
    // neither able to see the other's changes.
    const busy = this.sessions.find((other) => other.project.path === project.path);
    if (busy !== undefined) {
      return {
        status: "refused",
        reason:
          `${project.name} already has a live session (${busy.id}); continue there, or stop it first`,
      };
    }

    if (this.options.scheduler.reserveSession() === null) {
      return { status: "refused", reason: this.options.scheduler.sessionRefusedReason() };
    }

    const stateDir = join(this.options.config.stateDir, id);

    let thread: { id: string; port: ThreadPort };
    try {
      // The agent stores its own state under a home inside the session's state
      // directory, so it is writable under a mapped user id.
      Deno.mkdirSync(join(stateDir, "home"), { recursive: true });
      ensureProjectDirectory(project, this.options.config.projectRoot);
      thread = await createThread(threadName(project.name, project.prompt));
    } catch (error) {
      // No thread means no session and, deliberately, no sandbox: starting one
      // would leave an agent running that nobody could see or stop.
      this.options.scheduler.releaseSession();
      try {
        Deno.removeSync(stateDir, { recursive: true });
      } catch {
        // It may never have been created, which is the state we wanted.
      }
      return { status: "refused", reason: `no session was started: ${String(error)}` };
    }

    const fanOut = new ViewFanOut(
      this.options.log,
      undefined,
      new Transcript(join(stateDir, TRANSCRIPT_FILENAME), this.options.log),
    );
    await fanOut.attach(thread.port);
    this.views.set(id, fanOut);

    const session = new Session({
      ...this.shared(fanOut),
      id,
      project,
      stateDir,
      ownerId: message.authorId,
      ...(message.authorName === undefined ? {} : { ownerName: message.authorName }),
      threadId: thread.id,
      onGuestsChanged: (guests) => this.rememberGuests(thread.id, guests),
      onEnded: (reason) => this.forget(thread.id, id, reason),
    });

    this.byThread.set(thread.id, session);
    this.options.registry.remember({
      threadId: thread.id,
      sessionId: id,
      stateDir,
      projectName: project.name,
      projectPath: project.path,
      ownerId: message.authorId,
      guests: [],
      updatedAt: (this.options.now ?? Date.now)(),
    });

    await session.start({ ...message, content: project.prompt });
    return { status: "started", session };
  }

  /**
   * Restarts a thread's session, continuing the agent conversation.
   *
   * A daemon restart ends every sandbox, but the agent's history lives in the
   * session state directory, so a thread can be picked up where it stopped
   * rather than being told it is over.
   */
  async resume(threadId: string, message: IncomingMessage): Promise<StartOutcome> {
    const record = this.options.registry.get(threadId);
    if (record === undefined) {
      return { status: "refused", reason: "this thread is not one of mine to resume" };
    }
    if (this.byThread.has(threadId)) {
      return { status: "refused", reason: "this thread already has a live session" };
    }

    const busy = this.sessions.find((other) => other.project.path === record.projectPath);
    if (busy !== undefined) {
      return {
        status: "refused",
        reason:
          `${record.projectName} already has a live session (${busy.id}); continue there, or stop it first`,
      };
    }

    if (this.options.scheduler.reserveSession() === null) {
      return { status: "refused", reason: this.options.scheduler.sessionRefusedReason() };
    }

    const port = await this.options.threads.portFor(threadId);
    if (port === undefined) {
      this.options.scheduler.releaseSession();
      return { status: "refused", reason: "this thread could not be reopened" };
    }

    const transcript = new Transcript(join(record.stateDir, TRANSCRIPT_FILENAME), this.options.log);
    const stored = transcript.read();
    const fanOut = new ViewFanOut(this.options.log, undefined, transcript);

    // The thread is attached while the record is still empty, because it is
    // the surface that produced this history and already shows it. Restoring
    // first would post the whole conversation back into it.
    await fanOut.attach(port);
    fanOut.restore(
      stored.entries.map((held) => ({ turn: held.turn, entry: held.entry })),
      stored.dropped,
    );
    this.views.set(record.sessionId, fanOut);

    const session = new Session({
      ...this.shared(fanOut),
      id: record.sessionId,
      project: {
        name: record.projectName,
        path: record.projectPath,
        prompt: message.content,
        wasExplicit: true,
      },
      startTurn: fanOut.currentTurn,
      stateDir: record.stateDir,
      ownerId: record.ownerId,
      threadId: record.threadId,
      guestIds: record.guests,
      resume: true,
      onGuestsChanged: (guests) => this.rememberGuests(threadId, guests),
      onEnded: (reason) => this.forget(threadId, record.sessionId, reason),
    });

    this.byThread.set(threadId, session);
    this.options.registry.remember({ ...record, updatedAt: (this.options.now ?? Date.now)() });
    await session.start(message);
    return { status: "started", session };
  }

  /** Everything a session is given whether it is new or resumed. */
  private shared(fanOut: ViewFanOut): SharedOptions {
    return {
      thread: redacting(fanOut, this.secrets),
      sandbox: this.options.sandbox,
      scheduler: this.options.scheduler,
      config: this.options.config,
      log: this.options.log,
      operatorIds: this.operatorIds,
      memory: this.options.memory,
      guildId: this.guildId,
      publicUrl: this.options.publicUrl,
      unavailable: () => this.unavailable(),
      ...(this.options.describeImages === undefined
        ? {}
        : { describeImages: this.options.describeImages }),
      openPullRequest: (request: Parameters<typeof openPullRequest>[0]) =>
        openPullRequest(request, runCommand, callApi),
      ...(this.options.timers === undefined ? {} : { timers: this.options.timers }),
    };
  }

  private rememberGuests(threadId: string, guests: string[]): void {
    const current = this.options.registry.get(threadId);
    if (current !== undefined) this.options.registry.remember({ ...current, guests });
  }

  /**
   * Lets go of a session that has ended.
   *
   * Only a deliberate stop drops the thread from the durable index: stopping
   * is how somebody says they are finished with it. Everything else, including
   * a crash, an idle timeout, and a daemon restart, leaves the thread
   * resumable, which is the whole point of keeping the index.
   */
  private forget(threadId: string, sessionId: string, reason: EndReason): void {
    this.byThread.delete(threadId);
    this.endedThreads.add(threadId);
    this.views.delete(sessionId);
    if (reason === "stopped") this.options.registry.forget(threadId);
    this.options.threads.release?.(threadId);
    this.options.log.info("session removed from the registry", { session: sessionId, reason });
  }

  /** True when a thread is one this daemon has seen before. */
  canResume(threadId: string): boolean {
    return !this.byThread.has(threadId) && this.options.registry.get(threadId) !== undefined;
  }

  /**
   * Sessions that are not running but could be picked up again.
   *
   * A session that idled out is not over: the agent's history outlives its
   * sandbox, so it is listed rather than forgotten.
   */
  resumable(): ThreadRecord[] {
    return this.options.registry.all().filter((record) => !this.byThread.has(record.threadId));
  }

  /**
   * Attaches a view to a live session and shows it what it missed.
   *
   * @returns a function that detaches it, or undefined when there is no such
   *   live session.
   */
  attachView(sessionId: string, view: ThreadPort): Promise<(() => void) | undefined> {
    const fanOut = this.views.get(sessionId);
    if (fanOut === undefined) return Promise.resolve(undefined);
    return fanOut.attach(view);
  }

  /**
   * The thread a session belongs to, whether it is running or only remembered.
   *
   * A surface uses it to link back to the conversation, where the same session
   * is also being shown.
   */
  threadIdFor(sessionId: string): string | undefined {
    for (const [threadId, session] of this.byThread) {
      if (session.id === sessionId) return threadId;
    }
    return this.options.registry.all().find((record) => record.sessionId === sessionId)?.threadId;
  }

  /** Delivers a message to the session bound to its thread. */
  async deliver(threadId: string, message: IncomingMessage): Promise<boolean> {
    const session = this.byThread.get(threadId);
    if (session === undefined) return false;
    await session.handle(message);
    return true;
  }

  /**
   * Delivers a message to a session by its own identifier.
   *
   * The interface knows sessions, not threads: a thread is one of the surfaces
   * showing a session, and the browser never sees it. Writing to a session
   * that has stopped resumes it, which is what sending to it is asking for.
   */
  async deliverToSession(sessionId: string, message: IncomingMessage): Promise<boolean> {
    const session = this.forSession(sessionId);
    if (session !== undefined) {
      await session.handle(message);
      return true;
    }

    const record = this.resumable().find((candidate) => candidate.sessionId === sessionId);
    if (record === undefined) return false;
    return (await this.resume(record.threadId, message)).status === "started";
  }

  /** Ends the session bound to a thread, if any. */
  async endThread(threadId: string, reason: EndReason): Promise<void> {
    await this.byThread.get(threadId)?.stop(reason);
  }

  /** Ends every live session, for shutdown. */
  async shutdown(): Promise<void> {
    await Promise.allSettled(this.sessions.map((session) => session.stop("shutdown")));
    this.byThread.clear();
  }
}
