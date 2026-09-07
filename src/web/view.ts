/**
 * A connected browser, as a view of a session.
 *
 * Implements the same interface a chat thread does, so the session layer does
 * not know or care which it is talking to. Everything it is told becomes an
 * event on one server-sent stream.
 *
 * Nothing here blocks: a browser that has stopped reading must not hold up the
 * session or the other views, so a full queue drops rather than waits.
 */

import type {
  EndReason,
  NoticeLevel,
  SessionUsage,
  ThreadPort,
  ToolActivity,
  ToolResult,
} from "../session/port.ts";

/**
 * One thing to show, as the interface receives it.
 *
 * Every entry carries the turn it belongs to, except in a session recorded
 * before turns were kept, where there is none to carry.
 */
export type WireEntry =
  & { turn?: number }
  & (
    | { kind: "message"; text: string; at: number }
    | { kind: "prompt"; author: string; text: string; at: number }
    | { kind: "aside"; author: string; text: string; at: number }
    | { kind: "notice"; text: string; level: NoticeLevel; at: number }
    | { kind: "thinking"; text: string; at: number }
    | { kind: "reply"; text: string; command: string; at: number }
    | { kind: "toolResult"; result: ToolResult; at: number }
    | { kind: "activity"; line: string; tool?: ToolActivity; at: number }
    | { kind: "file"; name: string; size: number; at: number }
    | {
      kind: "diff";
      path: string;
      added: number;
      removed: number;
      body: string;
      /** The tool call that made the change, so it is shown on it. */
      cause?: string;
      at: number;
    }
  );

/** A change to how the session should be displayed. */
export interface WireState {
  busy?: boolean;
  waiting?: string | null;
  ended?: boolean;
  usage?: SessionUsage;
}

/** How much is held for a browser that has stopped reading before dropping. */
const MAX_QUEUED = 1_000;

/** Looks up who an account id belongs to. */
export type NameLookup = (id: string) => string | undefined;

/**
 * Rewrites chat mentions for a reader who is not in the chat service.
 *
 * A mention is markup that the service turns into a name. Here it would show
 * as `<@1234> [done]`, so a known id becomes the name and an unknown one is
 * dropped rather than shown as a number nobody can place.
 */
export function withoutMentions(text: string, names?: NameLookup): string {
  return text
    .replace(/<@!?(\d+)>\s*/g, (_whole, id: string) => {
      const known = names?.(id);
      return known === undefined ? "" : `@${known} `;
    })
    .replace(/<@!?[^>]+>\s*/g, "")
    .trim();
}

/** A browser attached to one session. */
export class WebView implements ThreadPort {
  private controller: ReadableStreamDefaultController<Uint8Array> | null = null;
  private readonly encoder = new TextEncoder();
  private queued = 0;
  private closed = false;
  /** The turn being received, stamped on everything sent to the browser. */
  private turn: number | undefined;

  /** The stream handed to the browser. */
  readonly body: ReadableStream<Uint8Array>;

  constructor(private readonly names?: NameLookup) {
    this.body = new ReadableStream<Uint8Array>({
      start: (controller) => {
        this.controller = controller;
        // Tells the interface to clear what it has, because everything it is
        // about to receive is the session's record from the beginning. A
        // reconnecting browser therefore ends up correct rather than doubled.
        this.send("reset", {});
      },
      cancel: () => {
        this.closed = true;
        this.controller = null;
      },
    });
  }

  /** True once the browser has gone, so the view can be detached. */
  get isClosed(): boolean {
    return this.closed;
  }

  private send(event: string, payload: unknown): void {
    if (this.closed || this.controller === null) return;
    if (this.queued > MAX_QUEUED) return;

    try {
      this.controller.enqueue(
        this.encoder.encode(`event: ${event}\ndata: ${JSON.stringify(payload)}\n\n`),
      );
      this.queued += 1;
    } catch {
      // The browser went away between the check and the write.
      this.closed = true;
      this.controller = null;
    }
  }

  private entry(entry: WireEntry): void {
    this.send("entry", this.turn === undefined ? entry : { ...entry, turn: this.turn });
  }

  private state(state: WireState): void {
    this.send("state", state);
  }

  /** Ends the stream, which is how a detached view stops the browser waiting. */
  stop(): void {
    if (this.closed) return;
    this.closed = true;
    try {
      this.controller?.close();
    } catch {
      // Already closed by the browser disconnecting.
    }
    this.controller = null;
  }

  beginTurn(turn: number): void {
    this.turn = turn;
  }

  post(text: string): Promise<void> {
    this.entry({ kind: "message", text: withoutMentions(text, this.names), at: Date.now() });
    return Promise.resolve();
  }

  noteThinking(text: string): void {
    this.entry({ kind: "thinking", text, at: Date.now() });
  }

  postNotice(text: string, level: NoticeLevel): Promise<void> {
    this.entry({ kind: "notice", text: withoutMentions(text, this.names), level, at: Date.now() });
    return Promise.resolve();
  }

  /**
   * A command's answer goes to the thread it was asked in, not here.
   *
   * The interface shows what the agent did. A listing somebody asked for is
   * theirs, in the moment, and reading it back later alongside the agent's
   * work only crowds it.
   */
  postReply(): Promise<void> {
    return Promise.resolve();
  }

  noteToolResult(result: ToolResult): void {
    this.entry({ kind: "toolResult", result, at: Date.now() });
  }

  noteAside(author: string, text: string): Promise<void> {
    this.entry({
      kind: "aside",
      author,
      text: withoutMentions(text, this.names),
      at: Date.now(),
    });
    return Promise.resolve();
  }

  notePrompt(author: string, text: string): Promise<void> {
    this.entry({
      kind: "prompt",
      author,
      text: withoutMentions(text, this.names),
      at: Date.now(),
    });
    return Promise.resolve();
  }

  appendActivity(line: string, tool?: ToolActivity): Promise<void> {
    this.entry({ kind: "activity", line, at: Date.now(), ...(tool === undefined ? {} : { tool }) });
    return Promise.resolve();
  }

  postDiff(
    path: string,
    added: number,
    removed: number,
    body: string,
    cause?: string,
  ): Promise<void> {
    this.entry({
      kind: "diff",
      path,
      added,
      removed,
      body,
      at: Date.now(),
      ...(cause === undefined ? {} : { cause }),
    });
    return Promise.resolve();
  }

  setWaiting(text: string | null): Promise<void> {
    this.state({ waiting: text });
    return Promise.resolve();
  }

  /**
   * Reactions are a chat affordance with no counterpart here: the interface
   * shows a message's fate through the session's own state.
   */
  setReaction(): Promise<void> {
    return Promise.resolve();
  }

  setUsage(usage: SessionUsage): void {
    this.state({ usage });
  }

  setBusy(busy: boolean): void {
    this.state({ busy });
  }

  upload(name: string, bytes: Uint8Array): Promise<void> {
    this.entry({ kind: "file", name, size: bytes.length, at: Date.now() });
    return Promise.resolve();
  }

  /**
   * The session has ended, which the interface shows as state.
   *
   * The stream is left open: a stopped session can be picked up again, and a
   * browser watching one should see that happen rather than be disconnected.
   */
  close(_reason: EndReason): Promise<void> {
    this.state({ ended: true, busy: false });
    return Promise.resolve();
  }
}
