/**
 * Managing threads and what they left on disk, from the terminal.
 *
 * Deliberately not reachable from chat or from the interface. Removing a
 * thread's data is destructive and irreversible, and the people who can post
 * in a channel are not the same people who administer the host it runs on.
 *
 * Everything here reads the same index the daemon writes. Running it while the
 * daemon is up is safe for the commands that only read; the ones that change
 * the index say so.
 */

import { basename } from "@std/path";
import type { ThreadRecord, ThreadRegistry } from "../session/registry.ts";

/** What the commands need, injected so a test needs no disk and no clock. */
export interface Deps {
  registry: ThreadRegistry;
  /** Bytes a thread's state directory holds, or undefined when it is gone. */
  sizeOf(stateDir: string): Promise<number | undefined>;
  /** Deletes a thread's state directory. */
  remove(stateDir: string): Promise<void>;
  write(line: string): void;
  now(): number;
}

const USAGE = [
  "usage: errand threads <command>",
  "",
  "  list                 every remembered thread, most recent first",
  "  show <thread>        one thread in full, with what it holds on disk",
  "  forget <thread>      stop resuming it, and keep its data",
  "  remove <thread>      forget it and delete its data, needs --yes",
  "  prune                forget threads whose data is already gone",
].join("\n");

function humanSize(bytes: number): string {
  const units = ["B", "K", "M", "G", "T"];
  let size = bytes;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${unit === 0 ? size : size.toFixed(1)}${units[unit]}`;
}

function ago(at: number, now: number): string {
  const seconds = Math.max(0, Math.round((now - at) / 1000));
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
  if (seconds < 86_400) return `${Math.round(seconds / 3600)}h`;
  return `${Math.round(seconds / 86_400)}d`;
}

/**
 * Finds a thread by its id, or by a unique part of one.
 *
 * A thread id is a snowflake: nineteen digits that differ only near the end,
 * so the tail is what somebody copies and what tells two of them apart. The
 * head is matched too, since that is what people try first.
 */
export function findThread(
  records: readonly ThreadRecord[],
  wanted: string,
): ThreadRecord | { ambiguous: ThreadRecord[] } | undefined {
  const exact = records.find((record) => record.threadId === wanted);
  if (exact !== undefined) return exact;

  for (
    const matches of [
      (id: string) => id.endsWith(wanted),
      (id: string) => id.startsWith(wanted),
    ]
  ) {
    const matching = records.filter((record) => matches(record.threadId));
    if (matching.length === 1) return matching[0];
    if (matching.length > 1) return { ambiguous: matching };
  }
  return undefined;
}

function isAmbiguous(
  value: ThreadRecord | { ambiguous: ThreadRecord[] } | undefined,
): value is { ambiguous: ThreadRecord[] } {
  return value !== undefined && "ambiguous" in value;
}

async function list(deps: Deps): Promise<number> {
  const records = deps.registry.all();
  if (records.length === 0) {
    deps.write("no threads are remembered");
    return 0;
  }

  const widest = Math.max(6, ...records.map((record) => record.threadId.length));
  deps.write(`${"thread".padEnd(widest)}  ${"project".padEnd(18)}    used     size  owner`);
  for (const record of records) {
    const bytes = await deps.sizeOf(record.stateDir);
    deps.write(
      [
        record.threadId.padEnd(widest),
        record.projectName.slice(0, 18).padEnd(18),
        ago(record.updatedAt, deps.now()).padStart(6),
        (bytes === undefined ? "gone" : humanSize(bytes)).padStart(7),
        record.ownerId,
      ].join("  "),
    );
  }
  return 0;
}

async function show(deps: Deps, wanted: string | undefined): Promise<number> {
  if (wanted === undefined) {
    deps.write("say which thread, as `errand threads show <thread>`");
    return 2;
  }

  const found = findThread(deps.registry.all(), wanted);
  if (found === undefined) {
    deps.write(`no thread here is called ${wanted}`);
    return 1;
  }
  if (isAmbiguous(found)) {
    deps.write(`${wanted} matches ${found.ambiguous.length} threads; give more of the id`);
    return 1;
  }

  const bytes = await deps.sizeOf(found.stateDir);
  deps.write(`thread    ${found.threadId}`);
  deps.write(`session   ${found.sessionId}`);
  deps.write(`project   ${found.projectName}  ${found.projectPath}`);
  deps.write(`state     ${found.stateDir}  ${bytes === undefined ? "gone" : humanSize(bytes)}`);
  deps.write(`owner     ${found.ownerId}`);
  deps.write(`guests    ${found.guests.length === 0 ? "none" : found.guests.join(", ")}`);
  deps.write(`used      ${ago(found.updatedAt, deps.now())} ago`);
  return 0;
}

function forget(deps: Deps, wanted: string | undefined): number {
  if (wanted === undefined) {
    deps.write("say which thread, as `errand threads forget <thread>`");
    return 2;
  }

  const found = findThread(deps.registry.all(), wanted);
  if (found === undefined || isAmbiguous(found)) {
    deps.write(
      found === undefined
        ? `no thread here is called ${wanted}`
        : `${wanted} matches several threads; give more of the id`,
    );
    return 1;
  }

  deps.registry.forget(found.threadId);
  deps.write(`forgot ${found.threadId}; its data is still at ${found.stateDir}`);
  return 0;
}

async function remove(deps: Deps, wanted: string | undefined, confirmed: boolean): Promise<number> {
  if (wanted === undefined) {
    deps.write("say which thread, as `errand threads remove <thread> --yes`");
    return 2;
  }

  const found = findThread(deps.registry.all(), wanted);
  if (found === undefined || isAmbiguous(found)) {
    deps.write(
      found === undefined
        ? `no thread here is called ${wanted}`
        : `${wanted} matches several threads; give more of the id`,
    );
    return 1;
  }

  // Asked for rather than assumed: this deletes the agent's history, and
  // nothing else keeps a copy of it.
  if (!confirmed) {
    const bytes = await deps.sizeOf(found.stateDir);
    deps.write(
      `this deletes ${found.stateDir}${bytes === undefined ? "" : ` and its ${humanSize(bytes)}`}`,
    );
    deps.write(`run it again with --yes to go ahead`);
    return 1;
  }

  try {
    await deps.remove(found.stateDir);
  } catch (error) {
    deps.write(`could not delete ${found.stateDir}: ${error}`);
    return 1;
  }
  deps.registry.forget(found.threadId);
  deps.write(`removed ${found.threadId} and deleted ${found.stateDir}`);
  return 0;
}

async function prune(deps: Deps): Promise<number> {
  let forgotten = 0;
  for (const record of deps.registry.all()) {
    if ((await deps.sizeOf(record.stateDir)) !== undefined) continue;
    deps.registry.forget(record.threadId);
    deps.write(`forgot ${record.threadId}, whose data at ${basename(record.stateDir)} is gone`);
    forgotten += 1;
  }
  deps.write(
    forgotten === 0 ? "every remembered thread still has its data" : `forgot ${forgotten}`,
  );
  return 0;
}

/**
 * Runs one thread command.
 *
 * @returns the process exit code: 0 for done, 1 for refused, 2 for misused.
 */
export function runThreads(args: readonly string[], deps: Deps): Promise<number> {
  const [command, target] = args;
  const confirmed = args.includes("--yes");

  switch (command) {
    case "list":
    case undefined:
      return list(deps);
    case "show":
      return show(deps, target);
    case "forget":
      return Promise.resolve(forget(deps, target));
    case "remove":
      return remove(deps, target, confirmed);
    case "prune":
      return prune(deps);
    case "--help":
    case "help":
      deps.write(USAGE);
      return Promise.resolve(0);
    default:
      deps.write(`there is no threads command called ${command}`);
      deps.write(USAGE);
      return Promise.resolve(2);
  }
}
