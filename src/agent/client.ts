/**
 * Drives one agent process: commands out, events in.
 *
 * The client owns the protocol only. It knows nothing about chat, threads, or
 * admission, so it can be exercised against a fake process in tests.
 *
 * Turn completion is taken from `agent_settled`, not from `agent_end`. An
 * `agent_end` may be followed by an automatic retry, so releasing an admission
 * slot on it would let more work into the provider than the cap allows.
 */

import type { Logger } from "../log.ts";
import { LineFramer, RecordTooLargeError } from "./framing.ts";
import {
  type AgentCommand,
  type AgentImage,
  type AgentRecord,
  asDialogRequest,
  type DialogRequest,
  type DialogResponse,
  isFireAndForget,
  messageRole,
  messageText,
  startsThinking,
  type StreamingBehavior,
  thinkingEnded,
  toolTarget,
  type Usage,
  usageOf,
} from "./protocol.ts";

/**
 * Where the agent is in its life.
 *
 * One value rather than a set of booleans, because the interesting questions
 * are about combinations: whether a command may be sent, and whether a turn
 * was in flight when the process died. Two booleans can hold a state that
 * cannot happen, and then something has to decide what it means.
 *
 * ```
 *  starting --ready--> ready <--settled--> working
 *      |                 |                    |
 *      +-----------------+--------exit--------+--> ended
 * ```
 */
export type AgentState = "starting" | "ready" | "working" | "ended";

/** The process the client speaks to. Abstracted so tests need no sandbox. */
export interface AgentProcess {
  readonly stdout: ReadableStream<Uint8Array>;
  readonly stderr: ReadableStream<Uint8Array>;
  /** Writes to the agent's stdin. Throws when the process is gone. */
  write(bytes: Uint8Array): void;
  /** Resolves with the exit code once the process ends. */
  readonly exited: Promise<number>;
}

/** Everything the client reports outward. Every field is optional. */
export interface AgentHandlers {
  /** A turn started. */
  onTurnStart?: () => void;
  /**
   * The assistant finished saying something, reported as it happens so that it
   * interleaves with tool activity in the order the agent produced it.
   */
  onAssistantText?: (text: string) => void;
  /**
   * A turn settled, with no retry or queued continuation pending.
   *
   * @param producedText whether the assistant said anything during the turn.
   */
  onTurnSettled?: (producedText: boolean) => void;
  /** The agent began a tool call. */
  onToolStart?: (id: string, toolName: string, target: string | undefined) => void;
  /** A tool call finished, with whether it failed. */
  onToolEnd?: (id: string, toolName: string, failed: boolean, output: string) => void;
  /** The agent is thinking. Reported once per turn. */
  onThinking?: () => void;
  /** What the agent reasoned, once a block of it finished. */
  onThought?: (text: string) => void;
  /** What a finished turn cost, when the agent reported it. */
  onUsage?: (usage: Usage) => void;
  /** The agent reported an error. */
  onError?: (detail: string) => void;
  /** The agent refused a command outright, so no turn will follow it. */
  onCommandRejected?: (command: string, detail: string) => void;
  /** The agent began an automatic retry, which is the rate limit signal. */
  onRetry?: (detail: string) => void;
  /** The agent is blocked on a dialog. */
  onDialog?: (request: DialogRequest) => void;
  /** A dialog went unanswered long enough that it was cancelled. */
  onDialogTimeout?: (request: DialogRequest) => void;
  /** The agent asked for an interaction a chat thread cannot serve. */
  onUnsupportedDialog?: (method: string) => void;
  /**
   * The process ended. Reported exactly once.
   *
   * @param duringTurn true when a turn was running, so the caller knows the
   *   turn will never settle and can release what it was holding.
   */
  onExit?: (code: number, duringTurn: boolean) => void;
  /** The protocol was violated and the session cannot continue. */
  onProtocolViolation?: (detail: string) => void;
}

/** What happened to a reply offered for a pending dialog. */
export type AnswerOutcome =
  /** Accepted and sent to the agent. */
  | "accepted"
  /** Neither an option nor a yes or no; the dialog still stands. */
  | "unrecognized"
  /** No dialog with that id is pending. */
  | "unknown";

/** How much of a dying process's stderr is kept, in characters. */
const STDERR_KEPT = 2_000;

const AFFIRMATIVE = new Set(["yes", "y", "true", "ok", "confirm"]);
const NEGATIVE = new Set(["no", "n", "false", "cancel", "deny"]);

interface PendingDialog {
  request: DialogRequest;
  timer: ReturnType<typeof setTimeout>;
}

