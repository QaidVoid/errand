/**
 * Narrowing a session to one kind of thing.
 *
 * A lens changes what is shown inside each exchange rather than which
 * exchanges are shown. Filtering whole exchanges was worse than doing nothing:
 * an exchange that said something and also ran twenty commands still showed
 * all twenty under "said", so the control looked broken.
 *
 * Each lens also reports how much it would show. A control that says "ran 34"
 * has visibly done something before it is pressed, and one that says nothing
 * to show is disabled rather than leaving somebody wondering.
 */

import type { Item, Turn } from "./turns.ts";

/** What a session can be narrowed to. */
export type Lens = "all" | "said" | "ran" | "changed";

/** The lenses, in the order they are offered. */
export const LENSES: Lens[] = ["all", "said", "ran", "changed"];

/** What each one is for, so the control can say. */
export const LENS_HELP: Record<Lens, string> = {
  all: "everything, in order",
  said: "what was asked and what the agent answered",
  ran: "the commands and tools it ran",
  changed: "the files it changed",
};

/** Whether one item survives a lens. */
export function keeps(lens: Lens, item: Item): boolean {
  if (lens === "all") return true;

  if (lens === "ran") {
    return item.kind === "call" || (item.kind === "entry" && item.entry.kind === "delegation");
  }

  if (lens === "changed") {
    if (item.kind === "call") return item.call.changes.length > 0;
    return item.entry.kind === "diff";
  }

  // What was said: the conversation, which is the agent's own words and the
  // asides around them. A prompt opens an exchange rather than being an item
  // in it, so an exchange that was only asked something still has one.
  return item.kind === "entry" &&
    (item.entry.kind === "message" || item.entry.kind === "aside" ||
      item.entry.kind === "thinking");
}

/** An exchange with only what the lens keeps in it. */
export function through(lens: Lens, turn: Turn): Turn {
  if (lens === "all") return turn;
  return { ...turn, items: turn.items.filter((item) => keeps(lens, item)) };
}

/**
 * Whether an exchange has anything left under a lens.
 *
 * An exchange that was only asked something is kept under "said", because the
 * question is the thing being read.
 */
export function survives(lens: Lens, turn: Turn): boolean {
  if (lens === "all") return true;
  if (lens === "said" && turn.asked !== undefined) return true;
  return turn.items.some((item) => keeps(lens, item));
}

/** How much each lens would show, for saying so on the control. */
export function counts(turns: readonly Turn[]): Record<Lens, number> {
  const found: Record<Lens, number> = { all: turns.length, said: 0, ran: 0, changed: 0 };
  for (const lens of ["said", "ran", "changed"] as const) {
    for (const turn of turns) {
      found[lens] += turn.items.filter((item) => keeps(lens, item)).length;
      if (lens === "said" && turn.asked !== undefined) found[lens] += 1;
    }
  }
  return found;
}
