/**
 * Where the daemon keeps what a session must not be able to rewrite.
 *
 * A session's state directory is handed to it as `/state`, writable, because
 * the agent genuinely needs somewhere to keep its home, its notes, and the
 * file it asks for a pull request with. Two things were living there that are
 * not the agent's to change: the transcript, which is the record of what
 * happened, and the mark saying a person asked for a pull request, which is
 * the thing that makes the request attributable at all.
 *
 * Both move here, to a directory beside the state one that is never granted.
 * Beside rather than beneath, because the grant covers the state directory
 * whole and anything under it comes with it.
 */

import { join } from "@std/path";
import { TRANSCRIPT_FILENAME } from "./transcript.ts";

/** The directory holding a session's record, given its state directory. */
export function recordDir(stateDir: string): string {
  return `${stateDir}.record`;
}

/**
 * The transcript to read, preferring the record directory.
 *
 * A session written before the record directory existed kept its transcript in
 * the state directory, and that history is still worth showing, so the older
 * place is read when the newer one holds nothing. Writing always goes to the
 * newer one, which {@link prepareRecordDir} has moved anything older into.
 */
export function transcriptPath(stateDir: string): string {
  const placed = join(recordDir(stateDir), TRANSCRIPT_FILENAME);
  if (exists(placed)) return placed;
  const legacy = join(stateDir, TRANSCRIPT_FILENAME);
  return exists(legacy) ? legacy : placed;
}

/**
 * Makes the record directory, moving a transcript left in the older place.
 *
 * The move is what takes an existing session's history out of the agent's
 * reach; without it a resumed thread would keep appending where the agent can
 * still rewrite. A failure to move is not a failure to start: the session
 * matters more than where its record sits, and the next attempt tries again.
 */
export function prepareRecordDir(stateDir: string): string {
  const directory = recordDir(stateDir);
  Deno.mkdirSync(directory, { recursive: true });

  const placed = join(directory, TRANSCRIPT_FILENAME);
  const legacy = join(stateDir, TRANSCRIPT_FILENAME);
  if (!exists(placed) && exists(legacy)) {
    try {
      Deno.renameSync(legacy, placed);
    } catch {
      // Left where it is; it is still read, and still shown.
    }
  }
  return directory;
}

function exists(path: string): boolean {
  try {
    return Deno.statSync(path).isFile;
  } catch {
    return false;
  }
}

/**
 * Takes a withdrawn message out of the record, in place.
 *
 * The entry stays and loses its text. A transcript that quietly drops a turn
 * reads as one that never had it, and the replies around it stop making sense;
 * saying that something was withdrawn keeps the conversation followable and is
 * also the honest thing to show.
 *
 * Written to a new file and renamed over the old one, so a reader sees the
 * whole of one version or the whole of the other. A line that cannot be parsed
 * is carried across untouched rather than dropped: this is a redaction, not a
 * repair.
 *
 * @returns the text that was withdrawn, or undefined when nothing matched.
 *   The caller needs it: the agent keeps no chat id, so what was said is the
 *   only thing the two records have in common.
 */
export async function withdrawFromRecord(
  stateDir: string,
  messageId: string,
): Promise<string | undefined> {
  const path = transcriptPath(stateDir);
  let text: string;
  try {
    text = await Deno.readTextFile(path);
  } catch {
    return undefined;
  }

  let said: string | undefined;
  const lines = text.split("\n").map((line) => {
    if (line.trim().length === 0) return line;
    let parsed: { entry?: { call?: string; id?: string; text?: string } };
    try {
      parsed = JSON.parse(line);
    } catch {
      return line;
    }
    const entry = parsed.entry;
    if (entry === undefined || entry.id !== messageId) return line;
    if (entry.call !== "prompt" && entry.call !== "aside") return line;
    if (typeof entry.text === "string" && entry.text.length > 0) said = entry.text;
    return JSON.stringify({
      ...parsed,
      entry: { ...entry, text: "", withdrawn: true },
    });
  });
  if (said === undefined) return undefined;

  const staging = `${path}.withdrawing`;
  await Deno.writeTextFile(staging, lines.join("\n"));
  await Deno.rename(staging, path);
  return said;
}

/**
 * Takes a withdrawn message out of the agent's own stored conversation.
 *
 * This is the copy that decides what a resumed session sends to a model, and
 * it belongs to the agent rather than to the daemon. Two things make writing it
 * safe enough to do: the agent appends and closes rather than holding the file
 * open, and the caller only reaches here between turns.
 *
 * The record keeps its identity and loses its words. The file is a chain by
 * `parentId`, so removing an entry would strand everything after it; what is
 * left is a message that says it was withdrawn, which reads correctly and sends
 * nothing.
 *
 * A record that cannot be parsed is carried across untouched. Repairing the
 * agent's file is not this function's business, and a withdrawal is no reason
 * to start.
 *
 * @returns whether anything was withdrawn.
 */
export async function withdrawFromAgentSession(
  stateDir: string,
  said: string,
): Promise<boolean> {
  const dir = join(stateDir, "sessions");
  let entries: Deno.DirEntry[];
  try {
    entries = [...Deno.readDirSync(dir)];
  } catch {
    return false;
  }

  let withdrew = false;
  for (const entry of entries) {
    if (!entry.isFile || !entry.name.endsWith(".jsonl")) continue;
    const path = join(dir, entry.name);
    let text: string;
    try {
      text = await Deno.readTextFile(path);
    } catch {
      continue;
    }

    let found = false;
    const lines = text.split("\n").map((line) => {
      if (line.trim().length === 0) return line;
      let parsed: Record<string, unknown>;
      try {
        parsed = JSON.parse(line);
      } catch {
        return line;
      }
      if (!carriesWithdrawn(parsed, said)) return line;
      found = true;
      const message = parsed.message as Record<string, unknown>;
      return JSON.stringify({
        ...parsed,
        message: { ...message, content: [{ type: "text", text: WITHDRAWN_TEXT }] },
      });
    });
    if (!found) continue;

    const staging = `${path}.withdrawing`;
    await Deno.writeTextFile(staging, lines.join("\n"));
    await Deno.rename(staging, path);
    withdrew = true;
  }
  return withdrew;
}

/** What stands in for a withdrawn message, so the conversation still follows. */
const WITHDRAWN_TEXT = "[a message here was withdrawn by the person who sent it]";

/**
 * Whether a stored record is the user message that carried what was withdrawn.
 *
 * Matched on the words, because there is nothing else to match on: the chat's
 * own id never reaches the agent. errand wraps a prompt with context before
 * sending it, so the stored text contains what was said rather than equalling
 * it.
 */
function carriesWithdrawn(parsed: Record<string, unknown>, said: string): boolean {
  const message = parsed.message;
  if (typeof message !== "object" || message === null) return false;
  const fields = message as { role?: unknown; content?: unknown };
  if (fields.role !== "user" || !Array.isArray(fields.content)) return false;
  return fields.content.some((block) =>
    typeof block === "object" && block !== null &&
    typeof (block as { text?: unknown }).text === "string" &&
    (block as { text: string }).text.includes(said)
  );
}
