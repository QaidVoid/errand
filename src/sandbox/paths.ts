/**
 * Whether a path stays inside the directory a session is confined to.
 *
 * One rule, used everywhere a path arrives from outside: from the agent, from
 * a delegation, from a chat message. A second rule written slightly
 * differently is how a containment check ends up being true in one place and
 * false in another.
 */

import { normalize, resolve, SEPARATOR } from "@std/path";

/**
 * Resolves `wanted` against `root` and returns it only if it stays inside.
 *
 * Traversal is removed before the comparison rather than searched for, so a
 * path does not have to be recognised as hostile to be refused. The root
 * itself counts as inside.
 *
 * @returns the resolved absolute path, or undefined when it escapes.
 */
export function within(root: string, wanted: string): string | undefined {
  const base = resolve(root);
  const target = resolve(base, normalize(wanted));
  if (target === base) return target;
  return target.startsWith(base + SEPARATOR) ? target : undefined;
}