interface PendingRequest {
  resolve: (record: AgentRecord) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

/**
 * Turns a reply into a protocol response, or null when the reply does not
 * answer the question that was asked.
 */
function buildResponse(request: DialogRequest, reply: string): DialogResponse | null {
  const trimmed = reply.trim();

  if (request.method === "confirm") {
    const lowered = trimmed.toLowerCase();
    if (AFFIRMATIVE.has(lowered)) {
      return { type: "extension_ui_response", id: request.id, confirmed: true };
    }
    if (NEGATIVE.has(lowered)) {
      return { type: "extension_ui_response", id: request.id, confirmed: false };
    }
    return null;
  }

  if (request.method === "select") {
    const options = request.options ?? [];
    const index = Number.parseInt(trimmed, 10);
    if (Number.isInteger(index) && index >= 1 && index <= options.length) {
      return { type: "extension_ui_response", id: request.id, value: options[index - 1] as string };
    }
    const matched = options.find((option) => option.toLowerCase() === trimmed.toLowerCase());
    if (matched !== undefined) {
      return { type: "extension_ui_response", id: request.id, value: matched };
    }
    return null;
  }

  if (trimmed.length === 0) return null;
  return { type: "extension_ui_response", id: request.id, value: trimmed };
}

function detailOf(record: AgentRecord): string {
  for (const candidate of [record.error, record.message, record.reason]) {
    if (typeof candidate === "string" && candidate.length > 0) return candidate;
  }
  return String(record.type ?? "unknown");
}

/** Speaks the agent protocol over one process's pipes. */
export class AgentClient {
  private readonly framer: LineFramer;
  private readonly encoder = new TextEncoder();
  private lifecycle: AgentState = "starting";
  private exitReported = false;
  private thinkingReported = false;
  private lastWords = "";
  private producedText = false;

  private readonly dialogs = new Map<string, PendingDialog>();
  private readonly requests = new Map<string, PendingRequest>();
  private nextRequestId = 1;

  /**
   * How many tokens the model can hold, once the agent has said.
   *
   * Learned from the readiness call rather than asked for separately, since
   * that call already returns the model and is made on every session.
   */
  contextWindow: number | undefined;

  constructor(
    private readonly process: AgentProcess,
    private readonly handlers: AgentHandlers,
    private readonly log: Logger,
    private readonly dialogTimeoutMs: number = 300_000,
    maxRecordBytes?: number,
  ) {
    this.framer = new LineFramer(maxRecordBytes);
  }

  /**
   * The last thing the agent said on stderr before it went.
   *
   * Kept because an exit code on its own explains nothing: a process killed
   * for filling the disk and one that hit a bug both exit with 1, and only
   * this says which. Bounded, since a crash can print a great deal.
   */
  get dyingWords(): string {
    return this.lastWords;
  }

  /** Where the agent is in its life. */
  get state(): AgentState {
    return this.lifecycle;
  }

  /** False once the process has ended or a write has failed. */
  get isAlive(): boolean {
    return this.lifecycle !== "ended";
  }

  /** True while a turn is running. */
  get isWorking(): boolean {
    return this.lifecycle === "working";
  }

  /** Begins reading both streams. Resolves when the process has ended. */
  async run(): Promise<void> {
    await Promise.all([this.readStdout(), this.readStderr()]);
    this.end(await this.process.exited);
  }

  /** Waits until the agent answers, which is what readiness means here. */
  async waitUntilReady(timeoutMs: number): Promise<void> {
    const answer = await this.request({ type: "get_state" }, timeoutMs);
    const data = (answer as { data?: { model?: { contextWindow?: unknown } } }).data;
    const window = data?.model?.contextWindow;
    if (typeof window === "number" && window > 0) this.contextWindow = window;
    if (this.lifecycle === "starting") this.lifecycle = "ready";
  }

  /** Sends a prompt, queueing it behind a running turn when one exists. */
  prompt(
    message: string,
    options: { images?: AgentImage[]; behavior?: StreamingBehavior } = {},
  ): boolean {
    const command: AgentCommand = { type: "prompt", message };
    if (options.images !== undefined) command.images = options.images;
    if (options.behavior !== undefined) command.streamingBehavior = options.behavior;
    return this.send(command);
  }

  /** Redirects the turn that is already running. */
  steer(message: string, images: AgentImage[] = []): boolean {
    return this.send({ type: "steer", message, ...(images.length === 0 ? {} : { images }) });
  }

  /** Queues a message for after the running turn finishes. */
  followUp(message: string): boolean {
    return this.send({ type: "follow_up", message });
  }

  /**
   * Switches the model the session runs on, from this turn onward.
   *
   * The conversation is kept: what was said stays said, and the next turn is
   * answered by the model named here. That is the point of switching rather
   * than starting again.
   */
  setModel(provider: string, modelId: string): boolean {
    return this.send({ type: "set_model", provider, modelId });
  }

