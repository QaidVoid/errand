/**
 * Durable memory, so the agent knows who it is talking to and where it is.
 *
 * SQLite through `node:sqlite`, which the runtime ships, so this adds no
 * dependency and no server. The data is kilobytes of short facts; anything
 * heavier would be machinery in search of a use.
 *
 * Facts are kept short and injected as plain lines rather than as JSON,
 * because every one of them is paid for in the agent's context on every
 * session that user starts.
 */

import { DatabaseSync } from "node:sqlite";

/**
 * What a fact is about.
 *
 * `user` follows a person between sessions. `project` follows the working
 * directory, so a decision made in one thread is known to the next thread that
 * works there, whoever starts it.
 */
export type Scope = "user" | "project";

/** One remembered fact. */
export interface Fact {
  id: number;
  fact: string;
  createdAt: number;
}

/** Longest single fact kept. Anything longer is a note, not a fact. */
export const MAX_FACT_LENGTH = 300;

/** Default ceiling on the memory block injected into a session. */
export const DEFAULT_MEMORY_BUDGET = 2_000;

/** Facts kept for a project before the oldest are dropped. */
export const MAX_PROJECT_FACTS = 500;

/** Memory backed by one SQLite file. */
export class MemoryStore {
  private readonly db: DatabaseSync;

  constructor(path: string) {
    this.db = new DatabaseSync(path);
    this.db.exec("PRAGMA journal_mode = WAL");
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS users (
        user_id TEXT PRIMARY KEY,
        display_name TEXT,
        updated_at INTEGER NOT NULL
      )
    `);
    // The unique constraint is what makes remembering idempotent: an agent
    // that writes the same fact every session must not grow the block every
    // session.
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS facts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        scope TEXT NOT NULL,
        subject TEXT NOT NULL,
        fact TEXT NOT NULL,
        session_id TEXT,
        created_at INTEGER NOT NULL,
        UNIQUE (scope, subject, fact)
      )
    `);
    this.db.exec("CREATE INDEX IF NOT EXISTS facts_by_subject ON facts (scope, subject, id DESC)");
  }

  /** Records who an account id belongs to, for addressing them by name. */
  rememberUser(userId: string, displayName: string, now: number = Date.now()): void {
    this.db.prepare(
      `INSERT INTO users (user_id, display_name, updated_at) VALUES (?, ?, ?)
       ON CONFLICT (user_id) DO UPDATE SET display_name = excluded.display_name,
                                           updated_at = excluded.updated_at`,
    ).run(userId, displayName, now);
  }

  /** The name last seen for an account, if any. */
  displayName(userId: string): string | undefined {
    const row = this.db
      .prepare("SELECT display_name FROM users WHERE user_id = ?")
      .get(userId) as { display_name: string | null } | undefined;
    return row?.display_name ?? undefined;
  }

  /**
   * Stores one fact.
   *
   * @returns false when the fact was empty or already known, so a caller can
   *   report how much was actually new.
   */
  remember(
    scope: Scope,
    subject: string,
    fact: string,
    sessionId: string,
    now: number = Date.now(),
  ): boolean {
    const trimmed = fact.trim().replace(/\s+/g, " ").slice(0, MAX_FACT_LENGTH);
    if (trimmed.length === 0 || subject.length === 0) return false;

    const written = this.db.prepare(
      `INSERT OR IGNORE INTO facts (scope, subject, fact, session_id, created_at)
       VALUES (?, ?, ?, ?, ?)`,
    ).run(scope, subject, trimmed, sessionId, now);
    const stored = Number(written.changes) > 0;

    // A project accumulates facts from everyone who works on it, so it is the
    // one that needs pruning. Oldest go first; the newest are the live ones.
    if (stored && scope === "project") {
      this.db.prepare(
        `DELETE FROM facts WHERE scope = 'project' AND subject = ? AND id NOT IN (
           SELECT id FROM facts WHERE scope = 'project' AND subject = ?
           ORDER BY id DESC LIMIT ?
         )`,
      ).run(subject, subject, MAX_PROJECT_FACTS);
    }
    return stored;
  }

  /** Facts for one subject, newest first. */
  factsFor(scope: Scope, subject: string, limit = 100): Fact[] {
    const rows = this.db.prepare(
      `SELECT id, fact, created_at FROM facts
       WHERE scope = ? AND subject = ? ORDER BY id DESC LIMIT ?`,
    ).all(scope, subject, limit) as { id: number; fact: string; created_at: number }[];

    return rows.map((row) => ({ id: row.id, fact: row.fact, createdAt: row.created_at }));
  }

  /**
   * Removes everything held for one subject.
   *
   * @returns how many facts went, so somebody asking to be forgotten is told
   *   what was actually there.
   */
  forget(scope: Scope, subject: string): number {
    const gone = this.db
      .prepare("DELETE FROM facts WHERE scope = ? AND subject = ?")
      .run(scope, subject);
    if (scope === "user") {
      this.db.prepare("DELETE FROM users WHERE user_id = ?").run(subject);
    }
    return Number(gone.changes);
  }

  /**
   * Renders a user's memory as the block appended to the agent's system
   * prompt.
   *
   * Newest facts win the budget, because a contradiction is usually a
   * correction.
   *
   * @returns an empty string when there is nothing worth sending, so somebody
   *   new costs no context at all.
   */
  render(userId: string, budget: number = DEFAULT_MEMORY_BUDGET): string {
    const name = this.displayName(userId);
    const facts = this.factsFor("user", userId);
    if (name === undefined && facts.length === 0) return "";

    const header = name === undefined
      ? "You are talking to somebody in a chat thread."
      : `You are talking to ${name}.`;
    const lines = [header];
    let used = header.length;

    if (facts.length > 0) {
      const intro = "What you have been told about them before:";
      lines.push("", intro);
      used += intro.length + 2;
      used = appendWithin(lines, facts, used, budget);
    }

    return lines.join("\n");
  }

  /** Renders what is known about a project, for its session's context. */
  renderProject(project: string, budget: number = DEFAULT_MEMORY_BUDGET): string {
    const facts = this.factsFor("project", project);
    if (facts.length === 0) return "";

    const header = `What earlier conversations recorded about the ${project} project:`;
    const lines = [header];
    appendWithin(lines, facts, header.length, budget);
    return lines.join("\n");
  }

  /**
   * Renders memory for somebody who joins a conversation already in progress.
   *
   * Sent with their first message rather than in the system prompt, which was
   * fixed when the session started. Costs once per speaker per session, not
   * once per turn.
   */
  renderForSpeaker(userId: string, budget: number = DEFAULT_MEMORY_BUDGET): string {
    const block = this.render(userId, budget);
    return block.length === 0 ? "" : `[context: ${block.replace(/\n+/g, " ")}]`;
  }

  /** Closes the database. */
  close(): void {
    this.db.close();
  }
}

