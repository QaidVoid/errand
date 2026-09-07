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
