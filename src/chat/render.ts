/**
 * Splits and formats agent output into messages the chat service will take.
 *
 * Splitting is by code point, never by byte, so a multi-byte character is never
 * cut in half. A split landing inside a fenced code block closes the fence in
 * the earlier message and reopens it with the same language in the next, so
 * every message posted is independently well formed.
 */

import { prefixed, PREFIXES } from "./chars.ts";

/** The service's per-message character limit. */
export const MESSAGE_LIMIT = 2000;

/** The service's thread name limit. */
export const THREAD_NAME_LIMIT = 100;

/** The fence marker, and the cost of closing one at the end of a chunk. */
const FENCE = "```";
const CLOSING_COST = FENCE.length + 1;

/** Room left for a reopened fence when pre-splitting an over-long line. */
const FENCE_HEADROOM = 16;

/** Splits a string into pieces of at most `size` code points. */
function splitByCodePoints(text: string, size: number): string[] {
  const points = Array.from(text);
  if (points.length <= size) return [text];
  const pieces: string[] = [];
  for (let i = 0; i < points.length; i += size) {
    pieces.push(points.slice(i, i + size).join(""));
  }
  return pieces;
}

/** Length in code points, which is what the limit actually counts. */
function length(text: string): number {
  return Array.from(text).length;
}

/**
 * Returns the fence language when the line opens or closes a fenced block,
 * or null when it is ordinary text.
 */
function fenceLanguage(line: string): string | null {
  const trimmed = line.trimStart();
  if (!trimmed.startsWith(FENCE)) return null;
  return trimmed.slice(FENCE.length).trim();
}

/**
 * Splits text into messages that each fit the limit, preferring line
 * boundaries and repairing any fence the split lands inside.
 */
export function splitMessage(text: string, limit: number = MESSAGE_LIMIT): string[] {
  if (length(text) <= limit) return text.length === 0 ? [] : [text];

  const lines: string[] = [];
  for (const line of text.split("\n")) {
    lines.push(...splitByCodePoints(line, limit - FENCE_HEADROOM));
  }

  const chunks: string[] = [];
  let current: string[] = [];
  let currentLength = 0;
  let openLanguage: string | null = null;

  const reserve = (): number => (openLanguage === null ? 0 : CLOSING_COST);

  const flush = (): void => {
    if (current.length === 0) return;
    let body = current.join("\n");
    if (openLanguage !== null) body += `\n${FENCE}`;
    chunks.push(body);
    current = [];
    currentLength = 0;
    if (openLanguage !== null) {
      const reopened = `${FENCE}${openLanguage}`;
      current.push(reopened);
      currentLength = length(reopened);
    }
  };

  for (const line of lines) {
    const cost = current.length === 0 ? length(line) : length(line) + 1;
    if (currentLength + cost + reserve() > limit) flush();

    current.push(line);
    currentLength += current.length === 1 ? length(line) : length(line) + 1;

    const language = fenceLanguage(line);
    if (language !== null) {
      openLanguage = openLanguage === null ? language : null;
    }
  }

  if (current.length > 0) {
    chunks.push(current.join("\n"));
  }

  return chunks.filter((chunk) => chunk.length > 0);
}

/**
 * Derives a thread name from the first prompt and the project it runs in,
 * truncated to the limit without splitting a character.
 */
export function threadName(project: string, prompt: string): string {
  const firstLine = prompt.split("\n").find((line) => line.trim().length > 0) ?? "session";
  const collapsed = firstLine.trim().replace(/\s+/g, " ");
  const prefix = `${project}: `;
  const room = THREAD_NAME_LIMIT - length(prefix);
  const body = Array.from(collapsed).slice(0, room).join("").trimEnd();
  return `${prefix}${body.length > 0 ? body : "session"}`;
}

/** Truncates tool output and says so, rather than posting a silent prefix. */
export function truncate(text: string, max: number): string {
  const points = Array.from(text);
  if (points.length <= max) return text;
  return `${points.slice(0, max).join("")}\n[truncated, ${points.length - max} more characters]`;
}

/**
 * The line announcing that a tool call started.
 *
 * The command is shown whole. Its tail is often the part that says what it was
 * for, so shortening throws away the half worth reading. Only Discord's own
 * message limit ever cuts anything, and that is handled where blocks are sent.
 *
 * Whitespace is flattened so a multi-line command stays one entry in a block,
 * and the target is wrapped in inline code, which stops Discord turning any URL
 * in it into a link.
 */