/** Adds facts as bullets until the budget is spent. Returns what it used. */
function appendWithin(lines: string[], facts: Fact[], used: number, budget: number): number {
  let spent = used;
  for (const { fact } of facts) {
    const entry = `- ${fact}`;
    if (spent + entry.length + 1 > budget) break;
    lines.push(entry);
    spent += entry.length + 1;
  }
  return spent;
}

/** Filename the agent appends facts about the speaker to. */
export const NOTES_FILENAME = "remember.md";

/** Filename the agent appends facts about the project to. */
export const PROJECT_NOTES_FILENAME = "project-notes.md";

/** Filename of the memory block injected into the agent's system prompt. */
export const BLOCK_FILENAME = "memory.md";

/**
 * The instructions appended to the agent's system prompt.
 *
 * The agent cannot call back into the daemon, by design, so remembering
 * something is writing a line to a file in its own state directory. The daemon
 * reads that file after each turn.
 */
export function memoryInstructions(notesPath: string, projectNotesPath: string): string {
  return [
    "",
    "",
    "You have two kinds of memory, both carried between separate conversations.",
    "One fact per line in either. Never record secrets, credentials, or anything",
    "you were told in confidence.",
    "",
    `About the person speaking: append to ${notesPath}. Their preferences, how`,
    "they want things done, what they are working on. More than one person may",
    "take part here; what is known about somebody new is given to you at the",
    "start of their first message, and facts are held per person.",
    "",
    `About this project: append to ${projectNotesPath}. Decisions taken and why,`,
    "conventions, where things live, approaches already tried and rejected.",
    "Anything a later conversation about this project would otherwise have to",
    "rediscover. This is shared by everyone who works here, so write it for a",
    "reader who was not present.",
  ].join("\n");
}

/**
 * Reads facts an agent wrote, one per line.
 *
 * Blank lines, markdown bullets, and comment lines are tolerated because the
 * agent writes this by hand and will not be consistent about it.
 */
export function parseNotes(contents: string): string[] {
  return contents
    .split("\n")
    .map((line) => line.trim().replace(/^[-*]\s+/, ""))
    .filter((line) => line.length > 0 && !line.startsWith("#"));
}
