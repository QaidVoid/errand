/**
 * One session, presented through any number of surfaces at once.
 *
 * A session reports what it is doing to a {@link ThreadPort}. This is one, and
 * it forwards to every attached view, so a chat thread and a browser are two
 * views of the same session rather than two sessions.
 *
 * It also keeps a bounded record of what was reported, so a view that attaches
 * to a session already in progress is shown what it missed instead of an empty
 * pane. The record holds what was said, not the state around it: state is kept
 * separately and applied as it currently stands, because replaying every queue
 * position a session ever had would be noise rather than history.
 */

import type { Logger } from "../log.ts";
import type {
  Delegated,
  EndReason,
  NoticeLevel,
  ReactionOutcome,
  SessionUsage,
  ThreadPort,
  ToolActivity,
  ToolResult,
} from "./port.ts";

/** One thing a session reported, kept so a late view can be shown it. */
export type Recorded =
  | { call: "post"; text: string }
  | { call: "prompt"; author: string; text: string }
  | { call: "aside"; author: string; text: string }
  | { call: "notice"; text: string; level: NoticeLevel }
  | { call: "thinking"; text: string }
  | { call: "reply"; text: string; command: string }
  | { call: "toolResult"; result: ToolResult }
  | { call: "activity"; line: string; tool?: ToolActivity }
  | { call: "delegation"; delegated: Delegated }
  | {
    call: "diff";
    path: string;
    added: number;
    removed: number;
    body: string;
    /** The tool call that made the change, absent in an older recording. */
    cause?: string | undefined;
  }
  /**
   * An attachment is recorded by name and size, never by contents. Holding the
   * bytes would mean a long session pinning every file it ever sent in memory.
   */
  | { call: "attachment"; name: string; size: number }
  /**
   * What the session has cost so far. State rather than history: only the
   * latest is meaningful, so restoring takes the last one and replays none of
   * them. It is recorded at all because otherwise a session that outlives the
   * daemon that ran it comes back with no model and no context size, which is
   * most of what a reader wants to know about a conversation they are joining.
   */
  | { call: "usage"; usage: SessionUsage };

/**
 * One recorded thing and the turn it belongs to.
 *
 * The turn is absent for anything recorded before turns were kept, which is why
 * a transcript written by an older daemon still reads.
 */
export interface Held {
  turn?: number | undefined;
  entry: Recorded;
}

/**
 * Where output is written down so it outlives the session.
 *
 * An interface rather than the transcript itself, so this module knows nothing
 * about files and a test needs none.
 */
export interface Recorder {
  append(entry: Recorded, turn: number): void;
}

/** How much of a session's output is kept for a view that attaches later. */
export const DEFAULT_TRANSCRIPT_LIMIT = 400;

/** The state a view needs in order to look right the moment it attaches. */
export interface ViewState {
  busy: boolean;
  waiting: string | null;
  ended: boolean;
  usage?: SessionUsage | undefined;
}

/** Delivers a session's output to every attached view. */
export class ViewFanOut implements ThreadPort {
  private readonly views = new Set<ThreadPort>();
  private readonly recorded: Held[] = [];
  private readonly reactions = new Map<string, ReactionOutcome>();

  private busy = false;
  private waiting: string | null = null;
  private ended = false;
  private dropped = 0;
  private usage: SessionUsage | undefined;
  /**
   * The turn being recorded. Zero is everything before the first prompt, which
   * is where a session's opening notices live.
   */
  private turn = 0;

  constructor(
    private readonly log: Logger,
    private readonly limit: number = DEFAULT_TRANSCRIPT_LIMIT,
    /** Where output is written down, so it survives the session. */
    private readonly recorder?: Recorder,
  ) {}

  /**
   * Seeds the record from a stored transcript, for a resumed session.
   *
   * Replaces what is held rather than adding to it, so restoring twice cannot
   * double a session's history.
   */
  restore(entries: readonly Held[], dropped = 0): void {
    this.recorded.length = 0;
    this.recorded.push(...entries.slice(-this.limit));
    this.dropped = dropped + Math.max(0, entries.length - this.limit);

    // State, not history: the last one stands and none of them is replayed as
    // an event, or a reader would watch the cost climb through every turn the
    // session ever ran.
    for (const held of entries) {
      if (held.entry.call === "usage") this.usage = held.entry.usage;
    }
    const withoutUsage = this.recorded.filter((held) => held.entry.call !== "usage");
    this.recorded.length = 0;
    this.recorded.push(...withoutUsage);

    // Carries on from where the stored session left off, so a resumed session
    // does not restart its numbering and collapse two turns into one.
    //
    // Counted over everything restored rather than over what is kept: usage is
    // dropped from the record above, and a turn whose only surviving entry was
    // its cost would otherwise be forgotten and its number handed out twice.
    const turns = entries.map((held) => held.turn ?? 0);
    this.turn = turns.length === 0 ? 0 : Math.max(...turns);
  }

