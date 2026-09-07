/**
 * A session's output, written down so it outlives the sandbox that produced it.
 *
 * A session reports to its views, which are in memory: close the sandbox and
 * the record of what happened goes with it. That is fine for a chat thread,
 * which keeps its own copy, and wrong for anything reading a session back,
 * which would show an empty pane for every session that has stopped.
 *
 * One file per session, beside its state, so removing a session's state removes
 * its transcript with it. Appending is best effort: a session must never fail
 * because its history could not be written.
 */

import type { Logger } from "../log.ts";
import type { Recorded } from "./views.ts";

/** One recorded thing, with when it happened and the turn it belongs to. */
export interface Journaled {
  at: number;
  /** Absent in a transcript written before turns were recorded. */
  turn?: number | undefined;
  entry: Recorded;
}

/** The filename inside a session's state directory. */
export const TRANSCRIPT_FILENAME = "transcript.jsonl";

/**
 * How much is read back.
 *
 * A session that ran for hours can have written far more than is useful to
 * reopen, and the most recent is the part worth showing.
 */
export const MAX_REPLAYED = 2_000;

/**
 * How much of a transcript is read when looking for what was first asked.
 *
 * The opening prompt arrives within the first few lines, after a notice or
 * two. Reading the whole file for it costs the length of the session, and it
 * is read once per stopped session every time they are listed.
 */
export const OPENING_SCAN_BYTES = 64 * 1024;

/** What was read back, and what was left behind. */
export interface StoredTranscript {
  entries: Journaled[];
  /** Entries older than those returned, so a replay can admit to the gap. */
  dropped: number;
}

function isJournaled(value: unknown): value is Journaled {
  if (typeof value !== "object" || value === null) return false;
  const held = value as Record<string, unknown>;
  if (typeof held.at !== "number") return false;
  if (held.turn !== undefined && typeof held.turn !== "number") return false;
  const entry = held.entry;
  if (typeof entry !== "object" || entry === null) return false;
  return typeof (entry as Record<string, unknown>).call === "string";
}

/** Reads whole entries out of text, skipping anything torn or unrecognised. */
function journaled(text: string): Journaled[] {
  const entries: Journaled[] = [];
  for (const line of text.split("\n")) {
    if (line.trim().length === 0) continue;
    try {
      const value: unknown = JSON.parse(line);
      if (isJournaled(value)) entries.push(value);
    } catch {
      // A line torn by a crash mid-write is skipped rather than losing the
      // whole transcript around it.
    }
  }
  return entries;
}

/** An append-only transcript for one session. */
export class Transcript {
  constructor(private readonly path: string, private readonly log?: Logger) {}

  /** Appends one entry. Never throws: history is not worth failing a turn for. */
  append(entry: Recorded, turn?: number, at: number = Date.now()): void {
    try {
      Deno.writeTextFileSync(this.path, `${JSON.stringify({ at, turn, entry })}\n`, {
        append: true,
        mode: 0o600,
      });
    } catch (error) {
      this.log?.warn("a transcript entry could not be written", { detail: String(error) });
    }
  }

  /**
   * The first thing this session was asked to do.
   *
   * Read from here rather than from the thread index, which deliberately holds
   * no message content and exists only to find a session again.
   *
   * Only the head of the file is scanned. A session whose first prompt is
   * somehow beyond that is left unnamed, which costs a title, where reading
   * every transcript in full costs the length of every session.
   *
   * @returns undefined when the session was never asked anything, as when it
   *   failed before its first prompt.
   */
  opening(): string | undefined {
    let head: string;
    try {
      head = this.readHead(OPENING_SCAN_BYTES);
    } catch {
      return undefined;
    }

    for (const value of journaled(head)) {
      if (value.entry.call === "prompt") return value.entry.text.trim();
    }
    return undefined;
  }

  /** Reads the most recent entries. A missing or unreadable file reads empty. */
  read(limit: number = MAX_REPLAYED): StoredTranscript {
    let text: string;
    try {
      text = Deno.readTextFileSync(this.path);
    } catch {
      return { entries: [], dropped: 0 };
    }

    const parsed = journaled(text);
    return parsed.length <= limit
      ? { entries: parsed, dropped: 0 }
      : { entries: parsed.slice(-limit), dropped: parsed.length - limit };
  }

  /**
   * The first bytes of the file, cut back to the last whole line.
   *
   * Cutting back matters: a read that lands mid-line would otherwise hand a
   * truncated JSON object to the parser, which would drop the entry it was
   * halfway through even though the file holds it in full.
   */
  private readHead(bytes: number): string {
    const file = Deno.openSync(this.path, { read: true });
    try {
      const buffer = new Uint8Array(bytes);
      const read = file.readSync(buffer) ?? 0;
      const text = new TextDecoder().decode(buffer.subarray(0, read));
      const lastBreak = text.lastIndexOf("\n");
      return lastBreak === -1 ? text : text.slice(0, lastBreak);
    } finally {
      file.close();
    }
  }
}
