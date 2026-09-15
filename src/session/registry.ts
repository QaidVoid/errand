/**
 * Remembers which thread belonged to which session, across daemon restarts.
 *
 * The agent keeps its own conversation history in the session state directory,
 * so resuming a thread needs only enough to find that directory again. That is
 * what is stored, and nothing else: no message content and no credential.
 *
 * This is the daemon's only durable state, so it is written carefully. Losing
 * it does not stop the daemon starting, but it does silently withdraw every
 * thread anybody had open, which is the kind of failure that gets blamed on
 * the chat service.
 */

import { dirname, join } from "@std/path";
import type { Logger } from "../log.ts";

/** Filename of the index inside the daemon's state directory. */
export const REGISTRY_FILENAME = "threads.json";

/** How many threads are remembered before the oldest are dropped. */
export const MAX_REMEMBERED = 500;

/** What is needed to put a thread back to work after a restart. */
export interface ThreadRecord {
  threadId: string;
  sessionId: string;
  /** Where the agent's own session history lives. */
  stateDir: string;
  projectName: string;
  projectPath: string;
  /** Who started the session, so ownership survives a restart. */
  ownerId: string;
  /**
   * Accounts the owner invited to take part.
   *
   * Kept so a restart does not silently withdraw access somebody was given,
   * which would look like the bot ignoring them.
   */
  guests: string[];
  /**
   * The provider and model the session was started on, when they were chosen
   * rather than taken from the configuration.
   *
   * Kept so a restart puts the thread back on the model it was working with.
   * Coming back on a different one is a change nobody asked for, and a quiet
   * one: the answers simply start reading differently.
   */
  provider?: string | undefined;
  model?: string | undefined;
  /** Milliseconds since the epoch, for evicting the least recently used. */
  updatedAt: number;
}

export function isRecord(value: unknown): value is ThreadRecord {
  if (typeof value !== "object" || value === null) return false;
  const record = value as Record<string, unknown>;
  return (
    typeof record.threadId === "string" &&
    typeof record.sessionId === "string" &&
    typeof record.stateDir === "string" &&
    typeof record.projectName === "string" &&
    typeof record.projectPath === "string" &&
    typeof record.ownerId === "string" &&
    Array.isArray(record.guests) &&
    record.guests.every((guest) => typeof guest === "string") &&
    (record.provider === undefined || typeof record.provider === "string") &&
    (record.model === undefined || typeof record.model === "string") &&
    typeof record.updatedAt === "number"
  );
}

/** A durable thread-to-session index. */
export class ThreadRegistry {
  private readonly records = new Map<string, ThreadRecord>();

  constructor(private readonly path: string, private readonly log: Logger) {}

  /** The index path inside a daemon state directory. */
  static pathFor(stateDir: string): string {
    return join(stateDir, REGISTRY_FILENAME);
  }

  /**
   * Reads the index.
   *
   * A missing file is an empty index, which is the state on a first run. A
   * file that will not parse is kept aside rather than overwritten, because it
   * is the only copy of what was there and something has to be able to look at
   * it afterwards.
   */
  load(): void {
    this.records.clear();

    let text: string;
    try {
      text = Deno.readTextFileSync(this.path);
    } catch (error) {
      if (!(error instanceof Deno.errors.NotFound)) {
        this.log.error("the thread index could not be read, so no thread can be resumed", {
          path: this.path,
          detail: String(error),
        });
      }
      return;
    }

    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch (error) {
      this.setAside(String(error));
      return;
    }
    if (!Array.isArray(parsed)) {
      this.setAside("the index is not a list of threads");
      return;
    }

    let skipped = 0;
    for (const entry of parsed) {
      if (isRecord(entry)) this.records.set(entry.threadId, entry);
      else skipped += 1;
    }
    if (skipped > 0) {
      this.log.warn("entries in the thread index were not readable and were dropped", { skipped });
    }
  }

  /** The record for a thread, if one was ever kept. */
  get(threadId: string): ThreadRecord | undefined {
    return this.records.get(threadId);
  }

  /** Number of threads currently remembered. */
  get size(): number {
    return this.records.size;
  }

  /** Every remembered thread, most recently used first. */
  all(): ThreadRecord[] {
    return [...this.records.values()].sort((a, b) => b.updatedAt - a.updatedAt);
  }

  /** Records a thread, evicting the least recently used past the bound. */
  remember(record: ThreadRecord): void {
    this.records.set(record.threadId, record);

    if (this.records.size > MAX_REMEMBERED) {
      const ordered = [...this.records.values()].sort((a, b) => a.updatedAt - b.updatedAt);
      for (const stale of ordered.slice(0, this.records.size - MAX_REMEMBERED)) {
        this.records.delete(stale.threadId);
      }
    }

    this.save();
  }

  /** Forgets a thread, so it is never resumed again. */
  forget(threadId: string): void {
    if (this.records.delete(threadId)) this.save();
  }

  /**
   * Writes the index so that a crash leaves either the old one or the new one.
   *
   * Written beside the target, flushed, and renamed. The rename is what makes
   * it atomic; the flush before it is what stops a crash leaving a file that
   * was renamed into place before its contents reached the disk. The directory
   * is flushed afterwards so the rename itself survives.
   */
  private save(): void {
    const temporary = `${this.path}.tmp`;
    const body = `${JSON.stringify([...this.records.values()], null, 2)}\n`;

    try {
      Deno.mkdirSync(dirname(this.path), { recursive: true });

      const file = Deno.openSync(temporary, { write: true, create: true, truncate: true });
      try {
        file.writeSync(new TextEncoder().encode(body));
        file.syncSync();
      } finally {
        file.close();
      }
      Deno.chmodSync(temporary, 0o600);
      Deno.renameSync(temporary, this.path);
      this.syncDirectory();
    } catch (error) {
      // Loud, because the cost is not visible until a restart, by which time
      // the threads are gone and nothing says why.
      this.log.error("the thread index could not be written, so a restart will forget threads", {
        path: this.path,
        detail: String(error),
      });
      try {
        Deno.removeSync(temporary);
      } catch {
        // Nothing to remove, which is the state we wanted.
      }
    }
  }

  private syncDirectory(): void {
    let directory: Deno.FsFile;
    try {
      directory = Deno.openSync(dirname(this.path), { read: true });
    } catch {
      // Not every platform allows opening a directory. The rename still
      // happened; only its durability across a power loss is weaker.
      return;
    }
    try {
      directory.syncSync();
    } catch {
      // As above.
    } finally {
      directory.close();
    }
  }

  /** Keeps an unreadable index rather than overwriting the only copy. */
  private setAside(reason: string): void {
    const kept = `${this.path}.broken`;
    try {
      Deno.renameSync(this.path, kept);
      this.log.error("the thread index was unreadable and was kept aside; no thread can resume", {
        kept,
        detail: reason,
      });
    } catch (error) {
      this.log.error("the thread index was unreadable and could not be kept aside", {
        path: this.path,
        detail: String(error),
      });
    }
  }
}
