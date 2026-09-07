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

/**
 * Translates a path as the agent sees it into a path on the host.
 *
 * A leading separator does not mean the host's root. An absolute path that is
 * not already inside the workspace is read as project-relative, so
 * `/etc/passwd` resolves to a file of that name inside the project rather than
 * to the host's. That keeps one rule whether the agent sees host paths or a
 * mount point.
 *
 * @returns the host path, or undefined when it would leave the project.
 */
export function hostPathUnder(
  workspace: string,
  projectPath: string,
  requested: string,
): string | undefined {
  const trimmed = requested.trim();
  if (trimmed.length === 0) return undefined;

  const relative = trimmed.startsWith(workspace)
    ? trimmed.slice(workspace.length).replace(/^\/+/, "")
    : trimmed.replace(/^\/+/, "");

  return within(projectPath, relative.length === 0 ? "." : relative);
}