  /** Asks the agent to stop the running turn. */
  abort(): boolean {
    return this.send({ type: "abort" });
  }

  /**
   * Asks the agent to summarise the conversation so far.
   *
   * Sent as a request rather than fired off, because the useful part is the
   * answer: how much context it freed, which is the only way to tell a
   * compaction that did something from one that did not.
   */
  compact(timeoutMs: number): Promise<AgentRecord> {
    return this.request({ type: "compact" }, timeoutMs);
  }

  /** Answers a dialog the agent is blocked on. */
  respondToDialog(response: DialogResponse): boolean {
    return this.send(response);
  }

  /** The dialog currently blocking the agent, when there is one. */
  get pendingDialog(): DialogRequest | undefined {
    for (const entry of this.dialogs.values()) return entry.request;
    return undefined;
  }

  /**
   * Offers a reply as the answer to a pending dialog.
   *
   * An unrecognized reply leaves the dialog pending, so the caller can repeat
   * the question rather than sending the agent something it did not ask for.
   */
  answerDialog(id: string, reply: string): AnswerOutcome {
    const entry = this.dialogs.get(id);
    if (entry === undefined) return "unknown";

    const response = buildResponse(entry.request, reply);
    if (response === null) return "unrecognized";

    clearTimeout(entry.timer);
    this.dialogs.delete(id);
    this.send(response);
    return "accepted";
  }

  /**
   * Cancels every pending dialog, so nothing is left waiting when a session
   * ends or the daemon shuts down.
   */
  cancelDialogs(): void {
    for (const [id, entry] of this.dialogs) {
      clearTimeout(entry.timer);
      this.respondToDialog({ type: "extension_ui_response", id, cancelled: true });
    }
    this.dialogs.clear();
  }

  /**
   * Sends a command carrying a correlation id and waits for its response.
   *
   * Used to establish readiness: the agent is ready exactly when it answers,
   * which is more reliable than watching for an event or sleeping.
   */
  request(command: Record<string, unknown>, timeoutMs: number): Promise<AgentRecord> {
    const id = `rq-${this.nextRequestId}`;
    this.nextRequestId += 1;

    return new Promise<AgentRecord>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.requests.delete(id);
        reject(new Error(`the agent did not answer ${String(command.type)} within ${timeoutMs}ms`));
      }, timeoutMs);

