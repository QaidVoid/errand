/**
 * The daemon's half of a delegation: reading requests and writing answers.
 *
 * The agent writes a request into the one directory both sides can reach, and
 * this picks it up, runs it, and writes the answer beside it. Polling rather
 * than watching, because a request arrives at most a few times a turn and a
 * watch on a directory inside a sandbox is more machinery than that is worth.
 *
 * Nothing here can fail a turn. A request that cannot be answered is refused
 * in words the agent reads, and the work stays with the session's own model.
 */

import { join } from "@std/path";
import { DELEGATE_DIR } from "../agent/requests.ts";
import type { Answer, TurnDelegations } from "../agent/delegate.ts";
import { isRefused, type Refused } from "../agent/delegation.ts";
import type { Logger } from "../log.ts";

/** How often the directory is looked at while a session is running. */
export const POLL_MS = 200;

/** What a delegation produced, for whoever is showing the session. */
export type Outcome = { asked: string } & (Answer | Refused);

/** What the watcher needs to answer a request. */
export interface Watching {
  /** The session's state directory, which holds the exchange directory. */
  stateDir: string;
  /** The turn's delegations, or undefined when no turn is running. */
  delegations: () => TurnDelegations | undefined;
  /** Reports what happened, so a reader sees the delegation and its answer. */
  report: (outcome: Outcome) => void;
  log: Logger;
  setTimeout: (handler: () => void, ms: number) => unknown;
  clearTimeout: (handle: unknown) => void;
}

/**
 * The answer as the agent reads it.
 *
 * Labelled, and naming the model. An agent that forgets it did not read the
 * thing itself starts asserting a description as an observation, which is the
 * one failure this whole shape exists to avoid.
 */
export function labelled(answer: Answer): string {
  return [
    `${answer.model} was asked about ${answer.describes} and said the following.`,
    "It is a description rather than the thing itself, so check it before you",
    "rely on it.",
    "",
    answer.text,
    "",
  ].join("\n");
}

/** Picks up delegation requests for one session and answers them. */
export class Delegating {
  private timer: unknown = null;
  private stopped = false;
  private readonly directory: string;

  constructor(private readonly options: Watching) {
    this.directory = join(options.stateDir, DELEGATE_DIR);
  }

  /** Begins looking for requests. */
  start(): void {
    this.schedule();
  }

  /** Stops looking. Anything outstanding is left for the agent's own deadline. */
  stop(): void {
    this.stopped = true;
    if (this.timer !== null) this.options.clearTimeout(this.timer);
    this.timer = null;
  }

  private schedule(): void {
    if (this.stopped) return;
    this.timer = this.options.setTimeout(() => {
      void this.sweep().finally(() => this.schedule());
    }, POLL_MS);
  }

  /** Answers whatever is waiting. Public so a test need not wait on a timer. */
  async sweep(): Promise<void> {
    let names: string[];
    try {
      names = [...Deno.readDirSync(this.directory)]
        .filter((entry) => entry.isFile && entry.name.endsWith(".request"))
        .map((entry) => entry.name)
        .sort();
    } catch {
      // No directory yet, which is every session that has not delegated.
      return;
    }

    for (const name of names) {
      if (this.stopped) return;
      await this.answer(name);
    }
  }

  private async answer(name: string): Promise<void> {
    const id = name.slice(0, -".request".length);
    const path = join(this.directory, name);

    let raw: unknown;
    try {
      raw = JSON.parse(Deno.readTextFileSync(path));
    } catch (error) {
      this.write(id, "refused", "that delegation could not be read as a request");
      this.log("a delegation request could not be read", error);
      this.discard(path);
      return;
    }
    // Removed before it is run, so a request cannot be answered twice if
    // running it takes longer than the next sweep.
    this.discard(path);

    const asked = typeof (raw as { question?: unknown }).question === "string"
      ? (raw as { question: string }).question
      : "";

    const delegations = this.options.delegations();
    if (delegations === undefined) {
      const refused = "there is no turn running to delegate from";
      this.options.report({ asked, refused });
      this.write(id, "refused", refused);
      return;
    }

    const outcome = await delegations.run(raw);
    this.options.report({ asked, ...outcome });

    if (isRefused(outcome)) {
      this.write(id, "refused", `${outcome.refused}; carry on yourself`);
      return;
    }
    this.write(id, "answer", labelled(outcome));
  }

  /**
   * Writes an answer where the agent is waiting for it.
   *
   * Under a temporary name and then renamed, so the agent cannot read half of
   * one and treat it as the whole answer.
   */
  private write(id: string, kind: "answer" | "refused", body: string): void {
    const target = join(this.directory, `${id}.${kind}`);
    const writing = `${target}.writing`;
    try {
      Deno.writeTextFileSync(writing, body.endsWith("\n") ? body : `${body}\n`, { mode: 0o600 });
      Deno.renameSync(writing, target);
    } catch (error) {
      this.log("a delegation answer could not be written", error);
    }
  }

  private discard(path: string): void {
    try {
      Deno.removeSync(path);
    } catch {
      // Already gone, which is the state we wanted.
    }
  }

  private log(message: string, error: unknown): void {
    this.options.log.warn(message, { detail: String(error) });
  }
}
