/**
 * The z.ai usage window, so a session is not started against a spent quota.
 *
 * z.ai meters tokens in a rolling five hour window. Past it every request is
 * refused, and without this the refusal arrives as a failed turn: the thread
 * has already opened, the sandbox has already started, and the person is told
 * something went wrong rather than when to come back.
 *
 * Deliberately specific to one provider. The endpoint, the field names, and
 * the five hour window are z.ai's, and nothing else here is metered this way,
 * so there is no second implementation to generalise for.
 */

/** Where z.ai reports what is left of a quota. */
export const QUOTA_URL = "https://bigmodel.cn/api/monitor/usage/quota/limit";

/** The rolling token window, as opposed to the monthly tool-call allowance. */
const TOKENS_LIMIT = "TOKENS_LIMIT";

/** What the provider says about the window a prompt would be charged to. */
export interface Quota {
  /** How much of the window is spent, 0 to 100. */
  percentage: number;
  /** When the window rolls over, in epoch milliseconds. */
  resetsAt: number;
}

/**
 * Reads the token window out of a quota response.
 *
 * @returns undefined for anything unrecognised rather than a guess. A shape
 *   that changed must not read as a spent quota, because that would stop every
 *   session on this host until somebody noticed.
 */
export function readQuota(body: unknown): Quota | undefined {
  if (typeof body !== "object" || body === null) return undefined;
  const data = (body as { data?: unknown }).data;
  if (typeof data !== "object" || data === null) return undefined;
  const limits = (data as { limits?: unknown }).limits;
  if (!Array.isArray(limits)) return undefined;

  for (const limit of limits) {
    if (typeof limit !== "object" || limit === null) continue;
    const entry = limit as { type?: unknown; percentage?: unknown; nextResetTime?: unknown };
    if (entry.type !== TOKENS_LIMIT) continue;
    if (typeof entry.percentage !== "number" || typeof entry.nextResetTime !== "number") {
      return undefined;
    }
    return { percentage: entry.percentage, resetsAt: entry.nextResetTime };
  }
  return undefined;
}

/** True when the window is spent and a prompt would be refused. */
export function isSpent(quota: Quota): boolean {
  return quota.percentage >= 100;
}

/** Fetches a URL. Injected so tests need no network. */
export type Fetch = (url: string, init: RequestInit) => Promise<Response>;

/**
 * Asks z.ai what is left of the window.
 *
 * @returns undefined when the answer cannot be had, which callers must treat
 *   as "carry on". A provider that is unreachable, slow, or has changed its
 *   response must not become a reason to refuse work: the cost of guessing
 *   wrong that way is every session refused, against one failed turn for
 *   guessing wrong the other way.
 */
export async function fetchQuota(
  key: string,
  fetchImpl: Fetch = (url, init) => fetch(url, init),
  timeoutMs = 10_000,
): Promise<Quota | undefined> {
  try {
    const response = await fetchImpl(QUOTA_URL, {
      // Raw, not a bearer token. This is what the endpoint accepts.
      headers: { Authorization: key, Accept: "application/json" },
      signal: AbortSignal.timeout(timeoutMs),
    });
    if (!response.ok) return undefined;
    return readQuota(await response.json());
  } catch {
    return undefined;
  }
}

/** True for a provider metered by the endpoint above. */
export function metersUsage(provider: string): boolean {
  return provider.startsWith("zai");
}

/** How long an unspent answer is reused before the provider is asked again. */
export const QUOTA_TTL_MS = 60_000;

/**
 * Holds the last answer so the provider is not asked once per message.
 *
 * A spent window is not asked about again until it rolls over, because the
 * answer cannot change before then. An unspent one is asked about on a short
 * interval, since the only way it changes is by being used.
 */
export class QuotaGate {
  private held: Quota | undefined;
  private heldAt = 0;

  constructor(
    private readonly key: string,
    private readonly fetchImpl?: Fetch,
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
      if (isSpent(this.held) && at < this.held.resetsAt) return this.held;
      if (!isSpent(this.held) && at - this.heldAt < QUOTA_TTL_MS) return this.held;
    }

    const fresh = await fetchQuota(this.key, this.fetchImpl);
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

/**
 * What a thread is told when the window is spent.
 *
 * The time is passed in already rendered, so this file stays free of anything
 * chat-shaped and can be read without knowing that surface.
 */
export function spentMessage(relative: string): string {
  return `the model provider's usage window is spent, so this cannot run yet; it resets ${relative}`;
}

/**
 * What the window looks like, in a line somebody asked for on purpose.
 *
 * Says what is left rather than what is spent. "58% left" is the number
 * somebody is deciding on, where "42% used" has to be subtracted first.
 */
export function quotaMessage(quota: Quota, relative: string): string {
  const left = Math.max(0, Math.round(100 - quota.percentage));
  const state = isSpent(quota)
    ? "the provider's usage window is spent"
    : `${left}% of the provider's usage window is left`;
  return `${state}, and it resets ${relative}`;
}

/** What to say when the provider will not say, which is not an error. */
export const UNKNOWN_QUOTA = "the model provider did not say what is left of the usage window";
