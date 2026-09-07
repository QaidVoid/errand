/**
 * A session, arranged into the turns a reader thinks in.
 *
 * The daemon stamps each entry with the turn it belongs to, so this groups
 * rather than guesses. A session recorded before that was kept has no stamps,
 * and is presented as one ungrouped run rather than being invented into turns.
 */

import type { Entry, ToolResult } from "./types.ts";

/** A tool call with whatever came back from it. */
export interface Call {
  entry: Extract<Entry, { kind: "activity" }>;
  result?: ToolResult | undefined;
  /** Changes the call made, shown on it rather than adrift after it. */
  changes: Extract<Entry, { kind: "diff" }>[];
}

/** One entry as it will be drawn, with a call's result folded into it. */
export type Item = { kind: "call"; call: Call } | { kind: "entry"; entry: Entry };

/** A prompt and everything the agent did in answering it. */
export interface Turn {
  /** The daemon's number for it, or undefined when the session predates them. */
  number?: number | undefined;
  /** What was asked, when a prompt opened the turn. */
  asked?: Extract<Entry, { kind: "prompt" }> | undefined;
  items: Item[];
  at: number;
  /** True once the turn reported that it finished. */
  done: boolean;
  /**
   * How many calls in it failed.
   *
   * A count rather than a flag: one command exiting non-zero among a hundred is
   * something to notice, not a turn that went wrong.
   */
  failures: number;
  /** What the turn reported it cost, taken from its closing notice. */
  cost?: string | undefined;
}

/** What a session looks like once arranged. */
export interface Reading {
  turns: Turn[];
  /** False when the session was recorded before turns were kept. */
  grouped: boolean;
}

/** The closing notice reports the cost after the marker, when there is one. */
function costOf(text: string): string | undefined {
  const after = text.split("[done]")[1]?.trim();
  return after !== undefined && after.length > 0 ? after : undefined;
}

/**
 * Arranges a session's entries into turns.
 *
 * Results and changes are folded onto the call they belong to. A change whose
 * call is unknown, as in an older recording, stays where it fell so that it is
 * never dropped for want of something to attach it to.
 */
export function read(entries: readonly Entry[]): Reading {
  const grouped = entries.some((entry) => entry.turn !== undefined);

  const results = new Map<string, ToolResult>();
  for (const entry of entries) {
    if (entry.kind === "toolResult") results.set(entry.result.id, entry.result);
  }

  const turns: Turn[] = [];
  const callsById = new Map<string, Call>();
  let current: Turn | undefined;

  const open = (entry: Entry): Turn => {
    const turn: Turn = {
      number: entry.turn,
      items: [],
      at: entry.at,
      done: false,
      failures: 0,
    };
    turns.push(turn);
    return turn;
  };

  for (const entry of entries) {
    // A result is shown on its call, so it is never an item of its own.
    if (entry.kind === "toolResult") continue;

    if (current === undefined || (grouped && entry.turn !== current.number)) {
      current = open(entry);
    }

    if (entry.kind === "prompt" && current.asked === undefined) {
      current.asked = entry;
      current.at = entry.at;
      continue;
    }

    if (entry.kind === "activity") {
      const result = entry.tool?.id === undefined ? undefined : results.get(entry.tool.id);
      const call: Call = { entry, result, changes: [] };
      if (entry.tool?.id !== undefined) callsById.set(entry.tool.id, call);
      if (result?.failed === true || entry.tool?.failed === true) current.failures += 1;
      current.items.push({ kind: "call", call });
      continue;
    }

    if (entry.kind === "diff" && entry.cause !== undefined) {
      const call = callsById.get(entry.cause);
      if (call !== undefined) {
        call.changes.push(entry);
        continue;
      }
    }

    if (entry.kind === "notice" && entry.level === "done") {
      current.done = true;
      current.cost = costOf(entry.text);
      continue;
    }

    current.items.push({ kind: "entry", entry });
  }

  return { turns, grouped };
}

/** A short label for a turn, for when it is folded to one line. */
export function label(turn: Turn): string {
  const asked = turn.asked?.text.trim();
  if (asked !== undefined && asked.length > 0) return asked.split("\n")[0] ?? asked;

  const first = turn.items.find((item) => item.kind === "call");
  if (first?.kind === "call") {
    const tool = first.call.entry.tool;
    return tool === undefined ? "worked" : `${tool.name} ${tool.target ?? ""}`.trim();
  }
  return turn.number === 0 ? "session opened" : "worked";
}

/** How many tool calls a turn made, for saying what folding it away hides. */
export function callCount(turn: Turn): number {
  return turn.items.filter((item) => item.kind === "call").length;
}
