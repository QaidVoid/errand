/**
 * Running the delegations of one turn.
 *
 * Held per turn rather than per session, so the count of what a turn has spent
 * lives and dies with the turn and there is no table to clean up.
 *
 * Every refusal here is ordinary. A delegation that cannot run leaves the work
 * with the session's own model, which is slower and dearer and correct.
 */

import type { Scheduler } from "../admission/scheduler.ts";
import { type Answer as Given, ask, AskFailed, type Endpoint, type Send } from "../provider/ask.ts";
import {
  type Delegation,
  isRefused,
  parseDelegation,
  type Refused,
  resolveSource,
  type Sources,
} from "./delegation.ts";

/** An answer, and what it is an answer about. */
export interface Answer {
  /** What the model said. */
  text: string;
  /** The model that said it, so the answer can be attributed. */
  model: string;
  /** What it was shown, so the answer can say what it describes. */
  describes: string;
  /** Tokens the delegated model was charged, when the provider said. */
  tokens: number | undefined;
  /** Characters kept out of the session's context by asking instead of reading. */
  keptOut: number;
}

/** What a turn's delegations need in order to run. */
export interface Options {
  sessionId: string;
  endpoint: Endpoint;
  scheduler: Scheduler;
  sources: Sources;
  /** How long one delegation may take before it is abandoned. */
  deadlineMs: number;
  /** How many delegations one turn may make. */
  perTurn: number;
  send?: Send;
}

/** The delegations of one turn. */
export class TurnDelegations {
  private used = 0;

  constructor(private readonly options: Options) {}

  /** How many delegations this turn has left. */
  get remaining(): number {
    return Math.max(0, this.options.perTurn - this.used);
  }

  /**
   * Runs one delegation, or says why it did not.
   *
   * A refusal is returned rather than thrown: every caller continues either
   * way, and an exception would invite one of them not to.
   */
  async run(raw: unknown): Promise<Answer | Refused> {
    const { scheduler, sessionId, endpoint, sources, deadlineMs } = this.options;

    if (this.remaining === 0) {
      return { refused: `this turn has already delegated ${this.options.perTurn} times` };
    }

    const delegation = parseDelegation(raw);
    if (isRefused(delegation)) return delegation;

    const resolved = await resolveSource(delegation as Delegation, sources);
    if (isRefused(resolved)) return resolved;

    // Counted once it is going to be sent, so a malformed request does not
    // spend the turn's allowance.
    this.used += 1;

    const ticket = scheduler.tryAdmit(sessionId);
    if (ticket === null) {
      return {
        refused: scheduler.pausedBecause === null
          ? "there was no free slot to ask a second model"
          : "the provider is being backed off, so nothing was asked of it",
      };
    }

    const timeout = AbortSignal.timeout(deadlineMs);
    let given: Given;
    try {
      given = await ask(endpoint, resolved, timeout, this.options.send);
    } catch (error) {
      return { refused: error instanceof AskFailed ? error.message : String(error) };
    } finally {
      scheduler.release(ticket);
    }

    return {
      text: given.text,
      model: endpoint.model,
      describes: resolved.describes,
      tokens: given.tokens,
      keptOut: resolved.content.length,
    };
  }
}
