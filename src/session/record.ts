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
