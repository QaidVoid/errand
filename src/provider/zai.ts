/**
 * The z.ai usage window, so a session is not started against a spent quota.
 *
 * z.ai meters tokens in a rolling five hour window. Past it every request is
 * refused, and without this the refusal arrives as a failed turn: the thread
 * has already opened, the sandbox has already started, and the person is told
 * something went wrong rather than when to come back.
 *
 * Specific to this provider: the endpoint, the field names, and the five hour
 * window are z.ai's. What is shared with any other metered provider lives in
 * the module beside this one.
 */

import type { Fetch, Quota } from "./usage.ts";

/** Where z.ai reports what is left of a quota. */
export const QUOTA_URL = "https://bigmodel.cn/api/monitor/usage/quota/limit";

/** The rolling token window, as opposed to the monthly tool-call allowance. */
const TOKENS_LIMIT = "TOKENS_LIMIT";

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
    if (typeof entry.percentage !== "number") return undefined;
    // A window nothing has been charged to has nothing scheduled to reset, and
    // z.ai sends null for it. That is an answer: the window is empty, which is
    // the most useful thing it could say.
    return typeof entry.nextResetTime === "number"
      ? { percentage: entry.percentage, resetsAt: entry.nextResetTime }
      : { percentage: entry.percentage };
  }
  return undefined;
}

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
