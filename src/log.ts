/**
 * Structured ASCII-only logging.
 *
 * Chat output may carry emoji; a log line describing it may not. Escaping
 * rather than dropping keeps the line greppable and lossless without making a
 * log file's readability depend on terminal font coverage.
 */

/** Severity of a log line. */
export type LogLevel = "info" | "warn" | "error";

/** Extra key and value pairs appended to a line. */
export type LogFields = Record<string, string | number | boolean>;

/** Where a line goes. Injected so a test reads lines instead of a stream. */
export type Sink = (level: LogLevel, line: string) => void;

/** A logger, optionally bound to fields it repeats on every line. */
export interface Logger {
  info(message: string, fields?: LogFields): void;
  warn(message: string, fields?: LogFields): void;
  error(message: string, fields?: LogFields): void;
  /** Returns a logger that adds the given fields to every line. */
  with(fields: LogFields): Logger;
}

const MAX_ASCII = 0x7f;

/**
 * Replaces every non-ASCII character with its `\u{XXXX}` escape, so that a
 * line describing an emoji-bearing message is still pure ASCII.
 */
export function toAscii(text: string): string {
  let out = "";
  for (const character of text) {
    const codepoint = character.codePointAt(0) ?? 0;
    out += codepoint > MAX_ASCII
      ? `\\u{${codepoint.toString(16).toUpperCase().padStart(4, "0")}}`
      : character;
  }
  return out;
}

function formatFields(fields: LogFields): string {
  const parts = Object.entries(fields).map(([key, value]) => `${key}=${toAscii(String(value))}`);
  return parts.length === 0 ? "" : ` ${parts.join(" ")}`;
}

/**
 * Formats one line without writing it, so it can be asserted on.
 *
 * The time leads, because the first question asked of a daemon's log is when
 * something happened, and a line without one is only useful while the process
 * that wrote it is still running.
 */
export function formatLine(
  level: LogLevel,
  message: string,
  fields: LogFields = {},
  at: Date = new Date(),
): string {
  return `${at.toISOString()} [${level}] ${toAscii(message)}${formatFields(fields)}`;
}

const encoder = new TextEncoder();

/**
 * Writes to the process streams directly.
 *
 * Synchronously, so that a line written just before a crash is on disk rather
 * than in a buffer that never flushed.
 */
export const streamSink: Sink = (level: LogLevel, line: string): void => {
  const stream = level === "error" ? Deno.stderr : Deno.stdout;
  stream.writeSync(encoder.encode(`${line}\n`));
};

/** Creates a logger, optionally carrying fields on every line it writes. */
export function createLogger(base: LogFields = {}, sink: Sink = streamSink): Logger {
  const emit = (level: LogLevel, message: string, fields: LogFields = {}): void => {
    sink(level, formatLine(level, message, { ...base, ...fields }));
  };

  return {
    info: (message, fields) => emit("info", message, fields),
    warn: (message, fields) => emit("warn", message, fields),
    error: (message, fields) => emit("error", message, fields),
    with: (fields) => createLogger({ ...base, ...fields }, sink),
  };
}
