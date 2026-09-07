/**
 * How much a session has written.
 *
 * Measured rather than enforced: no backend caps what a process tree writes in
 * aggregate without a sized filesystem under it, so the daemon watches instead
 * and stops a session that passes its budget.
 */

import { join } from "@std/path";

/**
 * Bytes held under a directory, following no symlink.
 *
 * A link is counted as the link rather than as what it points at, so a session
 * cannot appear to hold a hundred gigabytes by linking to one, and cannot hide
 * what it wrote by linking out of its own directory either.
 *
 * @returns the total, or undefined when the directory is not there.
 */
export async function treeBytes(root: string): Promise<number | undefined> {
  let total = 0;
  const pending = [root];

  try {
    await Deno.lstat(root);
  } catch {
    return undefined;
  }

  while (pending.length > 0) {
    const directory = pending.pop() as string;
    let entries: AsyncIterable<Deno.DirEntry>;
    try {
      entries = Deno.readDir(directory);
    } catch {
      continue;
    }

    for await (const entry of entries) {
      const path = join(directory, entry.name);
      if (entry.isDirectory) {
        pending.push(path);
        continue;
      }
      try {
        total += (await Deno.lstat(path)).size;
      } catch {
        // Gone between listing and measuring, which a running session does.
      }
    }
  }
  return total;
}

/** What a check of a session's disk use concluded. */
export type Verdict = "under" | "close" | "over";

/** The share of the budget at which a session is warned rather than stopped. */
export const WARN_AT = 0.8;

/** Where a session's use sits against its budget. */
export function verdict(used: number, budget: number): Verdict {
  if (budget <= 0) return "under";
  if (used >= budget) return "over";
  return used >= budget * WARN_AT ? "close" : "under";
}

/** Fastest a session's use is measured, however quickly it is growing. */
export const MIN_CHECK_MS = 1_000;

/**
 * How long to wait before measuring again, from how fast the session is
 * writing now.
 *
 * A fixed interval decides the overshoot. Measured every 30 seconds, a session
 * writing a gigabyte a second is 20 GB past a 5 GB budget before anything
 * notices, which is what a real session did. Aiming at half the time the
 * current rate needs to reach the budget keeps the check ahead of the writing,
 * while an idle session settles back to the configured interval rather than
 * walking the tree every second for nothing.
 */
export function nextCheckMs(
  written: number,
  lastWritten: number,
  budget: number,
  sinceLast: number,
  slowest: number,
): number {
  const grew = written - lastWritten;
  if (grew <= 0 || sinceLast <= 0) return slowest;

  const perMs = grew / sinceLast;
  const remaining = Math.max(0, budget - written);
  const projected = (remaining / perMs) * 0.5;
  return Math.max(MIN_CHECK_MS, Math.min(slowest, Math.round(projected)));
}
