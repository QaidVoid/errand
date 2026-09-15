/**
 * One live session: a sandboxed agent, a project, and the surfaces watching.
 *
 * Everything a session does passes through here, which is why so little of it
 * is decided here. What may be typed and by whom is in `commands.ts`, where a
 * path may point is in `sandbox/paths.ts`, what a message looks like is in
 * `chat/render.ts`. This is the lifecycle: start it, route what arrives, take
 * a turn, and tear it down exactly once.
 */

import { join } from "@std/path";
import type { Scheduler, Ticket } from "../admission/scheduler.ts";
import { AgentClient, type AgentHandlers } from "../agent/client.ts";
import { TurnDelegations } from "../agent/delegate.ts";
import { isRefused } from "../agent/delegation.ts";
import {
  DELEGATE_COMMAND,
  delegateCommandContents,
  delegateInstructions,
} from "../agent/requests.ts";
import type { AgentImage, DialogRequest, Usage } from "../agent/protocol.ts";
import { fileDiff } from "../chat/diff.ts";
import {
  bytes,
  compactionLine,
  connectionLine,
  dialogLines,
  directoryListing,
  fileView,
  marker,
  questionLine,
  toolLine,
  truncate,
  usageSummary,
  warningLine,
} from "../chat/render.ts";
import type { RawAttachment } from "../chat/inbound.ts";
import type { Config, GithubConfig } from "../config/schema.ts";
import type { Logger } from "../log.ts";
import {
  BLOCK_FILENAME,
  memoryInstructions,
  type MemoryStore,
  NOTES_FILENAME,
  parseNotes,
  PROJECT_NOTES_FILENAME,
  type Scope,
} from "../memory/store.ts";
import type { Sandbox, SandboxHandle } from "../sandbox/backend.ts";
import { STATE_PATH } from "../sandbox/backend.ts";
import { hostPathUnder } from "../sandbox/paths.ts";
import { ATTACHMENTS_DIR, isImage, receive, type Taken } from "./attachments.ts";
import {
  ASIDE,
  asksForPullRequest,
  COMMANDS,
  helpText,
  isAddressedToBot,
  isAside,
  isCommand,
  mayRun,
  parseUserId,
} from "./commands.ts";
import { Delegating, type Outcome as DelegationOutcome } from "./delegating.ts";
import { MIN_CHECK_MS, nextCheckMs, treeBytes, verdict } from "./disk.ts";
import type { ChosenModel } from "./model.ts";
import { prepareRecordDir, recordDir } from "./record.ts";
import { readDirectory, readFileForDisplay } from "./files.ts";
import {
  ASKED_FILENAME,
  GH_SHIM_FILENAME,
  ghShimContents,
  GITCONFIG_FILENAME,
  gitConfigContents,
  gitIdentityEnv,
  REQUEST_FILENAME,
  reviewInstructions,
  type SessionLinks,
  threadLink,
  TOKEN_VARIABLE,
  transcriptLink,
} from "./github.ts";
import type { EndReason, ReactionOutcome, SessionUsage, ThreadPort } from "./port.ts";
import type { Request as PullRequest } from "./pr.ts";
import type { ProjectSelection } from "./projects.ts";
import { parseSize } from "../config/size.ts";
import { rulesBlock } from "./rules.ts";

/** A message as a session sees it, whatever surface it arrived from. */
export interface IncomingMessage {
  id: string;
  authorId: string;
  /** Display name, when the service gave one. */
  authorName?: string | undefined;
  content: string;
  attachments?: RawAttachment[];
}

