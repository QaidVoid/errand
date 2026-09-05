/**
 * A question the session's model asks a cheaper one about something that
 * already exists.
 *
 * A delegation names one artefact and asks one question about it. That shape
 * is the whole guarantee: the cheaper model is shown a single thing and asked
 * about it, so its answer can be checked against the same thing, and it is
 * never in a position to decide anything because it is never told what the
 * session is trying to do.
 */

import { within } from "../sandbox/paths.ts";

/** What a delegation may be asked about. */
export type Source =
  /** A file in the session's project. */
  | { kind: "file"; path: string }
  /** The output a tool call produced, by that call's id. */
  | { kind: "output"; callId: string }
  /** A file attached to the conversation. */
  | { kind: "attachment"; name: string };

/** A well-formed delegation, before its source has been read. */
export interface Delegation {
  question: string;
  source: Source;
}

/** Why a delegation was not accepted, in words worth showing. */
export interface Refused {
  refused: string;
}

/** What a delegation was given, once its source has been read. */
export interface Resolved {
  question: string;
  /** What the source is, for attributing the answer. */
  describes: string;
  content: string;
}

/** True when a result is a refusal rather than a value. */
export function isRefused(value: unknown): value is Refused {
  return typeof value === "object" && value !== null && "refused" in value;
}

function text(value: unknown): string | undefined {
  return typeof value === "string" && value.trim().length > 0 ? value.trim() : undefined;
}

/**
 * Reads a delegation the agent wrote.
 *
 * A request that names nothing is refused rather than sent, because a
 * delegation with no artefact is a conversation with a second model, which is
 * the thing this is not.
 */
export function parseDelegation(raw: unknown): Delegation | Refused {
  if (typeof raw !== "object" || raw === null) {
    return { refused: "a delegation must be an object with a question and a source" };
  }
  const record = raw as Record<string, unknown>;

  const question = text(record.question);
  if (question === undefined) return { refused: "a delegation must ask a question" };

  const path = text(record.path);
  const callId = text(record.callId);
  const attachment = text(record.attachment);
  const named = [path, callId, attachment].filter((value) => value !== undefined);

  if (named.length === 0) {
    return {
      refused: "a delegation must name what to look at: a file path, a call id, or an attachment",
    };
  }
  if (named.length > 1) {
    return { refused: "a delegation names one thing to look at, not several" };
  }

  if (path !== undefined) return { question, source: { kind: "file", path } };
  if (callId !== undefined) return { question, source: { kind: "output", callId } };
  return { question, source: { kind: "attachment", name: attachment as string } };
}

/** Where the content of a named source is found. Injected for testing. */
export interface Sources {
  /** The session's project directory, which a file must stay inside. */
  projectRoot: string;
  readFile(path: string): Promise<string>;
  /** The output a call produced, or undefined when there is no such call. */
  outputOf(callId: string): string | undefined;
  /** An attachment's text, or undefined when it is not one this session has. */
  attachment(name: string): string | undefined;
}

/**
 * Reads what a delegation names.
 *
 * A file is held to the same containment rule as the agent itself, so a
 * delegation cannot read what the session could not.
 */
export async function resolveSource(
  delegation: Delegation,
  sources: Sources,
): Promise<Resolved | Refused> {
  const { question, source } = delegation;

  if (source.kind === "file") {
    const path = within(sources.projectRoot, source.path);
    if (path === undefined) {
      return { refused: `${source.path} is outside this session's project` };
    }
    try {
      return { question, describes: source.path, content: await sources.readFile(path) };
    } catch {
      return { refused: `${source.path} could not be read` };
    }
  }

  if (source.kind === "output") {
    const content = sources.outputOf(source.callId);
    if (content === undefined) {
      return { refused: `no call in this session has the id ${source.callId}` };
    }
    return { question, describes: `the output of ${source.callId}`, content };
  }

  const content = sources.attachment(source.name);
  if (content === undefined) {
    return { refused: `nothing called ${source.name} is attached to this conversation` };
  }
  return { question, describes: source.name, content };
}
