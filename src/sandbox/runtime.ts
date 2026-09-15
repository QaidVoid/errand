/**
 * Where the agent's own program lives on this host.
 *
 * A confined session runs the agent the operator installed, and the sandbox
 * hands the target its own PATH rather than the caller's. A policy granting
 * only the project therefore produces a session that starts and immediately
 * fails to exec, which is why these directories are resolved and granted
 * explicitly.
 *
 * Only read access is granted, and only to the directories the agent is loaded
 * from. The operator's home is not granted, and neither is any parent of these.
 */

import { basename, dirname, join } from "@std/path";

/** The PATH a target is given when the policy sets none. */
export const SANDBOX_PATH = ["/usr/local/bin", "/usr/bin", "/bin"];

/** The directories a session needs in order to load and run the agent. */
export interface AgentRuntime {
  /** Directories the policy grants read access to. */
  readPaths: string[];
  /** Directories placed ahead of the sandbox PATH. */
  pathEntries: string[];
}

/** Looks a program up the way a shell would. Injected so tests need no host. */
export type Lookup = (name: string) => string | undefined;

/** Whether a path is a file this user can execute. */
function isExecutable(path: string): boolean {
  try {
    const info = Deno.statSync(path);
    return info.isFile && (info.mode === null || (info.mode & 0o111) !== 0);
  } catch {
    return false;
  }
}

/** Finds a program on PATH, which is what a shell would run for that name. */
export function which(name: string, path = Deno.env.get("PATH") ?? ""): string | undefined {
  for (const directory of path.split(":")) {
    if (directory.length === 0) continue;
    const candidate = join(directory, name);
    if (isExecutable(candidate)) return candidate;
  }
  return undefined;
}

/**
 * The package a file belongs to, found by walking up to its `package.json`.
 *
 * The agent reads its own manifest at startup, so granting the directory the
 * bundle sits in is not enough: it loads and then misreports its own version.
 */
/**
 * The `node_modules` a package was installed into, if it is in one.
 *
 * The nearest one going up, which is where a flat installation keeps every
 * package the agent resolves against. Nothing when the agent was installed
 * some other way, such as a self-contained binary, which needs no such grant.
 */
function installRoot(directory: string): string | undefined {
  let current = directory;
  for (;;) {
    if (basename(current) === "node_modules") return current;
    const parent = dirname(current);
    if (parent === current) return undefined;
    current = parent;
  }
}

function packageRoot(file: string): string | undefined {
  let directory = dirname(file);
  for (;;) {
    try {
      Deno.statSync(join(directory, "package.json"));
      return directory;
    } catch {
      const parent = dirname(directory);
      if (parent === directory) return undefined;
      directory = parent;
    }
  }
}

/** Adds a path once, keeping the order entries were discovered in. */
function add(into: string[], value: string | undefined): void {
  if (value !== undefined && !into.includes(value)) into.push(value);
}

function realPath(path: string): string {
  try {
    return Deno.realPathSync(path);
  } catch {
    return path;
  }
}

/**
 * Resolves what a confined session must read in order to run the agent.
 *
 * Returns undefined when the agent is not installed, which is a reason to
 * refuse to start rather than something to work around.
 */
export function agentRuntime(lookup: Lookup = (name) => which(name)): AgentRuntime | undefined {
  const launcher = lookup("pi");
  if (launcher === undefined) return undefined;

  const readPaths: string[] = [];
  const pathEntries: string[] = [];

  add(pathEntries, dirname(launcher));
  add(readPaths, dirname(launcher));

  // A launcher installed by a package manager is usually a link into the
  // package it belongs to, and the link's own directory holds none of the code.
  const real = realPath(launcher);
  const root = packageRoot(real) ?? dirname(real);
  add(readPaths, root);

  // Its dependencies are packages of their own, sitting beside it rather than
  // inside it, and the runtime finds them by walking up to the `node_modules`
  // they were all installed into. Granting only the agent's own package leaves
  // an import of a sibling failing to resolve, which reads as the agent
  // exiting at startup rather than as anything to do with the sandbox.
  const installed = installRoot(root);
  if (installed !== undefined) add(readPaths, installed);

  // The interpreter named by the launcher's `#!` line. Absent when the agent is
  // a self-contained binary, which needs nothing further granted.
  const node = lookup("node");
  if (node !== undefined) {
    const interpreter = dirname(realPath(node));
    add(pathEntries, interpreter);
    add(readPaths, interpreter);
  }

  return { readPaths, pathEntries };
}
