/**
 * Types for the agent's line-delimited JSON protocol.
 *
 * Only the subset this system sends and reacts to is modelled. Everything else
 * the agent emits is carried as an unrecognized record and ignored, so a newer
 * agent version adding an event cannot break a session.
 */

/** A record read from the agent, before it is classified. */
export type AgentRecord = Record<string, unknown> & { type?: unknown };

/** How a prompt behaves when a turn is already running. */
export type StreamingBehavior = "steer" | "followUp";

/**
 * What a turn cost, as the agent reports it.
 *
 * Cache reads are counted separately from fresh input, which is what makes a
 * cache hit rate meaningful rather than a guess.
 */
export interface Usage {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  totalTokens: number;
  cost: number;
  model?: string;
}

/** Reads usage off an event, when it carries any. */
export function usageOf(record: AgentRecord): Usage | undefined {
  const message = (record.message ?? record) as Record<string, unknown>;
  const raw = message.usage;
  if (typeof raw !== "object" || raw === null) return undefined;

  const held = raw as Record<string, unknown>;
  const number = (value: unknown): number => (typeof value === "number" ? value : 0);
  const total = held.cost as Record<string, unknown> | undefined;
  const model = message.model;

  return {
    input: number(held.input),
    output: number(held.output),
    cacheRead: number(held.cacheRead),
    cacheWrite: number(held.cacheWrite),
    totalTokens: number(held.totalTokens),
    cost: number(total?.total),
    ...(typeof model === "string" ? { model } : {}),
  };
}

/** A command written to the agent. */
export type AgentCommand =
  | {
    type: "prompt";
    message: string;
    images?: AgentImage[];
    streamingBehavior?: StreamingBehavior;
  }
  | { type: "steer"; message: string; images?: AgentImage[] }
  | { type: "follow_up"; message: string; images?: AgentImage[] }
  | { type: "set_model"; provider: string; modelId: string }
  | { type: "abort" };

/** An image delivered alongside a prompt. */
export interface AgentImage {
  type: "image";
  data: string;
  mimeType: string;
}

/**
 * A dialog the agent is blocked on. `editor` is included because it must be
 * answered, even though the answer is always a cancellation.
 */
export type DialogMethod = "select" | "confirm" | "input" | "editor";

/** A dialog request the agent is waiting on. */
export interface DialogRequest {
  id: string;
  method: DialogMethod;
  title: string;
  message?: string;
  options?: string[];
  placeholder?: string;
  prefill?: string;
}

/** The reply to a dialog request. */
export type DialogResponse =
  | { type: "extension_ui_response"; id: string; value: string }
  | { type: "extension_ui_response"; id: string; confirmed: boolean }
  | { type: "extension_ui_response"; id: string; cancelled: true };

/** Fire-and-forget UI methods, which are acknowledged by doing nothing. */
const FIRE_AND_FORGET = new Set([
  "notify",
  "setStatus",
  "setWidget",
  "setTitle",
  "set_editor_text",
]);

const DIALOG_METHODS = new Set<string>(["select", "confirm", "input", "editor"]);

/** True when a UI request expects a response rather than being informational. */
export function isDialogMethod(method: unknown): method is DialogMethod {
  return typeof method === "string" && DIALOG_METHODS.has(method);
}

/** True when a UI request is informational and must not be answered. */
export function isFireAndForget(method: unknown): boolean {
  return typeof method === "string" && FIRE_AND_FORGET.has(method);
}

function str(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

/** Reads a dialog request out of a raw record, or null when it is not one. */
export function asDialogRequest(record: AgentRecord): DialogRequest | null {
  if (record.type !== "extension_ui_request") return null;
  const id = str(record.id);
  if (id === undefined || !isDialogMethod(record.method)) return null;

  const options = Array.isArray(record.options)
    ? record.options.filter((o): o is string => typeof o === "string")
    : undefined;

  const request: DialogRequest = {
    id,
    method: record.method,
    title: str(record.title) ?? "",
  };
  const message = str(record.message);
  if (message !== undefined) request.message = message;
  if (options !== undefined) request.options = options;
  const placeholder = str(record.placeholder);
  if (placeholder !== undefined) request.placeholder = placeholder;
  const prefill = str(record.prefill);
  if (prefill !== undefined) request.prefill = prefill;
  return request;
}

/** A tool call the agent has started. */
export interface ToolStart {
  toolName: string;
  target: string | undefined;
}

/** Pulls a readable target out of a tool call's arguments, when there is one. */
export function toolTarget(args: unknown): string | undefined {
  if (typeof args !== "object" || args === null) return undefined;
  const record = args as Record<string, unknown>;
  for (const key of ["command", "path", "file_path", "filePath", "pattern", "query", "url"]) {
    const value = record[key];
    if (typeof value === "string" && value.trim().length > 0) return value.trim();
  }
  return undefined;
}

/** Concatenates the text parts of an agent message's content. */
export function messageText(message: unknown): string {
  if (typeof message !== "object" || message === null) return "";
  const content = (message as Record<string, unknown>).content;
  if (!Array.isArray(content)) return "";
  return content
    .filter(
      (part): part is { type: string; text: string } =>
        typeof part === "object" &&
        part !== null &&
        (part as Record<string, unknown>).type === "text" &&
        typeof (part as Record<string, unknown>).text === "string",
    )
    .map((part) => part.text)
    .join("");
}

/**
 * True when a streaming update reports the agent starting to think.
 *
 * The update carries an `assistantMessageEvent`, not a message: the deltas
 * arrive before any message exists to inspect.
 */
export function startsThinking(record: AgentRecord): boolean {
  const event = record.assistantMessageEvent;
  if (typeof event !== "object" || event === null) return false;
  return (event as Record<string, unknown>).type === "thinking_start";
}

/**
 * The agent's reasoning for a block that has just finished, if this is one.
 *
 * The end event carries the whole block, so there is no need to accumulate the
 * deltas that led to it.
 */
export function thinkingEnded(record: AgentRecord): string | undefined {
  const event = record.assistantMessageEvent;
  if (typeof event !== "object" || event === null) return undefined;

  const held = event as Record<string, unknown>;
  if (held.type !== "thinking_end") return undefined;
  return typeof held.content === "string" ? held.content : undefined;
}

/** The role of a message, when it has one. */
export function messageRole(message: unknown): string | undefined {
  if (typeof message !== "object" || message === null) return undefined;
  const role = (message as Record<string, unknown>).role;
  return typeof role === "string" ? role : undefined;
}