  /**
   * The turn the recorded history has reached.
   *
   * Read when a session resumes, so its own counter carries on rather than
   * restarting at one and labelling a new exchange with a number the stored
   * history already used.
   */
  get currentTurn(): number {
    return this.turn;
  }

  /** How many views are currently attached. */
  get size(): number {
    return this.views.size;
  }

  /** The session's current state, for a view that has just attached. */
  get state(): ViewState {
    return { busy: this.busy, waiting: this.waiting, ended: this.ended, usage: this.usage };
  }

  /** Everything kept for replay, oldest first. */
  get history(): readonly Recorded[] {
    return this.recorded.map((held) => held.entry);
  }

  /** Everything kept for replay, with the turn each belongs to. */
  get held(): readonly Held[] {
    return this.recorded;
  }

  /** How much was dropped from the record, which a replay must admit to. */
  get droppedCount(): number {
    return this.dropped;
  }

  /**
   * Attaches a view and shows it what it missed.
   *
   * @returns a function that detaches it. Detaching never ends the session,
   *   however many views are left.
   */
  async attach(view: ThreadPort): Promise<() => void> {
    this.views.add(view);
    await this.replayTo(view);
    return () => {
      this.views.delete(view);
    };
  }

  /** Detaches a view without affecting the session or the other views. */
  detach(view: ThreadPort): void {
    this.views.delete(view);
  }

  private async replayTo(view: ThreadPort): Promise<void> {
    try {
      if (this.dropped > 0) {
        await view.post(`[${this.dropped} earlier line(s) not kept]`);
      }

      // The turn is re-announced at each boundary rather than passed with every
      // call, so a view stamps what it draws without every method growing an
      // argument that only one surface uses.
      let announced: number | undefined;

      for (const held of this.recorded) {
        const entry = held.entry;
        if (held.turn !== undefined && held.turn !== announced) {
          announced = held.turn;
          view.beginTurn(held.turn);
        }

        if (entry.call === "post") await view.post(entry.text);
        else if (entry.call === "prompt") await view.notePrompt(entry.author, entry.text);
        else if (entry.call === "aside") await view.noteAside(entry.author, entry.text);
        else if (entry.call === "notice") await view.postNotice(entry.text, entry.level);
        else if (entry.call === "thinking") view.noteThinking(entry.text);
        else if (entry.call === "reply") await view.postReply(entry.text, entry.command);
        else if (entry.call === "toolResult") view.noteToolResult(entry.result);
        else if (entry.call === "activity") await view.appendActivity(entry.line, entry.tool);
        else if (entry.call === "diff") {
          await view.postDiff(entry.path, entry.added, entry.removed, entry.body, entry.cause);
        } else if (entry.call === "attachment") {
          await view.post(`[attached ${entry.name}, ${entry.size} bytes]`);
        }
        // A usage entry is state, carried by `state` rather than replayed.
      }

      // Anything the view is told after this belongs to the turn in progress,
      // which is not the one the last replayed entry belonged to.
      if (announced !== undefined && announced !== this.turn) view.beginTurn(this.turn);

      for (const [messageId, outcome] of this.reactions) {
        await view.setReaction(messageId, outcome);
      }

      if (this.usage !== undefined) view.setUsage(this.usage);
      if (this.waiting !== null) await view.setWaiting(this.waiting);
      // Only when it is true: a view that has just attached already assumes a
      // session is not working, so saying so is noise.
      if (this.busy) view.setBusy(true);
      if (this.ended) await view.post("[this session has ended]");
    } catch (error) {
      this.log.warn("replaying to a new view failed", { detail: String(error) });
    }
  }

  private record(entry: Recorded): void {
    this.recorder?.append(entry, this.turn);
    this.recorded.push({ turn: this.turn, entry });
    while (this.recorded.length > this.limit) {
      this.recorded.shift();
      this.dropped += 1;
    }
  }

