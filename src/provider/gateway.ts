/**
 * The usage window of a proxy gateway that reports one beside its API.
 *
 * A gateway of this kind serves `/usage` next to `/chat/completions`, under
 * the same base URL and the same key, and answers with the one constraint that
 * decides whether work can start. That is the whole of what is read here: the
 * other windows it lists are for a person looking, not for a decision.
 *
 * Told apart from the provider's own metering by where it is asked. This is
 * the base URL the operator configured, so a provider that does not serve it
 * simply does not answer and the daemon carries on.
 */

import type { Fetch, Quota } from "./usage.ts";

/** Where a gateway reports what is left, relative to its base URL. */
export const USAGE_PATH = "/usage";

/**
 * The value of `usage` on a provider definition that opts in.
 *
 * Named rather than a boolean, so a second shape can be added later without
 * the configuration having to mean two things by `true`.
 */
export const GATEWAY_USAGE = "gateway";

/**
 * Reads the window out of a gateway's answer.
 *
 * Only `limiting` is read, which the gateway defines as the one constraint
 * that decides whether work can start. A status other than `ok` means the
 * gateway is not sure, and an unsure answer must not read as a spent window:
 * that would stop every session on this host until somebody noticed.
 *
 * @returns undefined for anything unrecognised rather than a guess.
 */
export function readGatewayUsage(body: unknown): Quota | undefined {
  if (typeof body !== "object" || body === null) return undefined;
  const limiting = (body as { limiting?: unknown }).limiting;
  if (typeof limiting !== "object" || limiting === null) return undefined;

  const entry = limiting as {
    status?: unknown;
    spent?: unknown;
    remainingPercent?: unknown;
    peakUsedPercent?: unknown;
    resetsAt?: unknown;
  };
  if (entry.status !== "ok") return undefined;

  const resetsAt = typeof entry.resetsAt === "string" ? Date.parse(entry.resetsAt) : Number.NaN;
  if (!Number.isFinite(resetsAt)) return undefined;

  // The gateway says outright whether the window is spent, and that is worth
  // more than a percentage: it is the answer the gateway would give its own
  // rate limiter. A spent window is reported as full so that everything
  // downstream, which asks only about the percentage, agrees with it.
  if (entry.spent === true) return { percentage: 100, resetsAt };

  const used = typeof entry.peakUsedPercent === "number"
    ? entry.peakUsedPercent
    : typeof entry.remainingPercent === "number"
    ? 100 - entry.remainingPercent
    : undefined;
  if (used === undefined) return undefined;

  // Not spent, whatever the arithmetic says, because the gateway already said
  // so and rounding must not close a window it left open.
  return { percentage: Math.min(Math.max(used, 0), 99.9), resetsAt };
}

/**
 * Asks a gateway what is left.
 *
 * @returns undefined when the answer cannot be had, which callers must treat
 *   as "carry on". A gateway that is unreachable, slow, or has changed its
 *   response must not become a reason to refuse work.
 */
export async function fetchGatewayUsage(
  baseUrl: string,
  key: string,
  fetchImpl: Fetch = (url, init) => fetch(url, init),
  timeoutMs = 10_000,
): Promise<Quota | undefined> {
  try {
    const response = await fetchImpl(`${baseUrl.replace(/\/$/, "")}${USAGE_PATH}`, {
      headers: { Authorization: `Bearer ${key}`, Accept: "application/json" },
      signal: AbortSignal.timeout(timeoutMs),
    });
    if (!response.ok) return undefined;
    return readGatewayUsage(await response.json());
  } catch {
    return undefined;
  }
}
