/**
 * Resolves a project from the first message of a session.
 *
 * A message may name a project as a `name:` prefix. The name becomes a
 * directory under the configured root, created on first use. Without a prefix a
 * session works in a directory of its own.
 *
 * This decides which directory an agent gets write access to, so a name that is
 * not a plain single path segment is rejected outright rather than sanitised.
 * Resolution is then checked against the root a second time, because a name can
 * be harmless while the path it lands on is a symlink pointing elsewhere.
 */

import { join, resolve, SEPARATOR } from "@std/path";

/**
 * A valid project name: one path segment, no separators, no leading dot.
 *
 * Excluding `.` as a first character is what makes `.` and `..`
 * unrepresentable, and the character class excludes both separators so a name
 * can never span segments.
 */
export const PROJECT_NAME = /^[A-Za-z0-9_][A-Za-z0-9._-]{0,63}$/;

/** Whether a name is a single safe path segment. */
export function isValidProjectName(name: string): boolean {
  return PROJECT_NAME.test(name);
}

/** The project a session will run in, and the prompt with any prefix removed. */
export interface ProjectSelection {
  /** Project name, which is also its directory name under the root. */
  name: string;
  /** Absolute host path. Always inside the configured root. */
  path: string;
  /** The first prompt, with a recognised project prefix stripped. */
  prompt: string;
  /** Whether the prefix named the project explicitly. */
  wasExplicit: boolean;
}

/** Raised when a resolved project path would land outside the root. */
export class ProjectEscapeError extends Error {
  constructor(name: string, path: string, root: string) {
    super(`project ${name} resolves to ${path}, which is outside the project root ${root}`);
    this.name = "ProjectEscapeError";
  }
}

/** Matches a leading `name:` on the first line, before any validation. */
const PREFIX = /^([^\s:/\\]{1,64}):\s*/;

/**
 * Chooses the project for a session, without touching the filesystem.
 *
 * A named project is a place to come back to: the same name reaches the same
 * directory across sessions. An unnamed one gets a directory of its own, so
 * casual work never lands on top of another session's, or your own from an
 * hour ago.
 *
 * A prefix that is not a valid project name is left as ordinary prompt text,
 * so a malformed name can never redirect a session rather than merely failing
 * to select one.
 */
export function selectProject(
  message: string,
  root: string,
  fallbackName: string,
): ProjectSelection {
  const match = PREFIX.exec(message);
  const candidate = match?.[1];
  if (match !== null && candidate !== undefined && isValidProjectName(candidate)) {
    return {
      name: candidate,
      path: join(root, candidate),
      prompt: message.slice(match[0].length).trim(),
      wasExplicit: true,
    };
  }

  return {
    name: fallbackName,
    path: join(root, fallbackName),
    prompt: message.trim(),
    wasExplicit: false,
  };
}

/**
 * Creates the project directory if it does not exist and confirms it really is
 * inside the root.
 *
 * The containment check runs after resolution, not before: the name can be a
 * clean single segment while the directory it names is a symlink out of the
 * root, and only the resolved path reveals that.
 *
 * @throws ProjectEscapeError when the resolved path is outside the root.
 */
export function ensureProjectDirectory(selection: ProjectSelection, root: string): void {
  Deno.mkdirSync(selection.path, { recursive: true });

  const realRoot = Deno.realPathSync(resolve(root));
  const realPath = Deno.realPathSync(resolve(selection.path));

  if (realPath !== realRoot && !realPath.startsWith(realRoot + SEPARATOR)) {
    throw new ProjectEscapeError(selection.name, realPath, realRoot);
  }
}