export function toolLine(toolName: string, target: string | undefined): string {
  if (target === undefined || target.trim().length === 0) {
    return prefixed("tool", `\`${toolName}\``);
  }

  const flattened = target.replace(/\s+/g, " ").trim();

  // Backticks inside the target would end the inline code span early.
  return prefixed("tool", `\`${toolName}\` \`${flattened.replace(/`/g, "'")}\``);
}

/**
 * What a session has cost, in one short line.
 *
 * Cached input is reported as a share of everything sent, because that is the
 * number worth watching: it is what keeps a long session affordable.
 */
export function usageSummary(usage: {
  cacheRead: number;
  input: number;
  totalTokens: number;
  cost: number;
  contextTokens?: number | undefined;
  contextWindow?: number | undefined;
}): string {
  const sent = usage.input + usage.cacheRead;
  const cached = sent === 0 ? 0 : Math.round((usage.cacheRead / sent) * 100);

  const parts = [`${tokens(usage.totalTokens)} tokens`, `${cached}% cached`];

  // What the conversation is carrying now, as opposed to what it has spent in
  // total. The share is the useful part: it says how much room is left.
  if (usage.contextTokens !== undefined && usage.contextTokens > 0) {
    parts.push(
      usage.contextWindow === undefined || usage.contextWindow <= 0
        ? `${tokens(usage.contextTokens)} context`
        : `${tokens(usage.contextTokens)}/${tokens(usage.contextWindow)} context (${
          Math.round(
            (usage.contextTokens / usage.contextWindow) * 100,
          )
        }%)`,
    );
  }
  if (usage.cost > 0) parts.push(`$${usage.cost.toFixed(4)}`);
  return parts.join(", ");
}

/** The line announcing that the agent is thinking. */
export function thinkingLine(): string {
  return prefixed("thinking", "thinking");
}

/** A degraded-state line, such as a shared workspace or a provider backoff. */
/** Suffixes, smallest first, each a thousand times the one before. */
const MAGNITUDES: [number, string][] = [
  [1_000_000_000, "B"],
  [1_000_000, "M"],
  [1_000, "k"],
];

/**
 * A count as a person would read it.
 *
 * Carries up rather than growing a long number: a million tokens reads as
 * `1.0M`, not `1000.0k`. Below a thousand it is left exactly as it is, since
 * rounding a small count loses the only detail it had.
 */
export function tokens(count: number): string {
  const size = Math.abs(count);
  for (let index = 0; index < MAGNITUDES.length; index += 1) {
    const [magnitude, suffix] = MAGNITUDES[index] as [number, string];
    if (size < magnitude) continue;

    const scaled = count / magnitude;
    // One decimal until three digits, then none: 12.3M, but 123M.
    const digits = Math.abs(scaled) >= 100 ? 0 : 1;

    // Rounding can push a value into the next magnitude: 999,999 would read as
    // 1000k, which is a magnitude out. Carry it up instead.
    if (Math.abs(Number(scaled.toFixed(digits))) >= 1000 && index > 0) {
      const [bigger, biggerSuffix] = MAGNITUDES[index - 1] as [number, string];
      return `${(count / bigger).toFixed(1)}${biggerSuffix}`;
    }
    return `${scaled.toFixed(digits)}${suffix}`;
  }
  return String(count);
}

/**
 * A moment, rendered so Discord counts down to it in the reader's own zone.
 *
 * `<t:seconds:R>` is resolved by the client, so one message reads correctly for
 * everyone and keeps reading correctly as the wait shortens. Writing the time
 * out here would be wrong for anyone in another zone and stale a minute later.
 */
export function whenRelative(epochMs: number): string {
  return `<t:${Math.floor(epochMs / 1000)}:R>`;
}

export function warningLine(text: string): string {
  return prefixed("warning", text);
}

/** A connection or session lifecycle line. */
export function connectionLine(text: string): string {
  return prefixed("connection", text);
}

/** A question the agent is asking the user. */
export function questionLine(text: string): string {
  return prefixed("question", text);
}

/**
 * A bracketed ASCII marker, for states with no enumerated glyph. A queue
 * position is a number, not a state, so no emoji spells it.
 */
export function marker(state: string): string {
  return `[${state}]`;
}

/** Every enumerated prefix glyph, for asserting output stays inside the table. */
export function prefixGlyphs(): string[] {
  return Object.keys(PREFIXES).map((key) => prefixed(key as keyof typeof PREFIXES, "").trimEnd());
}