/** Timers, injected so a test does not wait out an idle timeout. */
export interface Timers {
  setTimeout(handler: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

const systemTimers: Timers = {
  setTimeout: (handler, ms) => setTimeout(handler, ms),
  clearTimeout: (handle) => clearTimeout(handle as number),
};

/**
 * Endings a thread is not told about.
 *
 * None of them is a failure and none of them is final: the next message picks
 * the session up, and that session says so when it starts.
 */
const QUIET_ENDINGS = new Set<EndReason>(["idle", "shutdown", "thread archived"]);

/** Tools whose effect is worth showing as a diff. */
const EDITING_TOOLS = new Set(["edit", "write", "create", "str_replace", "multi_edit"]);

/** Largest file uploaded on request. The service refuses much more than this. */
const MAX_UPLOAD_BYTES = 8 * 1024 * 1024;

/** Largest file diffed. Beyond this the change is summarised, not shown. */
const MAX_DIFFABLE_BYTES = 512 * 1024;

/** Tool outputs kept so a delegation can name one, newest first. */
const MAX_REMEMBERED_OUTPUTS = 50;

/** Fetches an attachment. Replaced in tests, which have no network. */
async function defaultFetch(url: string): Promise<Uint8Array> {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`the chat service answered ${response.status}`);
  return new Uint8Array(await response.arrayBuffer());
}

/** An attachment as the agent protocol carries an image. */
function imageOf(file: Taken): AgentImage {
  return {
    type: "image",
    data: encodeBase64(file.bytes),
    mimeType: file.contentType ?? "image/png",
  };
}

function encodeBase64(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

function firstLine(text: string): string {
  const line = text.split("\n")[0] ?? "";
  return line.length > 80 ? `${line.slice(0, 80)}...` : line;
}

function reason(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** Everything the manager gives a session at construction. */
export interface SessionOptions {
  id: string;
  project: ProjectSelection;
  /**
   * Provider and model this session runs on, when the opening message named
   * them. Absent leaves the configured pair, which is the ordinary case.
   */
  chosen?: ChosenModel | undefined;
  stateDir: string;
  thread: ThreadPort;
  sandbox: Sandbox;
  scheduler: Scheduler;
  config: Config;
  log: Logger;
  timers?: Timers;
  /** The account that started this session and may control it. */
  ownerId: string;
  /** The owner's display name, when the service gave one. */
  ownerName?: string | undefined;
  /**
   * The turn a resumed session carries on from.
   *
   * A thread outlives the session running it, and its history is numbered.
   * Starting again at one would label a new exchange with a number already
   * used, and a reader grouping by turn would see the two as one.
   */
  startTurn?: number | undefined;
  /** Opens a pull request. Injected so a session can be driven without one. */
  openPullRequest?: ((request: PullRequest) => Promise<string>) | undefined;
  /** The thread this session runs in, for linking back to the conversation. */
  threadId?: string | undefined;
  /** The guild the thread is in, which a thread link needs. */
  guildId?: string | undefined;
  /** Where the interface is published, when it is. */
  publicUrl?: string | undefined;
  /**
   * Models this host knows this provider serves, for switching between them.
   *
   * Read once at startup rather than asked of the agent, which reports what it
   * is running rather than what it could run.
   */
  availableModels?: readonly string[] | undefined;
  /**
   * Where the provider is reached for a delegated question.
   *
   * Read once at startup from the agent's own model store, so the cheaper
   * model is reached at the endpoint that already serves this provider.
   */
  delegateBaseUrl?: string | undefined;
  /**
   * Why a prompt cannot run yet, or undefined when it can.
   *
   * Supplied rather than asked for directly, so a session knows nothing about
   * which provider is in use or how it reports a spent window.
   */
  unavailable?: (() => Promise<string | undefined>) | undefined;
  /** Accounts that may control any session, not only their own. */
  operatorIds: readonly string[];
  /** Accounts the owner has invited to take part in this thread. */
  guestIds?: readonly string[];
  /** Called when the guest list changes, so it can be persisted. */
  onGuestsChanged?: (guests: string[]) => void;
  /**
   * Told when the session moves to another model, so a restart puts it back on
   * the one it was working with rather than the configured one.
   */
  onModelChanged?: (provider: string, model: string) => void;
  /** Memory, or undefined when it is switched off. */
  memory?: MemoryStore | undefined;
  /** Fetches an attachment. Injected so tests need no network. */
  fetchAttachment?: ((url: string) => Promise<Uint8Array>) | undefined;
  /**
   * Describes an attached image, for a session whose model cannot see one.
   *
   * Injected only in that case: present means the images must not be handed
   * over, absent means they can be. The session does not decide which, because
   * what a model accepts is the provider's business and is read once at
   * startup rather than per attachment.
   */
  describeImages?: ((images: AgentImage[], question: string) => Promise<string>) | undefined;
  /** Continue the agent conversation already stored in the state directory. */
  resume?: boolean;
  /** Called once the session is finished with, so the manager can forget it. */
  onEnded: (reason: EndReason) => void;
}

/** One live session. */
export class Session {
  private sandbox: SandboxHandle | null = null;
  private client: AgentClient | null = null;
  private ticket: Ticket | null = null;
  private currentMessageId: string | null = null;
  private currentAuthorId: string | null = null;
  private idleTimer: unknown = null;
  private diskTimer: unknown = null;
  /** Bytes held by the project and state directory when the session started. */
  private diskBaseline = 0;
  /** True once the session has been told it is close to its budget. */
  private diskWarned = false;
  /** What the previous measurement saw, so a write rate can be derived. */
  private diskLastWritten = 0;
  private diskLastAt = 0;
  /**
   * Who has already been told why they cannot take part.
   *
   * Every refused message still gets its reaction, so nobody is left without
   * an answer. The sentence explaining it is posted once per person: somebody
   * who keeps typing in a thread they were not invited to would otherwise fill
   * it with the same line, which spams the owner with the refusal rather than
   * with the messages being refused.
   */
  private readonly explained = new Set<string>();
  private readonly pendingEdits = new Map<string, string>();
  /**
   * What each tool call produced, so a delegation can name one by its id.
   *
   * Bounded: a long turn makes hundreds of calls, and a delegation asks about
   * one it has just seen rather than one from an hour ago.
   */
  private readonly outputs = new Map<string, string>();
  /** The delegations of the turn now running, if any. */
  private turnDelegations: TurnDelegations | null = null;
  private delegating: Delegating | null = null;
  /** What delegation has cost and saved this session, for reporting it. */
  private delegated = { asked: 0, answered: 0, tokens: 0, keptOut: 0 };
  private readonly guests: Set<string>;
  /** Where a `!model` moved this session, which outlives the command. */
  private switched: { provider: string; model: string } | undefined;
  /** Speakers whose memory has already been given to the agent this session. */
  private readonly introduced = new Set<string>();
  /** Whoever spoke most recently, so a recorded fact is attributed to them. */
  private lastSpeakerId: string | null = null;
  private ended = false;
  private lastActive = Date.now();
  /** The command being answered, so its replies can be marked as such. */
  private replyingTo: string | null = null;
  /** Turns opened so far. Zero means nothing has been asked for yet. */
  private turn: number;
  private usage: SessionUsage = {
    input: 0,
    output: 0,
    cacheRead: 0,
    cacheWrite: 0,
    totalTokens: 0,
    cost: 0,
    contextTokens: 0,
    turns: 0,
  };
  private aborting = false;
  private readonly timers: Timers;
  private readonly log: Logger;

  constructor(private readonly options: SessionOptions) {
    this.timers = options.timers ?? systemTimers;
    this.turn = options.startTurn ?? 0;
    this.guests = new Set(options.guestIds ?? []);
    this.log = options.log.with({ session: options.id });
  }

  /** The session's stable identifier. */
  get id(): string {
    return this.options.id;
  }

  /** The project this session works in. */
  get project(): ProjectSelection {
    return this.options.project;
  }

  /** The account that started this session. */
  get ownerId(): string {
    return this.options.ownerId;
  }

  /** What this session was first asked to do, for naming it. */
  get opening(): string {
    return this.options.project.prompt.trim();
  }

  /** When this session last did or was told anything. */
  get lastActiveAt(): number {
    return this.lastActive;
  }

  /** True once the session has finished, by any path. */
  get isEnded(): boolean {
    return this.ended;
  }

  /** True while a turn holds an admission slot. */
  get isBusy(): boolean {
    return this.ticket !== null;
  }

  /** Everyone invited to take part, for reporting and persistence. */
  get guestList(): string[] {
    return [...this.guests];
  }

  /**
   * Resolves a path inside this session's project.
   *
   * The same containment the sandbox applies to the agent, so a surface cannot
   * read anything the agent could not.
   *
   * @returns undefined when the path lies outside the project.
   */
  resolveInProject(relative: string): string | undefined {
    return hostPathUnder(this.options.project.path, this.options.project.path, relative);
  }

  /**
   * Starts the sandbox and waits for the agent to answer.
   *
   * @returns false when the session could not start, having already reported
   *   why in the thread and released everything it reserved.
   */
  async start(firstMessage: IncomingMessage): Promise<boolean> {
    const github = this.options.config.github;
    this.writeGitConfig(github);
    this.writeAgentBin(github);
    const systemPromptPath = this.writeMemoryBlock();

    try {
      this.sandbox = await this.options.sandbox.launch({
        sessionId: this.options.id,
        projectPath: this.options.project.path,
        stateDir: this.options.stateDir,
        // The GitHub token crosses too. Reading issues and leaving comments is
        // most of working on somebody's repository, and none of it is possible
        // without one. Pull requests are still composed by the daemon.
        env: {
          [this.options.config.agent.credentialName]: this.options.config.agent.credential,
          ...(github === undefined
            ? {}
            : { [TOKEN_VARIABLE]: github.token, ...gitIdentityEnv(github) }),
        },
        provider: this.provider(),
        model: this.model(),
        providers: this.options.config.agent.providers,
        systemPromptPath,
        resume: this.options.resume === true,
      });
    } catch (error) {
      await this.say(`could not start this session: ${reason(error)}`);
      await this.finish("startup failed");
      return false;
    }

    void this.startDiskWatch();
    this.startDelegating();

    this.client = new AgentClient(
      this.sandbox.process,
      this.buildHandlers(),
      this.log,
      this.options.config.timeouts.questionMs,
    );
    void this.client.run();

    try {
      await this.client.waitUntilReady(this.options.config.timeouts.startupMs);
    } catch (error) {
      await this.say(
        `the agent did not become ready within ${this.options.config.timeouts.startupMs}ms: ${
          reason(error)
        }`,
      );
      await this.finish("startup failed");
      return false;
    }

    // Which sandbox confines a session is not named in the thread. It tells a
    // reader nothing they can act on, and tells anyone else what to probe. The
    // operator sees it at startup, in the log, where it belongs.
    const opening = this.options.resume === true
      ? `resumed, continuing in ${this.options.project.name}`
      : `ready, working in ${this.options.project.name}`;

    // Offered once, when the thread is new. A thread shows the conversation
    // and the interface shows the work behind it, and somebody reading on a
    // phone has no other way to find the second from the first.
    const transcript = this.sessionLinks().transcript;
    await this.options.thread.postNotice(
      transcript === undefined
        ? connectionLine(opening)
        : `${connectionLine(opening)}\n${transcript}`,
      "started",
    );
    this.resetIdleTimer();
    await this.route(firstMessage);
    return true;
  }

  /** Routes a message from the thread: a command, a dialog answer, or a prompt. */
  async handle(message: IncomingMessage): Promise<void> {
    if (this.ended) return;
    this.resetIdleTimer();
    await this.route(message);
  }

  /** Ends the session on request, reporting the reason in the thread. */
  async stop(why: EndReason): Promise<void> {
    await this.endBecause(why, `this session ended (${why})`);
  }

  private async route(message: IncomingMessage): Promise<void> {
    const content = message.content.trim();
    const [word, ...rest] = content.split(/\s+/);

    // First, so that an aside which happens to read like a command still runs
    // nothing. Deciding this later would make the marker unreliable, which is
    // the opposite of what it is for.
    if (isAside(content)) {
      await this.noteAside(content, message);
      return;
    }

    if (isCommand(content)) {
      await this.runCommand(word as string, rest.join(" "), message);
      return;
    }

    // Left alone entirely. It is another bot's command, where anything from
    // this one is noise in somebody else's exchange, or a person typing to the
    // room, where marking it failed says their message was wrong when it was
    // simply not addressed here.
    if (isAddressedToBot(content)) return;

    const pending = this.client?.pendingDialog;
    if (pending !== undefined) {
      await this.answerDialog(pending, content, message);
      return;
    }

    // A message carrying only a file still says something: that a file
    // arrived. Discarding it for having no text is why one used to vanish.
    const attached = await this.takeAttachments(message);
    if (content.length === 0 && attached.note.length === 0) return;

    await this.submitPrompt(content, message, { attached });
  }

  /**
   * Says something in the session.
   *
   * While a command is being answered this marks what it writes as that
   * command's reply. Everywhere else it is ordinary output. Routing it in one
   * place is what keeps every reply marked without each of them having to
   * remember to say so.
   */
  private async say(text: string): Promise<void> {
    if (this.replyingTo !== null) {
      await this.options.thread.postReply(text, this.replyingTo);
      return;
    }
    await this.options.thread.post(text);
  }

  /**
   * Takes what was attached and says what the agent should know about it.
   *
   * @returns the line to add to the prompt, and the images to hand over. Empty
   *   when nothing was attached or nothing survived the limits.
   */
  private async takeAttachments(
    message: IncomingMessage,
  ): Promise<{ note: string; images: AgentImage[] }> {
    const attached = message.attachments ?? [];
    if (attached.length === 0) return { note: "", images: [] };

    const outcome = await receive(
      attached,
      this.options.project.path,
      {
        maxBytes: this.options.config.output.maxAttachmentBytes,
        maxCount: this.options.config.output.maxAttachmentsPerMessage,
      },
      this.options.fetchAttachment ?? defaultFetch,
    );

    for (const refusal of outcome.refused) {
      await this.say(warningLine(`\`${refusal.name}\` was not taken: ${refusal.reason}`));
    }

    if (outcome.taken.length === 0) return { note: "", images: [] };

    const listed = outcome.taken.map((file) => file.path).join(", ");
    const note = `Files attached to this message, saved in the project at: ${listed}`;

    // Handed over to look at as well as saved, so the agent has both the
    // picture and the path.
    const images = outcome.taken
      .filter((file) => isImage(file.contentType, file.path))
      .map((file) => imageOf(file));

    const describe = this.options.describeImages;
    if (images.length === 0 || describe === undefined) return { note, images };

    // Injected only when this session's model cannot be shown an image, so
    // reaching here means handing them over would fail the turn.
    try {
      return { note: `${note}\n\n${await describe(images, message.content)}`, images: [] };
    } catch (error) {
      this.log.warn("could not describe an attached image", { detail: String(error) });
      await this.say(
        warningLine(
          error instanceof Error
            ? error.message
            : "an attached image could not be described for this model",
        ),
      );
      return { note, images: [] };
    }
  }

  private async answerDialog(
    dialog: DialogRequest,
    content: string,
    message: IncomingMessage,
  ): Promise<void> {
    const outcome = this.client?.answerDialog(dialog.id, content);
    if (outcome === "accepted") {
      // Recorded like any other prompt. It is what the agent was waiting for,
      // and a transcript without it shows a question that answered itself.
      await this.options.thread.notePrompt(message.authorName ?? message.authorId, content);
      await this.options.thread.setReaction(message.id, "accepted");
      return;
    }
    await this.say(questionLine(`that did not answer the question. ${dialogLines(dialog)}`));
  }

  private async submitPrompt(
    content: string,
    message: IncomingMessage,
    options: { hold?: boolean; attached?: { note: string; images: AgentImage[] } } = {},
  ): Promise<void> {
    const attached = options.attached ?? { note: "", images: [] };
    if (content.length === 0 && attached.note.length === 0) return;

    if (asksForPullRequest(content)) this.notePullRequestAsked(message);

    // What arrived is said as part of the prompt, so the agent knows a file is
    // there and where to read it.
    const said = attached.note.length === 0 ? content : `${content}\n\n${attached.note}`.trim();

    // A session is the owner's: their project, their model spend, their turn
    // in the queue. Taking part is something they invite you to.
    if (!this.mayTakePart(message.authorId)) {
      await this.refuse(message, this.notInvited());
      return;
    }

    // Checked before the turn is queued or the running one redirected, so a
    // spent window is answered with when to come back rather than with a turn
    // that starts and then fails against the provider.
    const spent = await this.options.unavailable?.();
    if (spent !== undefined) {
      await this.say(spent);
      await this.options.thread.setReaction(message.id, "failed");
      return;
    }

    // Saying something to a working agent redirects what it is doing, which is
    // the point of saying it now rather than waiting. It is the same turn, so
    // it neither opens one nor reserves a slot.
    if (this.isBusy && options.hold !== true) {
      await this.redirect(said, message, attached.images);
      return;
    }

    // Opened before the prompt is noted, so the prompt is the first thing in
    // the turn it starts rather than the last thing in the one before it.
    this.turn += 1;
    this.turnDelegations = this.newDelegations();
    this.options.thread.beginTurn(this.turn);

    await this.options.thread.setReaction(message.id, "accepted");
    await this.options.thread.notePrompt(message.authorName ?? message.authorId, said);

    const withContext = this.introduce({ ...message, content: said });
    this.lastSpeakerId = message.authorId;

    const outcome = this.options.scheduler.submit({
      sessionId: this.options.id,
      onAdmitted: (ticket) => {
        void this.sendIfStillAllowed(ticket, withContext, message, attached.images);
      },
      onExpired: () => {
        void this.options.thread.setWaiting(null);
        void this.say(
          `this message waited longer than the queue allows and was not sent: ${firstLine(said)}`,
        );
        void this.options.thread.setReaction(message.id, "failed");
      },
      onPositionChanged: (position) => {
        void this.options.thread.setWaiting(`waiting for a turn slot, position ${position}`);
      },
    });

    if (outcome.status === "admitted") {
      await this.sendPrompt(outcome.ticket, withContext, message, attached.images);
      return;
    }

    if (outcome.status === "rejected") {
      await this.say(outcome.reason);
      await this.options.thread.setReaction(message.id, "failed");
      return;
    }

    await this.options.thread.setWaiting(`waiting for a turn slot, position ${outcome.position}`);
  }

  /**
   * Sends a prompt that waited for a turn slot, unless the window closed while
   * it waited.
   *
   * The check at submission cannot cover this. A prompt may sit in the queue
   * for as long as the queue allows, and the provider's window can be spent in
   * that time, so a prompt admitted later would spend its slot on a turn the
   * provider refuses. The ticket is released through the same path a failed
   * turn uses, since a slot held by a turn that never ran shrinks the
   * concurrency cap for good.
   */
  private async sendIfStillAllowed(
    ticket: Ticket,
    content: string,
    message: IncomingMessage,
    images: AgentImage[] = [],
  ): Promise<void> {
    const spent = await this.options.unavailable?.();
    if (spent === undefined) {
      await this.sendPrompt(ticket, content, message, images);
      return;
    }

    this.ticket = ticket;
    this.currentMessageId = message.id;
    await this.options.thread.setWaiting(null);
    await this.say(spent);
    await this.settleTurn("failed");
  }

  private async sendPrompt(
    ticket: Ticket,
    content: string,
    message: IncomingMessage,
    images: AgentImage[] = [],
  ): Promise<void> {
    this.ticket = ticket;
    this.currentMessageId = message.id;
    this.currentAuthorId = message.authorId;
    await this.options.thread.setWaiting(null);
    this.options.thread.setBusy(true);

    const sent = this.client?.prompt(content, {
      behavior: "followUp",
      ...(images.length === 0 ? {} : { images }),
    }) ?? false;
    if (!sent) {
      await this.say("the agent is not accepting prompts; this session has ended");
      await this.settleTurn("failed");
      await this.finish("crashed");
    }
  }

  /**
   * Notes something said to the people in the thread rather than to the agent.
   *
   * Kept and shown, because it is part of what happened, and marked as an
   * aside so that reading a session back distinguishes what the agent was told
   * from what was said around it.
   */
  private async noteAside(content: string, message: IncomingMessage): Promise<void> {
    const said = content.trimStart().slice(ASIDE.length).trim();
    await this.options.thread.noteAside(message.authorName ?? message.authorId, said);
    await this.options.thread.setReaction(message.id, "succeeded");
  }

  /**
   * Redirects the turn already running.
   *
   * Recorded in the exchange it changes rather than one of its own, because it
   * is the same turn: a reader sees the turn take a different course, which is
   * what happened.
   */
  private async redirect(
    content: string,
    message: IncomingMessage,
    images: AgentImage[] = [],
  ): Promise<void> {
    this.resetIdleTimer();
    this.lastSpeakerId = message.authorId;

    await this.options.thread.setReaction(message.id, "accepted");
    await this.options.thread.notePrompt(message.authorName ?? message.authorId, content);

    const sent = this.client?.steer(this.introduce({ ...message, content }), images) ?? false;
    if (!sent) {
      await this.say("the agent is not accepting anything further; this session has ended");
      await this.options.thread.setReaction(message.id, "failed");
    }
  }

  /**
   * Releases the turn's admission slot and sets the outcome reaction.
   *
   * Every path that ends a turn goes through here, so the slot is released
   * exactly once and the scheduler's view never drifts from reality.
   */
  private async settleTurn(outcome: ReactionOutcome): Promise<void> {
    const ticket = this.ticket;
    this.ticket = null;
    this.options.thread.setBusy(false);

    if (ticket !== null && !this.options.scheduler.release(ticket)) {
      this.log.warn("a turn slot was already released", { outcome });
    }

    const messageId = this.currentMessageId;
    this.currentMessageId = null;
    this.currentAuthorId = null;
    if (messageId !== null) await this.options.thread.setReaction(messageId, outcome);
  }

  /**
   * Adds a turn's usage to the running total.
   *
   * The agent reports each turn's own cost. Context is the latest turn's input
   * rather than a sum, because it is what the model is carrying now.
   */
  private accrue(usage: Usage): void {
    this.usage = {
      input: this.usage.input + usage.input,
      output: this.usage.output + usage.output,
      cacheRead: this.usage.cacheRead + usage.cacheRead,
      cacheWrite: this.usage.cacheWrite + usage.cacheWrite,
      totalTokens: this.usage.totalTokens + usage.totalTokens,
      cost: this.usage.cost + usage.cost,
      contextTokens: usage.input + usage.cacheRead,
      ...(this.client?.contextWindow === undefined
        ? {}
        : { contextWindow: this.client.contextWindow }),
      turns: this.usage.turns + 1,
      model: usage.model ?? this.usage.model,
    };
    this.options.thread.setUsage(this.usage);
  }

  private buildHandlers(): AgentHandlers {
    return {
      onTurnStart: (): void => {
        this.resetIdleTimer();
      },
      onAssistantText: (text: string): void => {
        this.resetIdleTimer();
        void this.say(text);
      },
      onTurnSettled: (producedText: boolean): void => {
        void this.onSettled(producedText);
      },
      onToolStart: (id: string, toolName: string, target: string | undefined): void => {
        this.resetIdleTimer();
        void this.options.thread.appendActivity(toolLine(toolName, target), {
          id,
          name: toolName,
          target,
        });
        if (EDITING_TOOLS.has(toolName) && target !== undefined) this.snapshot(target);
      },
      onToolEnd: (id: string, toolName: string, failed: boolean, output: string): void => {
        if (!failed && EDITING_TOOLS.has(toolName)) void this.reportEdit(toolName, id);

        // Kept whole rather than truncated as the thread shows it: a
        // delegation about a log is worth nothing if it is asked about the
        // first page of one.
        if (id.length > 0) {
          this.outputs.set(id, output);
          while (this.outputs.size > MAX_REMEMBERED_OUTPUTS) {
            const oldest = this.outputs.keys().next();
            if (oldest.done === true) break;
            this.outputs.delete(oldest.value);
          }
        }

        // Reported whatever the thread is configured to forward, because a
        // surface that can fold output away has no reason to be spared it.
        this.options.thread.noteToolResult({
          id,
          name: toolName,
          failed,
          output: truncate(output, this.options.config.output.maxToolOutputChars),
        });
        // The result was reported once, above. Each surface decides whether to
        // show it, so it is not also posted here as though the agent had said
        // it, which would show it twice wherever it is already attached to the
        // call it came from.
        if (failed && !this.options.config.output.forwardToolOutput) {
          void this.options.thread.appendActivity(toolLine(toolName, "failed"), {
            id,
            name: toolName,
            failed: true,
          });
        }
      },
      onUsage: (usage: Usage): void => {
        this.accrue(usage);
      },
      onThought: (text: string): void => {
        this.options.thread.noteThinking(text);
      },
      onThinking: (): void => {
        this.resetIdleTimer();
      },
      onError: (detail: string): void => {
        void this.say(`the agent reported an error: ${detail}`);
      },
      onCommandRejected: (command: string, detail: string): void => {
        // The agent refused outright, so no turn follows and nothing else will
        // ever settle this. Reporting and settling here is what keeps the
        // thread from going quiet and the admission slot from leaking.
        void (async () => {
          await this.say(`the agent refused the ${command}: ${detail}`);
          await this.settleTurn("failed");
        })();
      },
      onRetry: (detail: string): void => {
        this.options.scheduler.noteRateLimit();
        void this.options.thread.postNotice(
          warningLine(`waiting on the model provider before continuing: ${detail}`),
          "warning",
        );
      },
      onDialog: (request: DialogRequest): void => {
        this.resetIdleTimer();
        void this.say(questionLine(dialogLines(request)));
      },
      onDialogTimeout: (): void => {
        void this.say(
          "the question went unanswered for too long and was cancelled; the session is still running",
        );
      },
      onUnsupportedDialog: (): void => {
        void this.say(
          warningLine(
            "the agent asked for a text editor, which a thread cannot provide; it was told to carry on without one",
          ),
        );
      },
      onProtocolViolation: (detail: string): void => {
        void this.endBecause("protocol violation", `the agent broke the protocol: ${detail}`);
      },
      onExit: (code: number): void => {
        void this.onExit(code);
      },
    };
  }

  private async onSettled(producedText: boolean): Promise<void> {
    this.options.scheduler.noteSuccess();
    this.harvestMemory();
    await this.openRequestedPullRequest();

    // The agent's own words were posted as it produced them. This closes the
    // turn and pings whoever asked, which is the point of driving it remotely.
    //
    // The turn's own author is cleared when a turn settles, so a settle that
    // arrives without one falls back to whoever last spoke rather than to
    // nobody. A queued message must not lose its ping.
    const who = this.currentAuthorId ?? this.lastSpeakerId ?? this.options.ownerId;
    const mention = `<@${who}> `;
    const spent = this.usage.turns > 0 ? ` ${usageSummary(this.usage)}` : "";

    await this.options.thread.postNotice(
      producedText
        ? `${mention}${marker("done")}${spent}`
        : `${mention}${marker("done")} the turn finished without producing any output${spent}`,
      "done",
    );

    await this.settleTurn(this.aborting ? "interrupted" : "succeeded");
    this.turnDelegations = null;
    this.aborting = false;
    this.resetIdleTimer();
  }

  /**
   * What the agent's dying words say went wrong, when they say anything.
   *
   * An exit code alone explains nothing: a process killed for filling the
   * disk and one that hit a bug both exit with 1. This was reported as "exit
   * code 1" once, for a host that had simply run out of space, which is a
   * thing somebody can fix and a thing they cannot guess.
   */
  private diagnose(words: string): string | undefined {
    if (/ENOSPC|no space left on device/i.test(words)) {
      return "the host it runs on has run out of disk space";
    }
    if (/ENOMEM|out of memory|Cannot allocate memory/i.test(words)) {
      return "the host it runs on has run out of memory";
    }
    if (/EACCES|permission denied/i.test(words)) {
      return "it was refused permission to something it needs";
    }
    return undefined;
  }

  private async onExit(code: number): Promise<void> {
    if (this.ended) return;

    const words = this.client?.dyingWords ?? "";
    const named = this.diagnose(words);
    if (named !== undefined) {
      await this.endBecause("resource limit", `this session stopped because ${named}`);
      return;
    }

    // 137 is SIGKILL, which is how a container killed for exceeding a limit
    // ends. Naming the limit beats reporting a generic crash.
    if (code === 137) {
      const sandbox = this.options.config.sandbox;
      await this.endBecause(
        "resource limit",
        `the session was terminated for exceeding a configured resource limit (memory ${sandbox.memory}, cpus ${sandbox.cpus}, pids ${sandbox.pids})`,
      );
      return;
    }

    // Whatever it last said, so a reader has something to act on rather than
    // a number.
    const said = firstLine(words.split("\n").filter((line) => line.trim().length > 0).pop() ?? "");
    await this.endBecause(
      "crashed",
      said.length === 0
        ? `this session ended unexpectedly with exit code ${code}`
        : `this session ended unexpectedly with exit code ${code}: ${said}`,
    );
  }

  /**
   * Writes the git configuration into the agent's own home.
   *
   * Into the home rather than the project: a configuration in the working tree
   * is one the agent could commit by accident, and it would follow the code
   * into whatever it opens a pull request against.
   */
  private writeGitConfig(github: GithubConfig | undefined): void {
    if (github === undefined) return;
    const home = join(this.options.stateDir, "home");
    try {
      Deno.mkdirSync(home, { recursive: true });
      Deno.writeTextFileSync(join(home, GITCONFIG_FILENAME), gitConfigContents(github), {
        mode: 0o600,
      });
    } catch (error) {
      this.log.warn("could not write the git configuration", { detail: String(error) });
    }
  }

  /**
   * Puts the wrappers a session runs in place of the real program.
   *
   * The directory is made whether or not there is anything to put in it, since
   * the sandbox names it on PATH and a session started before it existed would
   * otherwise be refused a directory that is not there.
   */
  private writeAgentBin(github: GithubConfig | undefined): void {
    const bin = join(this.options.stateDir, "home", "bin");
    const delegate = this.options.config.agent.delegate;
    try {
      Deno.mkdirSync(bin, { recursive: true });
      if (delegate !== undefined) {
        Deno.writeTextFileSync(
          join(bin, DELEGATE_COMMAND),
          delegateCommandContents(delegate.deadlineMs),
          { mode: 0o755 },
        );
      }
      if (github === undefined) return;
      Deno.writeTextFileSync(join(bin, GH_SHIM_FILENAME), ghShimContents(), { mode: 0o755 });
    } catch (error) {
      this.log.warn("could not write the agent's wrappers", { detail: String(error) });
    }
  }

  /**
   * Who a pull request from this session is on behalf of.
   *
   * Whoever asked for it, who is not always whose session it is: a thread
   * belongs to the person who opened it, and somebody else asking for a pull
   * request in it should have their own name on it.
   *
   * The remembered display name first, since a resumed session has no message
   * to take one from and would otherwise attribute the work to a bare account
   * id, which is no use to anyone reading the pull request. None of them is
   * written as a mention: see `reviewInstructions`.
   */
  private requestedBy(): string {
    const asked = this.whoAsked();
    if (asked !== undefined && asked.id !== this.options.ownerId) {
      return this.options.memory?.displayName(asked.id) ?? asked.name ?? asked.id;
    }
    return this.options.memory?.displayName(this.options.ownerId) ??
      this.options.ownerName ??
      this.options.ownerId;
  }

  /** Whoever asked for a pull request, which is not always whose session it is. */
  private whoAsked(): { id: string; name?: string } | undefined {
    let contents: string;
    try {
      contents = Deno.readTextFileSync(join(recordDir(this.options.stateDir), ASKED_FILENAME));
    } catch {
      return undefined;
    }
    const [id, name] = contents.split("\n");
    if (id === undefined || id.trim().length === 0) return undefined;
    return {
      id: id.trim(),
      ...(name !== undefined && name.trim().length > 0 ? { name: name.trim() } : {}),
    };
  }

  /** Where this session can be read back, for a pull request to point at. */
  private sessionLinks(): SessionLinks {
    const { threadId, guildId, publicUrl } = this.options;
    return {
      ...(threadId === undefined || guildId === undefined
        ? {}
        : { thread: threadLink(guildId, threadId) }),
      ...(publicUrl === undefined
        ? {}
        : { transcript: transcriptLink(publicUrl, this.options.id) }),
    };
  }

  /**
   * Writes what the agent should know before it starts, as its system prompt.
   *
   * @returns the path, or undefined when there is nothing to say and it would
   *   cost context for no benefit.
   */
  private writeMemoryBlock(): string | undefined {
    const memory = this.options.memory;
    if (memory === undefined) return undefined;

    const notesPath = join(this.options.stateDir, NOTES_FILENAME);
    const projectNotesPath = join(this.options.stateDir, PROJECT_NOTES_FILENAME);

    const about = [
      memory.render(this.options.ownerId),
      memory.renderProject(this.options.project.name),
    ].filter((part) => part.length > 0).join("\n\n");

    // Only when the agent can actually push. Telling it how to attribute a
    // pull request it has no credential to open is instruction for its own
    // sake.
    const github = this.options.config.github;
    const attribution = github === undefined
      ? ""
      : reviewInstructions(github, this.requestedBy(), this.sessionLinks());

    const delegate = this.options.config.agent.delegate;
    const delegating = delegate === undefined
      ? ""
      : delegateInstructions(delegate.model, delegate.perTurn);

    const contents = `${this.houseRules() ?? ""}${about}${
      memoryInstructions(
        `${STATE_PATH}/${NOTES_FILENAME}`,
        `${STATE_PATH}/${PROJECT_NOTES_FILENAME}`,
      )
    }${attribution}${delegating}`;

    const path = join(this.options.stateDir, BLOCK_FILENAME);
    try {
      // Written before the sandbox is launched, so nothing else has had reason
      // to create the directory yet.
      Deno.mkdirSync(this.options.stateDir, { recursive: true });
      Deno.writeTextFileSync(path, `${contents}\n`);
      // Created empty so the agent appends to a file it can see exists.
      for (const notes of [notesPath, projectNotesPath]) {
        Deno.writeTextFileSync(notes, "", { createNew: true });
      }
    } catch (error) {
      if (!(error instanceof Deno.errors.AlreadyExists)) {
        this.log.warn("could not write the memory block", { detail: String(error) });
        return undefined;
      }
    }
    return path;
  }

  /**
   * The operator's standing instructions, when a file of them is configured.
   *
   * Read per session rather than held from startup, so editing the file
   * reaches the next session without a restart. A file that has gone since
   * startup is a warning and not a refusal: the session in front of somebody
   * is worth more than the rules it would have carried, and the daemon already
   * refused to start if the path was wrong when it was read.
   */
  private houseRules(): string | undefined {
    const path = this.options.config.agent.rulesPath;
    if (path === undefined) return undefined;
    try {
      return rulesBlock(Deno.readTextFileSync(path));
    } catch (error) {
      this.log.warn("the house rules could not be read", { path, detail: String(error) });
      return undefined;
    }
  }

  /**
   * Prefixes a speaker's memory to their first message in this session.
   *
   * The system prompt was fixed when the sandbox started, so a person who
   * joins later cannot be introduced through it. Doing it once per speaker
   * keeps the cost off every turn.
   */
  private introduce(message: IncomingMessage): string {
    const memory = this.options.memory;
    if (memory === undefined) return message.content;
    if (this.introduced.has(message.authorId)) return message.content;

    this.introduced.add(message.authorId);
    if (message.authorName !== undefined) {
      memory.rememberUser(message.authorId, message.authorName);
    }

    // The owner was already introduced through the system prompt.
    if (message.authorId === this.options.ownerId) return message.content;

    const block = memory.renderForSpeaker(message.authorId);
    return block.length === 0 ? message.content : `${block}\n\n${message.content}`;
  }

  /**
   * Records a file's contents before an edit, so the change can be shown.
   *
   * Read from the host rather than asked of the agent: the daemon can see the
   * project directly, and a path the sandbox could not reach maps to nothing.
   */
  private snapshot(agentPath: string): void {
    const host = this.sandbox?.toHostPath(agentPath);
    if (host === undefined) return;
    try {
      const stat = Deno.statSync(host);
      if (!stat.isFile || stat.size > MAX_DIFFABLE_BYTES) return;
      this.pendingEdits.set(agentPath, Deno.readTextFileSync(host));
    } catch {
      // A file that does not exist yet is an empty one for diffing purposes.
      this.pendingEdits.set(agentPath, "");
    }
  }

  /** Posts what an edit changed, once the tool has finished. */
  private async reportEdit(toolName: string, call: string): Promise<void> {
    if (!this.options.config.output.postDiffs) {
      this.pendingEdits.clear();
      return;
    }

    for (const [agentPath, before] of this.pendingEdits) {
      this.pendingEdits.delete(agentPath);
      const host = this.sandbox?.toHostPath(agentPath);
      if (host === undefined) continue;

      let after: string;
      try {
        const stat = Deno.statSync(host);
        if (!stat.isFile || stat.size > MAX_DIFFABLE_BYTES) continue;
        after = Deno.readTextFileSync(host);
      } catch {
        continue;
      }

      const diff = fileDiff(before, after);
      if (diff.empty) continue;
      this.log.info("posting an edit", { tool: toolName, path: agentPath });
      await this.options.thread.postDiff(
        this.displayPath(agentPath),
        diff.added,
        diff.removed,
        diff.body,
        call,
      );
    }
  }

  /** A path as a reader would recognise it, relative to the project. */
  private displayPath(agentPath: string): string {
    const host = this.sandbox?.toHostPath(agentPath);
    if (host === undefined) return agentPath;
    const root = this.options.project.path;
    return host.startsWith(root) ? host.slice(root.length).replace(/^\/+/, "") : host;
  }

  /**
   * Lists a directory or shows a file, without involving the agent.
   *
   * The path is confined the same way an upload is, so this cannot read
   * anything the sandbox itself could not.
   */
  private async readPath(request: string, mode: "list" | "show"): Promise<void> {
    const wanted = request.trim().length === 0 ? "." : request.trim();
    const host = this.sandbox?.toHostPath(wanted);
    if (host === undefined) {
      await this.say(`\`${wanted}\` is not inside this session's project`);
      return;
    }

    const display = this.displayPath(wanted) || ".";
    try {
      // A directory is listed whichever was asked for: `!cat` on one is a
      // mistake worth answering rather than an error worth reporting.
      if (Deno.statSync(host).isDirectory) {
        await this.say(directoryListing(readDirectory(host, display), display));
        return;
      }
      if (mode === "list") {
        await this.say(fileView(readFileForDisplay(host, display)));
        return;
      }
      await this.say(fileView(readFileForDisplay(host, display)));
    } catch (error) {
      await this.say(`could not read \`${wanted}\`: ${reason(error)}`);
    }
  }

  /** Uploads a file from the project on request. */
  private async uploadFile(request: string): Promise<void> {
    const wanted = request.trim();
    if (wanted.length === 0) {
      await this.say("say which file, as `!file <path>`");
      return;
    }

    const host = this.sandbox?.toHostPath(wanted);
    if (host === undefined) {
      await this.say(`\`${wanted}\` is not inside this session's project`);
      return;
    }

    try {
      const stat = Deno.statSync(host);
      if (!stat.isFile) {
        await this.say(`\`${wanted}\` is not a file`);
        return;
      }
      if (stat.size > MAX_UPLOAD_BYTES) {
        await this.say(
          `\`${wanted}\` is ${Math.round(stat.size / 1024)} KB, larger than the upload limit`,
        );
        return;
      }
      await this.options.thread.upload(
        host.split("/").pop() ?? "file",
        Deno.readFileSync(host),
        `\`${this.displayPath(wanted)}\` ${stat.size} bytes`,
      );
    } catch (error) {
      await this.say(`could not read \`${wanted}\`: ${reason(error)}`);
    }
  }

  /** Reads anything the agent wrote to its notes files and stores it. */
  private harvestMemory(): void {
    const memory = this.options.memory;
    if (memory === undefined) return;

    // Attributed to whoever spoke this turn, not to the session's owner: in a
    // shared thread the facts being recorded are about the person talking.
    const about = this.lastSpeakerId ?? this.options.ownerId;
    const stored = this.harvest(NOTES_FILENAME, "user", about) +
      this.harvest(PROJECT_NOTES_FILENAME, "project", this.options.project.name);

    if (stored > 0) this.log.info("recorded facts from a turn", { stored, about });
  }

  /**
   * Reads one notes file and stores what it holds.
   *
   * The file is emptied rather than deleted, so the agent's next append lands
   * in a file it already knows exists and no line is ever ingested twice.
   */
  private harvest(filename: string, scope: "user" | "project", subject: string): number {
    const memory = this.options.memory;
    if (memory === undefined) return 0;

    const path = join(this.options.stateDir, filename);
    let contents: string;
    try {
      contents = Deno.readTextFileSync(path);
    } catch {
      return 0;
    }

    let stored = 0;
    for (const fact of parseNotes(contents)) {
      if (memory.remember(scope, subject, fact, this.options.id)) stored += 1;
    }
    try {
      Deno.writeTextFileSync(path, "");
    } catch {
      // A fact offered again is deduplicated, so this is not worth failing on.
    }
    return stored;
  }

  /** True when this account may change what the session is doing. */
  private mayControl(authorId: string): boolean {
    return authorId === this.options.ownerId || this.options.operatorIds.includes(authorId);
  }

  /** True when this account may prompt the agent and read the project. */
  private mayTakePart(authorId: string): boolean {
    return this.mayControl(authorId) || this.guests.has(authorId);
  }

  private standing(authorId: string): { isOwner: boolean; isGuest: boolean } {
    return { isOwner: this.mayControl(authorId), isGuest: this.guests.has(authorId) };
  }

  private notInvited(): string {
    return `<@${this.options.ownerId}> has not invited you to this thread; they can with \`!allow\``;
  }

  private async runCommand(word: string, rest: string, message: IncomingMessage): Promise<void> {
    this.replyingTo = word;
    try {
      await this.answerCommand(word, rest, message);
    } finally {
      this.replyingTo = null;
    }
  }

  /**
   * Who or what a memory command was aimed at.
   *
   * `project` names this session's project. Anything else is an account, given
   * as a mention or as a bare id. An empty argument means whoever asked, which
   * is what somebody means by "what do you know about me".
   */
  private memorySubject(
    rest: string,
    askedBy: string,
  ): { scope: Scope; subject: string; label: string } | undefined {
    const trimmed = rest.trim();
    if (trimmed.length === 0) return { scope: "user", subject: askedBy, label: `<@${askedBy}>` };
    if (trimmed.toLowerCase() === "project") {
      return {
        scope: "project",
        subject: this.options.project.name,
        label: `\`${this.options.project.name}\``,
      };
    }
    const id = parseUserId(trimmed);
    return id === undefined ? undefined : { scope: "user", subject: id, label: `<@${id}>` };
  }

  /** Reads back what the agent is told before it answers. */
  private async reportFacts(rest: string, message: IncomingMessage): Promise<void> {
    const memory = this.options.memory;
    if (memory === undefined) {
      await this.say("nothing is remembered on this host");
      return;
    }
    const target = this.memorySubject(rest, message.authorId);
    if (target === undefined) {
      await this.refuse(message, "say who, as `!facts @somebody`, or `!facts project`");
      return;
    }

    const facts = memory.factsFor(target.scope, target.subject);
    if (facts.length === 0) {
      await this.say(`nothing is remembered about ${target.label}`);
      return;
    }
    // Numbered the way they are held, so what a person reads back is what the
    // agent was given, newest first.
    const lines = facts.map((fact) => `- ${fact.fact}`).join("\n");
    await this.say(`remembered about ${target.label}:\n${lines}`);
  }

  /** Drops what is remembered, which changes every later session too. */
  private async forgetFacts(rest: string, message: IncomingMessage): Promise<void> {
    const memory = this.options.memory;
    if (memory === undefined) {
      await this.say("nothing is remembered on this host");
      return;
    }
    if (rest.trim().length === 0) {
      await this.refuse(message, "say who, as `!forget @somebody`, or `!forget project`");
      return;
    }
    const target = this.memorySubject(rest, message.authorId);
    if (target === undefined) {
      await this.refuse(message, "say who, as `!forget @somebody`, or `!forget project`");
      return;
    }

    const gone = memory.forget(target.scope, target.subject);
    await this.options.thread.setReaction(message.id, "accepted");
    await this.say(
      gone === 0
        ? `nothing was remembered about ${target.label}`
        : `forgot ${gone} fact${gone === 1 ? "" : "s"} about ${target.label}`,
    );
  }

  /** The provider this session runs on: what was asked for, else configured. */
  private provider(): string {
    return this.switched?.provider ?? this.options.chosen?.provider ??
      this.options.config.agent.provider;
  }

  /** The model this session runs on, on that provider. */
  private model(): string | undefined {
    return this.switched?.model ?? this.options.chosen?.model ?? this.options.config.agent.model;
  }

  private async answerCommand(
    word: string,
    rest: string,
    message: IncomingMessage,
  ): Promise<void> {
    const access = COMMANDS[word]?.access ?? "owner";
    if (!mayRun(access, this.standing(message.authorId))) {
      await this.refuse(
        message,
        access === "owner"
          ? `only <@${this.options.ownerId}>, who started this session, can use ${word}`
          : this.notInvited(),
      );
      return;
    }

    switch (word) {
      case "!stop":
        await this.options.thread.setReaction(message.id, "accepted");
        await this.noteCommand(message, "!stop");
        await this.stop("stopped");
        return;

      case "!interrupt": {
        if (!this.isBusy) {
          await this.say("there is nothing running to interrupt");
          return;
        }
        this.aborting = true;
        await this.options.thread.setReaction(message.id, "accepted");
        await this.noteCommand(message, "!interrupt");
        await this.abort();
        return;
      }

      case "!allow":
      case "!deny": {
        const target = parseUserId(rest);
        if (target === undefined) {
          await this.say(`say who, as \`${word} @user\``);
          return;
        }
        if (target === this.options.ownerId) {
          await this.say("the owner already takes part in their own thread");
          return;
        }

        if (word === "!allow") this.guests.add(target);
        else this.guests.delete(target);
        this.options.onGuestsChanged?.(this.guestList);

        await this.options.thread.setReaction(message.id, "accepted");
        await this.say(
          word === "!allow"
            ? `<@${target}> can now prompt this session and read its project`
            : `<@${target}> can no longer take part in this thread`,
        );
        return;
      }

      case "!guests": {
        const guests = this.guestList;
        await this.say(
          guests.length === 0
            ? `only <@${this.options.ownerId}> takes part in this thread`
            : `taking part: <@${this.options.ownerId}> and ${
              guests.map((id) => `<@${id}>`).join(", ")
            }`,
        );
        return;
      }

      case "!facts": {
        await this.reportFacts(rest, message);
        return;
      }

      case "!forget": {
        await this.forgetFacts(rest, message);
        return;
      }

      case "!pwd":
        await this.say(`\`${this.options.project.name}\` at \`${this.options.project.path}\``);
        return;

      case "!ls":
        await this.readPath(rest, "list");
        return;

      case "!cat":
        await this.readPath(rest, "show");
        return;

      case "!file":
        await this.options.thread.setReaction(message.id, "accepted");
        await this.uploadFile(rest);
        return;

      case "!pr":
        this.notePullRequestAsked(message);
        await this.openPullRequest(rest, message);
        return;

      case "!compact":
        await this.compactConversation(message);
        return;

      case "!model":
        await this.switchModel(rest, message);
        return;

      case "!help":
        await this.say(helpText());
        return;

      case "!then": {
        if (rest.trim().length === 0) {
          await this.say("say what to hold, as `!then <instruction>`");
          return;
        }
        // With nothing running there is nothing to wait for, so it starts a
        // turn rather than being refused for asking at the wrong moment.
        await this.submitPrompt(rest, message, { hold: true });
        return;
      }

      case "!steer": {
        if (rest.trim().length === 0) {
          await this.say("say what to steer towards, as `!steer <instruction>`");
          return;
        }
        if (!this.isBusy) {
          await this.say("there is no running turn to steer; send it as an ordinary message");
          return;
        }
        await this.noteCommand(message, `!steer ${rest}`);
        this.client?.steer(rest);
        await this.options.thread.setReaction(message.id, "accepted");
        return;
      }

      default:
        await this.say(this.describeStatus());
        return;
    }
  }

  /**
   * Shows which models this session can run on, or moves it to one.
   *
   * The conversation is kept across a switch: what was said stays said, and
   * the next turn is answered by the model named. That is what makes this
   * worth having, since the expensive model can work out what to do and a
   * cheaper one can carry out the rest of it in the same thread.
   *
   * Refused while a turn is running, because changing the model underneath a
   * turn would answer half a question with one model and half with another.
   */
  private async switchModel(rest: string, message: IncomingMessage): Promise<void> {
    const wanted = rest.trim();
    const available = this.options.availableModels ?? [];

    if (wanted.length === 0) {
      const running = this.usage.model ?? this.options.config.agent.model ?? "the provider default";
      await this.say(
        available.length === 0
          ? `this session runs on \`${running}\`; the host lists no others to switch to`
          : [
            `this session runs on \`${running}\`. Switch with \`!model <name>\`:`,
            ...available.map((model) => `  ${model}`),
          ].join("\n"),
      );
      return;
    }

    if (this.isBusy) {
      await this.say("a turn is running; wait for it, or stop it with `!interrupt`");
      await this.options.thread.setReaction(message.id, "failed");
      return;
    }

    // Refused rather than passed through, so a typo becomes a message here
    // instead of a turn that fails against the provider later.
    if (available.length > 0 && !available.includes(wanted)) {
      await this.say(`this host does not list a model called \`${wanted}\``);
      await this.options.thread.setReaction(message.id, "failed");
      return;
    }

    const sent = this.client?.setModel(this.provider(), wanted) ?? false;
    if (!sent) {
      await this.say("the agent is not accepting anything further; this session has ended");
      await this.options.thread.setReaction(message.id, "failed");
      return;
    }

    this.switched = { provider: this.provider(), model: wanted };
    this.options.onModelChanged?.(this.switched.provider, wanted);

    await this.noteCommand(message, `!model ${wanted}`);
    await this.say(connectionLine(`this session now runs on \`${wanted}\`, keeping what was said`));
    await this.options.thread.setReaction(message.id, "accepted");
  }

  private describeStatus(): string {
    const lines = [
      `project: ${this.options.project.name}`,
      `state: ${this.isBusy ? "running a turn" : "idle"}`,
      `turns in flight across all sessions: ${this.options.scheduler.turnsInFlight}`,
      `prompts waiting: ${this.options.scheduler.queueLength}`,
    ];

    if (this.delegated.asked > 0) {
      const { asked, answered, tokens, keptOut } = this.delegated;
      lines.push(
        `delegated: ${answered} of ${asked} asked, ${tokens} token(s) spent, ${
          bytes(keptOut)
        } kept out of this conversation`,
      );
    }
    return lines.join("\n");
  }

  /** Aborts the running turn, force stopping if the agent will not confirm. */
  private async abort(): Promise<void> {
    if (await this.requestAbort()) return;

    await this.say("the agent did not confirm the interruption, so the session was force stopped");
    await this.endBecause("stopped", "force stopped after an unconfirmed interruption");
  }

  private requestAbort(): Promise<boolean> {
    if (this.client === null) return Promise.resolve(false);
    this.client.abort();

    return new Promise<boolean>((resolve) => {
      const deadline = this.timers.setTimeout(
        () => resolve(false),
        this.options.config.timeouts.abortMs,
      );
      const check = (): void => {
        if (!this.isBusy || this.ended) {
          this.timers.clearTimeout(deadline);
          resolve(true);
          return;
        }
        this.timers.setTimeout(check, 50);
      };
      this.timers.setTimeout(check, 50);
    });
  }

  /**
   * Builds the delegations one turn may make, when a model is configured.
   *
   * The endpoint is the provider's own, so the cheaper model is reached with
   * the same credential over the same connection as the session's.
   */
  private newDelegations(): TurnDelegations | null {
    const delegate = this.options.config.agent.delegate;
    const baseUrl = delegate?.baseUrl ?? this.options.delegateBaseUrl;
    if (delegate === undefined || baseUrl === undefined) return null;

    return new TurnDelegations({
      sessionId: this.options.id,
      endpoint: {
        baseUrl,
        model: delegate.model,
        credential: this.options.config.agent.credential,
      },
      scheduler: this.options.scheduler,
      sources: {
        projectRoot: this.options.project.path,
        readFile: (path: string) => Promise.resolve(Deno.readTextFileSync(path)),
        outputOf: (callId: string) => this.outputs.get(callId),
        attachment: (name: string) => {
          // Held to the same containment as everything else, so a name that
          // climbs out of the attachments directory reads nothing.
          const path = hostPathUnder(
            this.options.project.path,
            this.options.project.path,
            `${ATTACHMENTS_DIR}/${name}`,
          );
          if (path === undefined) return undefined;
          try {
            return Deno.readTextFileSync(path);
          } catch {
            return undefined;
          }
        },
      },
      deadlineMs: delegate.deadlineMs,
      perTurn: delegate.perTurn,
    });
  }

  /** Begins answering the delegations the agent asks for. */
  private startDelegating(): void {
    if (this.options.config.agent.delegate === undefined) return;

    this.delegating = new Delegating({
      stateDir: this.options.stateDir,
      delegations: () => this.turnDelegations ?? undefined,
      report: (outcome) => this.noteDelegation(outcome),
      log: this.log,
      setTimeout: (handler, ms) => this.timers.setTimeout(handler, ms),
      clearTimeout: (handle) => this.timers.clearTimeout(handle),
    });
    this.delegating.start();
  }

  /**
   * Reports a delegation and keeps a running total of what it bought.
   *
   * The totals are what answers whether this is worth doing at all, so they
   * are kept where somebody can ask for them rather than derived later from a
   * log nobody keeps.
   */
  private noteDelegation(outcome: DelegationOutcome): void {
    this.delegated.asked += 1;

    if (isRefused(outcome)) {
      this.options.thread.noteDelegation({
        question: outcome.asked,
        refused: outcome.refused,
      });
      return;
    }

    this.delegated.answered += 1;
    this.delegated.tokens += outcome.tokens ?? 0;
    this.delegated.keptOut += outcome.keptOut;

    this.options.thread.noteDelegation({
      question: outcome.asked,
      model: outcome.model,
      describes: outcome.describes,
      answer: outcome.text,
      tokens: outcome.tokens,
      keptOut: outcome.keptOut,
    });
  }

  /**
   * Records what the project already held, then watches how much the session
   * adds to it.
   *
   * The baseline is taken once, before the agent can have written anything, so
   * the budget covers the session's own output rather than the size of the
   * repository it was pointed at.
   */
  private async startDiskWatch(): Promise<void> {
    const budget = parseSize(this.options.config.sandbox.disk);
    if (budget === undefined || budget <= 0) return;

    this.diskBaseline = await this.measureDisk();
    if (this.ended) return;
    this.diskLastAt = Date.now();
    // The first interval is short on purpose: nothing has been observed yet,
    // so there is no rate to pace against, and waiting the configured interval
    // is exactly the window a fast writer would use to pass the budget.
    this.scheduleDiskCheck(MIN_CHECK_MS);
  }

  private scheduleDiskCheck(ms: number): void {
    this.diskTimer = this.timers.setTimeout(() => {
      void this.checkDisk();
    }, ms);
  }

  private async measureDisk(): Promise<number> {
    const [project, state] = await Promise.all([
      treeBytes(this.options.project.path),
      treeBytes(this.options.stateDir),
    ]);
    return (project ?? 0) + (state ?? 0);
  }

  /**
   * Ends the session once it has written more than its budget.
   *
   * This is a measurement rather than a boundary: the agent can exceed the
   * budget between two checks, and nothing here can stop it mid-write. What it
   * does guarantee is that a session filling a disk stops rather than
   * continuing until the disk is full.
   */
  private async checkDisk(): Promise<void> {
    if (this.ended) return;

    const budget = parseSize(this.options.config.sandbox.disk) ?? 0;
    const written = Math.max(0, (await this.measureDisk()) - this.diskBaseline);
    if (this.ended) return;

    const state = verdict(written, budget);
    if (state === "over") {
      this.log.warn("session stopped for writing past its disk budget", { written, budget });
      await this.endBecause(
        "resource limit",
        `this session stopped after writing ${bytes(written)}, past its ${bytes(budget)} budget`,
      );
      return;
    }

    if (state === "close" && !this.diskWarned) {
      this.diskWarned = true;
      await this.options.thread.postNotice(
        `this session has written ${bytes(written)} of its ${
          bytes(budget)
        } budget, and ends if it passes it`,
        "warning",
      );
    }

    const now = Date.now();
    const next = nextCheckMs(
      written,
      this.diskLastWritten,
      budget,
      now - this.diskLastAt,
      this.options.config.sandbox.diskCheckMs,
    );
    this.diskLastWritten = written;
    this.diskLastAt = now;
    this.scheduleDiskCheck(next);
  }

  /**
   * Records a command that changed the agent's course.
   *
   * As a prompt, because that is what it is from the conversation's point of
   * view: somebody said something and what the agent did next changed. A
   * transcript that showed a turn simply stopping, with nothing to say why,
   * reads as the agent having given up.
   *
   * Only the commands that reach the agent, and only once they have taken
   * effect. A refused interrupt changed nothing and belongs nowhere.
   */
  private async noteCommand(message: IncomingMessage, text: string): Promise<void> {
    await this.options.thread.notePrompt(message.authorName ?? message.authorId, text);
  }

  /** Records that a pull request was asked for, and by whom, for a resume. */
  private notePullRequestAsked(message: IncomingMessage): void {
    try {
      Deno.mkdirSync(this.options.stateDir, { recursive: true });
      Deno.writeTextFileSync(
        join(prepareRecordDir(this.options.stateDir), ASKED_FILENAME),
        `${message.authorId}\n${message.authorName ?? ""}\n`,
      );
    } catch (error) {
      this.log.warn("could not record that a pull request was asked for", {
        detail: String(error),
      });
    }
  }

  /**
   * Opens the pull request the agent asked for, if it asked for one.
   *
   * The request is a file in the state directory, which is the one place both
   * sides can reach, so the agent finishes the job by saying so rather than by
   * pushing. The file is removed either way: a request that fails must not be
   * retried on every turn that follows it.
   *
   * Honoured only when somebody in the thread asked for a pull request. The
   * instruction to wait for that has been ignored, and an unasked-for pull
   * request is somebody else's review time, so this does not rely on it alone.
   */
  private async openRequestedPullRequest(): Promise<void> {
    const path = join(this.options.stateDir, REQUEST_FILENAME);
    let contents: string;
    try {
      contents = Deno.readTextFileSync(path);
    } catch {
      return;
    }

    try {
      Deno.removeSync(path);
    } catch (error) {
      this.log.warn("could not clear the pull request request", { detail: String(error) });
    }

    if (this.whoAsked() === undefined) {
      this.log.warn("ignored a pull request nobody asked for");
      await this.say(
        "a pull request was asked for by the agent, not by anyone here, so it was ignored. Ask for one and it will go through",
      );
      return;
    }

    const lines = contents.split("\n");
    const title = (lines[0] ?? "").trim();
    const repository = lines.slice(1)
      .map((line) => /^repository:\s*(.+)$/.exec(line.trim())?.[1]?.trim())
      .find((name) => name !== undefined && name.length > 0);

    if (title.length === 0) {
      await this.say("a pull request was asked for without a title, so none was opened");
      return;
    }
    await this.openPullRequestNow(title, repository);
  }

  /** Opens one on request, reporting the outcome on the message that asked. */
  private async openPullRequest(title: string, message: IncomingMessage): Promise<void> {
    if (this.options.config.github === undefined) {
      await this.say("no GitHub identity is configured, so there is nowhere to open one");
      await this.options.thread.setReaction(message.id, "failed");
      return;
    }
    if (title.trim().length === 0) {
      await this.say("say what to call it, as `!pr <title>`");
      await this.options.thread.setReaction(message.id, "failed");
      return;
    }

    await this.options.thread.setReaction(message.id, "accepted");
    const opened = await this.openPullRequestNow(title.trim());
    await this.options.thread.setReaction(message.id, opened ? "succeeded" : "failed");
  }

  /** Opens one, reporting either the address or why it did not. */
  private async openPullRequestNow(title: string, repository?: string): Promise<boolean> {
    const github = this.options.config.github;
    const open = this.options.openPullRequest;
    if (github === undefined || open === undefined) {
      await this.say("no GitHub identity is configured, so there is nowhere to open one");
      return false;
    }

    try {
      const url = await open({
        github,
        projectPath: this.options.project.path,
        ...(repository === undefined ? {} : { repository }),
        title,
        requestedBy: this.requestedBy(),
        links: this.sessionLinks(),
      });
      await this.say(connectionLine(`opened ${url}`));
      return true;
    } catch (error) {
      this.log.warn("could not open a pull request", { detail: String(error) });
      await this.say(reason(error));
      return false;
    }
  }

  /**
   * Summarises the conversation so far, freeing context to carry on in.
   *
   * Refused while a turn is running: compacting underneath a turn would change
   * the conversation the agent is part way through answering about.
   */
  private async compactConversation(message: IncomingMessage): Promise<void> {
    if (this.isBusy) {
      await this.say("a turn is running; wait for it, or stop it with `!interrupt`");
      await this.options.thread.setReaction(message.id, "failed");
      return;
    }

    const client = this.client;
    if (client === null) {
      await this.say("this session has no agent to compact");
      await this.options.thread.setReaction(message.id, "failed");
      return;
    }

    await this.options.thread.setReaction(message.id, "accepted");
    await this.noteCommand(message, "!compact");
    try {
      const answer = await client.compact(this.options.config.timeouts.questionMs);
      await this.say(compactionLine(answer));
      await this.options.thread.setReaction(message.id, "succeeded");
    } catch (error) {
      this.log.warn("compaction failed", { detail: String(error) });
      await this.say(`compaction did not finish: ${reason(error)}`);
      await this.options.thread.setReaction(message.id, "failed");
    }
  }

  /**
   * Turns a message down, explaining the first time and reacting every time.
   *
   * Answered as a reply rather than said: a refusal is addressed to the person
   * who tripped it, not to the session. Posting it would record it and show it
   * in an interface as though the agent had said it, which is both untrue and
   * noise in a conversation the refused message never joined.
   */
  private async refuse(message: IncomingMessage, why: string): Promise<void> {
    if (!this.explained.has(message.authorId)) {
      this.explained.add(message.authorId);
      await this.options.thread.postReply(why, "refused");
    }
    await this.options.thread.setReaction(message.id, "failed");
  }

  private resetIdleTimer(): void {
    this.lastActive = Date.now();
    if (this.idleTimer !== null) this.timers.clearTimeout(this.idleTimer);
    this.idleTimer = this.timers.setTimeout(() => {
      void this.endBecause("idle", "nothing happened for a while, so this session stopped");
    }, this.options.config.timeouts.idleMs);
  }

  /**
   * Ends the session, saying so only when there is something to say.
   *
   * A session that idles out, or that goes down with the daemon, is picked up
   * again by the next message in its thread, and the resumed session announces
   * itself. Announcing the pause as well would be a message in every thread
   * that says nothing a reader has to act on.
   *
   * A failure is different: it stopped part way through something, and
   * somebody should know why. And a thread archived from outside is left
   * alone, because posting into it would open it again, which is the opposite
   * of what whoever archived it asked for.
   */
  private async endBecause(why: EndReason, detail: string): Promise<void> {
    if (this.ended) return;

    if (!QUIET_ENDINGS.has(why)) {
      await this.options.thread.postNotice(connectionLine(detail), "ended");
    }
    await this.finish(why);
  }

  /**
   * Tears everything down exactly once: pending dialogs, the turn slot, the
   * queued prompts, the sandbox, and the session reservation.
   */
  private async finish(why: EndReason): Promise<void> {
    if (this.ended) return;
    this.ended = true;

    if (this.idleTimer !== null) this.timers.clearTimeout(this.idleTimer);
    this.idleTimer = null;
    if (this.diskTimer !== null) this.timers.clearTimeout(this.diskTimer);
    this.diskTimer = null;
    this.delegating?.stop();
    this.delegating = null;

    this.client?.cancelDialogs();
    await this.settleTurn(why === "stopped" ? "interrupted" : "failed");
    this.options.scheduler.cancelSession(this.options.id);

    try {
      await this.sandbox?.stop();
    } catch (error) {
      this.log.warn("tearing down the sandbox failed", { detail: String(error) });
    }

    this.options.scheduler.releaseSession();
    await this.options.thread.setWaiting(null);
    await this.options.thread.close(why).catch(() => undefined);
    this.log.info("session ended", { reason: why });
    this.options.onEnded(why);
  }
}
