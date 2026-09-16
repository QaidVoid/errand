/**
 * What a provider says is left to spend, whoever the provider is.
 *
 * A metered provider refuses work once its window is spent, and without asking
 * first the refusal arrives as a failed turn: the thread has already opened,
 * the sandbox has already started, and the person is told something went wrong
 * rather than when to come back.
 *
 * Each provider answers this in its own shape and at its own endpoint, so the
 * reading is per provider and lives beside it. What is shared is the window
 * itself, how long an answer is worth holding, and what a spent one is called.
 */

/** What the provider says about the window a prompt would be charged to. */
export interface Quota {
  /** How much of the window is spent, 0 to 100. */
  percentage: number;
  /**
   * When the window rolls over, in epoch milliseconds, where that is known.
   *
   * A window nothing has been charged to yet has nothing scheduled to reset,
   * and a provider that says so is answering rather than failing. Treating the
   * missing time as no answer would clear the status exactly when the window
   * is emptiest.
   */
  resetsAt?: number | undefined;
}

/** True when the window is spent and a prompt would be refused. */
export function isSpent(quota: Quota): boolean {
  return quota.percentage >= 100;
}

/** How long an unspent answer is reused before the provider is asked again. */
export const QUOTA_TTL_MS = 60_000;

/** Fetches a URL. Injected so tests need no network. */
export type Fetch = (url: string, init: RequestInit) => Promise<Response>;

/** Asks one provider what is left. Undefined means the answer cannot be had. */
export type ReadQuota = () => Promise<Quota | undefined>;

/**
 * Holds the last answer so the provider is not asked once per message.
 *
 * A spent window is not asked about again until it rolls over, because the
 * answer cannot change before then. An unspent one is asked about on a short
 * interval, since the only way it changes is by being used.
 *
 * The reading is given rather than built here: the gate is about when to ask,
 * and every provider answers differently.
 */
export class QuotaGate {
  private held: Quota | undefined;
  private heldAt = 0;

  constructor(
    private readonly read: ReadQuota,
    private readonly now: () => number = Date.now,
  ) {}

  /**
   * What the window looks like, or undefined when that cannot be established.
   *
   * Undefined means carry on. It is returned for an unreachable provider as
   * well as for an unrecognised answer, and both must leave work running.
   */
  async current(): Promise<Quota | undefined> {
    const at = this.now();
    if (this.held !== undefined) {
      // A spent window cannot change before it rolls over, so it is held
      // until then. Without a time to wait for, it is held like any other.
      const until = this.held.resetsAt;
      if (isSpent(this.held) && until !== undefined && at < until) return this.held;
      if (!isSpent(this.held) && at - this.heldAt < QUOTA_TTL_MS) return this.held;
    }

    const fresh = await this.read();
    if (fresh === undefined) return undefined;
    this.held = fresh;
    this.heldAt = at;
    return fresh;
  }

  /** Forgets what was held, so the next question reaches the provider. */
  forget(): void {
    this.held = undefined;
    this.heldAt = 0;
  }
}

/** One provider whose window this host can ask about. */
export interface UsageSource {
  /** The provider name, as the configuration and `--model` call it. */
  provider: string;
  gate: QuotaGate;
}

/** What a thread is told when a window is spent. */
export function spentMessage(provider: string, relative: string | undefined): string {
  const back = relative === undefined ? "" : `; it resets ${relative}`;
  return `${provider}'s usage window is spent, so this cannot run yet${back}`;
}

/** What is said when a provider was asked and did not answer usefully. */
export const UNKNOWN_QUOTA = "the model provider did not say what is left of the usage window";

/**
 * What the window looks like, in a line somebody asked for on purpose.
 *
 * Says what is left rather than what is spent. "58% left" is the number
 * somebody is deciding on, where "42% used" has to be subtracted first.
 */
export function quotaMessage(
  provider: string,
  quota: Quota,
  relative: string | undefined,
): string {
  const left = Math.max(0, Math.round(100 - quota.percentage));
  const state = isSpent(quota)
    ? `${provider}'s usage window is spent`
    : `${left}% of ${provider}'s usage window is left`;
  return relative === undefined ? state : `${state}, and it resets ${relative}`;
}

/**
 * The window as a bot status, which has far less room than a message.
 *
 * Short enough to survive Discord's status limit whole, and carrying the two
 * things somebody glancing at the member list wants: how much is left, and
 * when it comes back. The time is passed in already rendered, as above.
 *
 * Each metered provider renders its own line and the caller joins them, so a
 * host with more than one says so without this knowing how many there are.
 */
export function quotaStatus(quota: Quota, relative: string | undefined): string {
  const left = Math.max(0, Math.round(100 - quota.percentage));
  if (isSpent(quota)) {
    return relative === undefined ? "usage spent" : `usage spent, back ${relative}`;
  }
  return relative === undefined ? `${left}% usage left` : `${left}% usage left, resets ${relative}`;
}