      this.requests.set(id, { resolve, reject, timer });
      if (!this.send({ ...command, id } as unknown as AgentCommand)) {
        clearTimeout(timer);
        this.requests.delete(id);
        reject(new Error("the agent is not accepting commands"));
      }
    });
  }

  private end(code: number): void {
    const duringTurn = this.lifecycle === "working";
    this.lifecycle = "ended";
    if (this.exitReported) return;
    this.exitReported = true;
    this.failPendingRequests(`the agent exited with code ${code}`);
    this.cancelDialogs();
    this.handlers.onExit?.(code, duringTurn);
  }

  private send(command: AgentCommand | DialogResponse): boolean {
    if (this.lifecycle === "ended") {
      this.log.warn("dropped a command for an agent that has ended", { command: command.type });
      return false;
    }
    try {
      this.process.write(this.encoder.encode(`${JSON.stringify(command)}\n`));
      return true;
    } catch (error) {
      this.lifecycle = "ended";
      this.log.warn("writing to the agent failed", { detail: String(error) });
      return false;
    }
  }

  private async readStdout(): Promise<void> {
    const reader = this.process.stdout.getReader();
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        if (value === undefined) continue;

        let records: string[];
        try {
          records = this.framer.push(value);
        } catch (error) {
          if (error instanceof RecordTooLargeError) {
            this.lifecycle = "ended";
            this.handlers.onProtocolViolation?.(error.message);
            return;
          }
          throw error;
        }
        for (const record of records) this.dispatch(record);
      }
    } catch (error) {
      this.log.warn("agent stdout ended with an error", { detail: String(error) });
    } finally {
      reader.releaseLock();
    }
  }

  private async readStderr(): Promise<void> {
    const reader = this.process.stderr.getReader();
    const decoder = new TextDecoder("utf-8");
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        if (value === undefined) continue;
        const text = decoder.decode(value, { stream: true }).trimEnd();
        if (text.length === 0) continue;
        this.log.warn("agent stderr", { detail: text });
        this.lastWords = `${this.lastWords}\n${text}`.slice(-STDERR_KEPT).trimStart();
      }
    } catch {
      // A closed stderr is not a session failure; stdout decides that.
    } finally {
      reader.releaseLock();
    }
  }

  private dispatch(line: string): void {
    if (line.trim().length === 0) return;

    let record: AgentRecord;
    try {
      const parsed: unknown = JSON.parse(line);
      if (typeof parsed !== "object" || parsed === null) throw new Error("not an object");
      record = parsed as AgentRecord;
    } catch (error) {
      this.log.warn("agent sent an unparseable line", {
        detail: String(error),
        length: line.length,
      });
      return;
    }

    if (this.dispatchDialog(record)) return;
    if (this.dispatchResponse(record)) return;
    this.dispatchEvent(record);
  }

  /** @returns true when the record was a dialog and has been handled. */
  private dispatchDialog(record: AgentRecord): boolean {
    const dialog = asDialogRequest(record);
    if (dialog === null) {
      return record.type === "extension_ui_request" && isFireAndForget(record.method);
    }

    if (dialog.method === "editor") {
      this.handlers.onUnsupportedDialog?.(dialog.method);
      this.respondToDialog({ type: "extension_ui_response", id: dialog.id, cancelled: true });
      return true;
    }

    const timer = setTimeout(() => {
      this.dialogs.delete(dialog.id);
      this.respondToDialog({ type: "extension_ui_response", id: dialog.id, cancelled: true });
      this.handlers.onDialogTimeout?.(dialog);
    }, this.dialogTimeoutMs);
    this.dialogs.set(dialog.id, { request: dialog, timer });
    this.handlers.onDialog?.(dialog);
    return true;
  }

  /** @returns true when the record was a response and has been handled. */
  private dispatchResponse(record: AgentRecord): boolean {
    if (record.type !== "response") return false;

    const id = typeof record.id === "string" ? record.id : undefined;
    const waiting = id === undefined ? undefined : this.requests.get(id);
    if (waiting !== undefined && id !== undefined) {
      clearTimeout(waiting.timer);
      this.requests.delete(id);
      waiting.resolve(record);
      return true;
    }

    // A command sent without a correlation id, such as a prompt, still gets a
    // response, and a rejection there is the whole outcome of that command.
    // Dropping it leaves the thread waiting on a turn that will never run.
    if (record.success === false) {
      this.handlers.onCommandRejected?.(String(record.command ?? "command"), detailOf(record));
    }
    return true;
  }

  private dispatchEvent(record: AgentRecord): void {
    switch (record.type) {
      case "agent_start":
        this.lifecycle = "working";
        this.thinkingReported = false;
        this.producedText = false;
        this.handlers.onTurnStart?.();
        return;

      case "turn_end": {
        const usage = usageOf(record);
        if (usage !== undefined) this.handlers.onUsage?.(usage);
        return;
      }

      case "agent_settled":
        if (this.lifecycle === "working") this.lifecycle = "ready";
        this.handlers.onTurnSettled?.(this.producedText);
        this.producedText = false;
        return;

      case "message_update": {
        if (!this.thinkingReported && startsThinking(record)) {
          this.thinkingReported = true;
          this.handlers.onThinking?.();
        }
        const thought = thinkingEnded(record);
        if (thought !== undefined && thought.trim().length > 0) {
          this.handlers.onThought?.(thought);
        }
        return;
      }

      case "message_end": {
        // Only the assistant's own words. The agent also emits message_end for
        // the user's message, and collecting that would echo the prompt back
        // into the thread it came from.
        if (messageRole(record.message) !== "assistant") return;
        const text = messageText(record.message).trim();
        if (text.length === 0) return;
        // Reported now rather than accumulated: an agent that speaks, runs a
        // tool, then speaks again must read in that order.
        this.producedText = true;
        this.handlers.onAssistantText?.(text);
        return;
      }

      case "tool_execution_start":
        this.handlers.onToolStart?.(
          String(record.toolCallId ?? ""),
          String(record.toolName ?? "tool"),
          toolTarget(record.args),
        );
        return;

      case "tool_execution_end":
        this.handlers.onToolEnd?.(
          String(record.toolCallId ?? ""),
          String(record.toolName ?? "tool"),
          record.isError === true,
          // The agent reports a tool's output under `result`, as content
          // parts. Reading the record itself finds nothing and silently yields
          // an empty output.
          messageText(record.result) || messageText(record) || String(record.output ?? ""),
        );
        return;

      case "auto_retry_start":
        this.handlers.onRetry?.(detailOf(record));
        return;

      case "extension_error":
        this.handlers.onError?.(detailOf(record));
        return;

      default:
        return;
    }
  }

  private failPendingRequests(reason: string): void {
    for (const [id, waiting] of this.requests) {
      clearTimeout(waiting.timer);
      this.requests.delete(id);
      waiting.reject(new Error(reason));
    }
  }
}
