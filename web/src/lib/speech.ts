/**
 * The agent's answer and its reasoning arrive a piece at a time, each piece
 * keeping the space or the break it was cut at. A run of pieces of the same
 * kind is one thing said, so it is shown as one entry rather than many.
 */
import type { Entry } from "./types.ts";

/** Whether `entry` continues a run of words the entry before it began. */
function continues(previous: Entry | undefined, entry: Entry): previous is Entry {
  return (
    previous !== undefined &&
    previous.kind === entry.kind &&
    (entry.kind === "message" || entry.kind === "thinking") &&
    previous.turn === entry.turn
  );
}

/**
 * `entries` with `arrived` added, each piece that continues the entry before
 * it joined onto that entry as it came, and the rest appended.
 */
export function joinSpeech(entries: readonly Entry[], arrived: readonly Entry[]): Entry[] {
  const joined = entries.slice();
  for (const entry of arrived) {
    const previous = joined[joined.length - 1];
    if (continues(previous, entry) && "text" in previous && "text" in entry) {
      joined[joined.length - 1] = { ...previous, text: previous.text + entry.text } as Entry;
    } else {
      joined.push(entry);
    }
  }
  return joined;
}
