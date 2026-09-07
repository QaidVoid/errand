/**
 * Files somebody attached to a message, taken into a session's project.
 *
 * An attachment is bytes from whoever can post in the channel, so it is
 * written where the daemon says and never where the sender says. The name is
 * derived from what was sent and then resolved by the same containment rule
 * that confines the agent, so a name aiming outside the project cannot get
 * there.
 *
 * They land under a directory of their own rather than the project root,
 * because a file handed over is an input rather than the work: an attachment
 * called README.md must not sit on top of the project's own.
 */

import { extname, join } from "@std/path";
import { hostPathUnder } from "../sandbox/paths.ts";
import type { RawAttachment } from "../chat/inbound.ts";

/** The directory attachments are written to, relative to the project. */
export const ATTACHMENTS_DIR = "attachments";

/** A file that was taken, and where it went. */
export interface Taken {
  /** The path within the project, which is what the agent is told. */
  path: string;
  bytes: Uint8Array;
  contentType: string | undefined;
}

/** A file that was not taken, and why, in words worth posting. */
export interface Refused {
  name: string;
  reason: string;
}

/** What came of a message's attachments. */
export interface Outcome {
  taken: Taken[];
  refused: Refused[];
}

/** Limits on what may arrive. */
export interface Limits {
  maxBytes: number;
  maxCount: number;
}

/** Fetches a file. Injected so tests need no network. */
export type Fetch = (url: string) => Promise<Uint8Array>;

/**
 * Reduces a sender's filename to one path segment.
 *
 * Everything that could make it a path is removed rather than escaped, because
 * a name is a label here and never a location.
 */
function safeName(name: string): string {
  const bare = name.replace(/[\\/]/g, "_").replace(/^\.+/, "").trim();
  const cleaned = bare.replace(/[^A-Za-z0-9._-]/g, "_").slice(0, 80);
  return cleaned.length > 0 ? cleaned : "attachment";
}

function exists(path: string): boolean {
  try {
    Deno.lstatSync(path);
    return true;
  } catch {
    return false;
  }
}

/**
 * A path that is free, by adding a number rather than replacing what is there.
 *
 * An attachment silently overwriting a file would be the worst outcome of
 * somebody being helpful.
 */
function freePath(directory: string, name: string): string {
  const extension = extname(name);
  const stem = name.slice(0, name.length - extension.length);
  let attempt = join(directory, name);
  let next = 2;
  while (exists(attempt)) {
    attempt = join(directory, `${stem}-${next}${extension}`);
    next += 1;
  }
  return attempt;
}

/** Whether a file is one the model could be asked to look at. */
export function isImage(contentType: string | undefined, name: string): boolean {
  if (contentType?.startsWith("image/") === true) return true;
  return /\.(png|jpe?g|gif|webp)$/i.test(name);
}

/**
 * Takes what was attached into the project.
 *
 * Refusals are collected rather than thrown, so one oversized file does not
 * lose the message it came with.
 */
export async function receive(
  attachments: readonly RawAttachment[],
  projectPath: string,
  limits: Limits,
  fetchFile: Fetch,
): Promise<Outcome> {
  const taken: Taken[] = [];
  const refused: Refused[] = [];
  const tooBig = `larger than the ${limits.maxBytes} byte limit`;

  for (const [index, file] of attachments.entries()) {
    if (index >= limits.maxCount) {
      refused.push({
        name: file.name,
        reason: `more than ${limits.maxCount} file(s) on one message`,
      });
      continue;
    }

    if (file.size > limits.maxBytes) {
      refused.push({ name: file.name, reason: tooBig });
      continue;
    }

    let bytes: Uint8Array;
    try {
      bytes = await fetchFile(file.url);
    } catch (error) {
      refused.push({ name: file.name, reason: `could not be fetched: ${String(error)}` });
      continue;
    }

    // Checked again against what actually arrived, because the size the
    // service reported is a claim until the bytes are in hand.
    if (bytes.length > limits.maxBytes) {
      refused.push({ name: file.name, reason: tooBig });
      continue;
    }

    const directory = hostPathUnder(projectPath, projectPath, ATTACHMENTS_DIR);
    if (directory === undefined) {
      refused.push({ name: file.name, reason: "the project has nowhere to put it" });
      continue;
    }

    try {
      Deno.mkdirSync(directory, { recursive: true });
      const target = freePath(directory, safeName(file.name));
      Deno.writeFileSync(target, bytes, { mode: 0o600 });
      taken.push({
        path: `${ATTACHMENTS_DIR}/${target.slice(directory.length + 1)}`,
        bytes,
        contentType: file.contentType,
      });
    } catch (error) {
      refused.push({ name: file.name, reason: `could not be saved: ${String(error)}` });
    }
  }

  return { taken, refused };
}
