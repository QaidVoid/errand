/**
 * The enumerated chat character table.
 *
 * This is the complete set of non-ASCII characters the system emits. Each entry
 * names exactly one state. None is decoration, and using one for a state it
 * does not name is a bug.
 *
 * Entries are declared as codepoints rather than as literal glyphs, so this
 * file is itself ASCII and needs no exception from the rule. An editor or a
 * terminal that cannot render an emoji therefore cannot silently corrupt one.
 */

/** One enumerated character: how it is spelled, and the single state it means. */
export interface ChatChar {
  /** Codepoints in `U+XXXX` form, in order. */
  readonly codepoints: readonly string[];
  /** The Unicode name. */
  readonly name: string;
  /** The one state this character denotes. */
  readonly meaning: string;
}

/**
 * Reactions placed on the sender's own message, tracking that message's fate.
 * Exactly one is present at a time; an outcome replaces the acknowledgement
 * rather than joining it.
 */
export const REACTIONS = {
  accepted: {
    codepoints: ["U+23F3"],
    name: "hourglass not done",
    meaning: "accepted, queued or running",
  },
  succeeded: {
    codepoints: ["U+2705"],
    name: "white heavy check mark",
    meaning: "the turn it started completed",
  },
  failed: {
    codepoints: ["U+274C"],
    name: "cross mark",
    meaning: "the turn failed, or the message was rejected",
  },
  interrupted: {
    codepoints: ["U+23F9", "U+FE0F"],
    name: "stop button",
    meaning: "the turn was interrupted",
  },
} as const satisfies Record<string, ChatChar>;

/** Leading glyph of a status line the bot posts in a thread. */
export const PREFIXES = {
  tool: {
    codepoints: ["U+1F527"],
    name: "wrench",
    meaning: "a tool call started",
  },
  thinking: {
    codepoints: ["U+1F4AD"],
    name: "thought balloon",
    meaning: "the agent is thinking",
  },
  question: {
    codepoints: ["U+2753"],
    name: "question mark",
    meaning: "the agent is asking the user something",
  },
  warning: {
    codepoints: ["U+26A0", "U+FE0F"],
    name: "warning sign",
    meaning: "degraded state: shared workspace, backend gap, provider backoff",
  },
  connection: {
    codepoints: ["U+1F50C"],
    name: "electric plug",
    meaning: "connection or session lifecycle changed",
  },
} as const satisfies Record<string, ChatChar>;

/** Every enumerated character, reactions and prefixes together. */
export const ALL_CHARS: readonly ChatChar[] = [
  ...Object.values(REACTIONS),
  ...Object.values(PREFIXES),
];

/** Name of a reaction, used to pick one by outcome. */
export type ReactionKey = keyof typeof REACTIONS;

/** Name of a status prefix. */
export type PrefixKey = keyof typeof PREFIXES;

function parseCodepoint(codepoint: string): number {
  const match = /^U\+([0-9A-F]{4,6})$/.exec(codepoint);
  if (match === null) {
    throw new Error(`malformed codepoint ${codepoint}; expected U+XXXX`);
  }
  return Number.parseInt(match[1] as string, 16);
}

/** Renders an entry to the string Discord actually receives. */
export function glyph(entry: ChatChar): string {
  return String.fromCodePoint(...entry.codepoints.map(parseCodepoint));
}

/** Renders a reaction by name. */
export function reaction(key: ReactionKey): string {
  return glyph(REACTIONS[key]);
}

/** Prefixes a status line with its enumerated glyph. */
export function prefixed(key: PrefixKey, text: string): string {
  return `${glyph(PREFIXES[key])} ${text}`;
}