  /**
   * Delivers to every view, and keeps going when one of them fails.
   *
   * A browser that closed mid-turn must not stop the chat thread from being
   * told what happened, so a failing view is logged and skipped rather than
   * allowed to propagate into the session.
   */
  private async each(
    what: string,
    deliver: (view: ThreadPort) => Promise<void> | void,
  ): Promise<void> {
    // Wrapped rather than called directly: several of these methods are
    // synchronous, and a view that throws on one would otherwise escape the
    // map instead of being settled and skipped.
    const results = await Promise.allSettled(
      [...this.views].map((view) => Promise.resolve().then(() => deliver(view))),
    );
    for (const result of results) {
      if (result.status === "rejected") {
        this.log.warn("a view failed and was skipped", { what, detail: String(result.reason) });
      }
    }
  }

  beginTurn(turn: number): void {
    this.turn = turn;
    void this.each("turn", (view) => {
      view.beginTurn(turn);
    });
  }

  async post(text: string): Promise<void> {
    this.record({ call: "post", text });
    await this.each("post", (view) => view.post(text));
  }

  noteThinking(text: string): void {
    this.record({ call: "thinking", text });
    void this.each("thinking", (view) => {
      view.noteThinking(text);
    });
  }

  async postNotice(text: string, level: NoticeLevel): Promise<void> {
    this.record({ call: "notice", text, level });
    await this.each("notice", (view) => view.postNotice(text, level));
  }

  /**
   * Answers a command, in the thread the command was run in and nowhere else.
   *
   * Not recorded, and not shown by an interface reading a session back. A
   * command's answer belongs to the person who ran it, in the moment they ran
   * it, and is not part of the conversation with the agent: replaying a
   * directory listing from an hour ago alongside the agent's work says nothing
   * about what the agent did.
   */
  async postReply(text: string, command: string): Promise<void> {
    await this.each("reply", (view) => view.postReply(text, command));
  }

  noteToolResult(result: ToolResult): void {
    this.record({ call: "toolResult", result });
    void this.each("toolResult", (view) => {
      view.noteToolResult(result);
    });
  }

  async notePrompt(author: string, text: string): Promise<void> {
    this.record({ call: "prompt", author, text });
    await this.each("prompt", (view) => view.notePrompt(author, text));
  }

  async noteAside(author: string, text: string): Promise<void> {
    this.record({ call: "aside", author, text });
    await this.each("aside", (view) => view.noteAside(author, text));
  }

  async appendActivity(line: string, tool?: ToolActivity): Promise<void> {
    this.record(tool === undefined ? { call: "activity", line } : { call: "activity", line, tool });
    await this.each("activity", (view) => view.appendActivity(line, tool));
  }

  noteDelegation(delegated: Delegated): void {
    this.record({ call: "delegation", delegated });
    void this.each("delegation", (view) => view.noteDelegation(delegated));
  }

  async postDiff(
    path: string,
    added: number,
    removed: number,
    body: string,
    cause?: string,
  ): Promise<void> {
    this.record({ call: "diff", path, added, removed, body, cause });
    await this.each("diff", (view) => view.postDiff(path, added, removed, body, cause));
  }

  async setWaiting(text: string | null): Promise<void> {
    this.waiting = text;
    await this.each("waiting", (view) => view.setWaiting(text));
  }

  async setReaction(messageId: string, outcome: ReactionOutcome): Promise<void> {
    this.reactions.set(messageId, outcome);
    await this.each("reaction", (view) => view.setReaction(messageId, outcome));
  }

  setUsage(usage: SessionUsage): void {
    this.usage = usage;
    this.recorder?.append({ call: "usage", usage }, this.turn);
    void this.each("usage", (view) => {
      view.setUsage(usage);
    });
  }

  setBusy(busy: boolean): void {
    this.busy = busy;
    void this.each("busy", (view) => {
      view.setBusy(busy);
    });
  }

  async upload(name: string, bytes: Uint8Array, caption: string): Promise<void> {
    this.record({ call: "attachment", name, size: bytes.length });
    await this.each("upload", (view) => view.upload(name, bytes, caption));
  }

  async close(reason: EndReason): Promise<void> {
    this.ended = true;
    this.busy = false;
    await this.each("close", (view) => view.close(reason));
  }
}
