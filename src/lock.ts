/**
 * Ensures one daemon per state directory.
 *
 * Two daemons on one bot token both receive every message and both act on it:
 * two sandboxes, two model bills, and a race to create the thread that one of
 * them loses with a confusing error in the channel. Nothing about the chat
 * connection prevents that, so it is prevented here.
 *
 * The lock holds a process id. One left by a process that no longer exists is
 * stale and is taken over, because a daemon that was killed must not stop the
 * next one from starting.
 */

import { join } from "@std/path";

/** Filename of the lock inside the daemon state directory. */
export const LOCK_FILENAME = "daemon.lock";

/** Raised when another daemon is already running against this state directory. */
export class AlreadyRunningError extends Error {
  constructor(readonly pid: number, readonly path: string) {
    super(
      [
        `another errand daemon is already running as process ${pid}.`,
        "Two daemons on one bot token both act on every message.",
        `Stop that one first, or remove ${path} if you are certain it is gone.`,
      ].join(" "),
    );
    this.name = "AlreadyRunningError";
  }
}

/**
 * Whether a process with this id exists.
 *
 * Probed with SIGURG, whose default disposition is to be ignored, so the
 * target is not disturbed by being asked about. Being refused permission
 * counts as running: the process is there, it just belongs to somebody else,
 * and that is not evidence that the lock is stale.
 */
export function isRunning(pid: number): boolean {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    Deno.kill(pid, "SIGURG");
    return true;
  } catch (error) {
    return error instanceof Deno.errors.PermissionDenied;
  }
}

/** A held lock, released on shutdown. */
export interface DaemonLock {
  path: string;
  release(): void;
}

/**
 * Takes the single-instance lock.
 *
 * @throws AlreadyRunningError when a live daemon already holds it.
 */
export function acquireLock(stateDir: string, pid: number = Deno.pid): DaemonLock {
  const path = join(stateDir, LOCK_FILENAME);

  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      Deno.writeTextFileSync(path, `${pid}\n`, { createNew: true, mode: 0o600 });
      break;
    } catch (error) {
      if (!(error instanceof Deno.errors.AlreadyExists)) throw error;

      const held = Number.parseInt(Deno.readTextFileSync(path).trim(), 10);
      if (isRunning(held)) throw new AlreadyRunningError(held, path);

      // The holder is gone, so the lock is stale. Clearing it and retrying is
      // what keeps a killed daemon from blocking every restart after it.
      try {
        Deno.removeSync(path);
      } catch {
        // Somebody else cleared it first, which is the state we wanted.
      }
    }
  }

  let released = false;
  return {
    path,
    release(): void {
      if (released) return;
      released = true;
      try {
        Deno.removeSync(path);
      } catch {
        // Already gone; the next daemon starts either way.
      }
    },
  };
}
