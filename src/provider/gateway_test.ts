import { assertEquals } from "@std/assert";
import { fetchGatewayUsage, readGatewayUsage, USAGE_PATH } from "./gateway.ts";

/** The shape the gateway actually answers with, trimmed to what is read. */
function usageBody(limiting: Record<string, unknown>): unknown {
  return { provider: "muse-code", scope: "key", limiting, windows: [], key: {} };
}

const OPEN = {
  status: "ok",
  id: "window-share:300m",
  label: "Share of the 5 hours window",
  peakUsedPercent: 12,
  lastUsedPercent: 3,
  remainingPercent: 88,
  spent: false,
  resetsAt: "2026-09-16T08:39:43.000Z",
};

Deno.test("an open window is read as what has been used of it", () => {
  const quota = readGatewayUsage(usageBody(OPEN));
  assertEquals(quota?.percentage, 12);
  assertEquals(quota?.resetsAt, Date.parse("2026-09-16T08:39:43.000Z"));
});

/** The gateway says outright, and that beats the arithmetic. */
Deno.test("a spent window is spent whatever the percentages say", () => {
  const quota = readGatewayUsage(
    usageBody({ ...OPEN, spent: true, peakUsedPercent: 97, remainingPercent: 3 }),
  );
  assertEquals(quota?.percentage, 100);
});

/** Rounding must not close a window the gateway left open. */
Deno.test("an unspent window never reads as full", () => {
  const quota = readGatewayUsage(
    usageBody({ ...OPEN, spent: false, peakUsedPercent: 100, remainingPercent: 0 }),
  );
  assertEquals(quota !== undefined && quota.percentage < 100, true);
});

/**
 * An unsure answer must not read as a spent window: that would stop every
 * session on this host until somebody noticed.
 */
Deno.test("an answer the gateway is unsure of is no answer", () => {
  assertEquals(readGatewayUsage(usageBody({ ...OPEN, status: "stale" })), undefined);
  assertEquals(readGatewayUsage(usageBody({ ...OPEN, status: "unknown" })), undefined);
  assertEquals(readGatewayUsage(usageBody({ ...OPEN, resetsAt: null })), undefined);
  assertEquals(readGatewayUsage(usageBody({ ...OPEN, resetsAt: "not a time" })), undefined);
  assertEquals(readGatewayUsage({}), undefined);
  assertEquals(readGatewayUsage(null), undefined);
  // Neither percentage present, so there is nothing to report.
  assertEquals(
    readGatewayUsage(usageBody({ status: "ok", spent: false, resetsAt: OPEN.resetsAt })),
    undefined,
  );
});

Deno.test("usage is asked for beside the base url, with the key", () => {
  let asked = "";
  let sent = "";
  const quota = fetchGatewayUsage("https://gateway.example/v1/", "the-key", (url, init) => {
    asked = url;
    sent = String(new Headers(init.headers).get("authorization"));
    return Promise.resolve(
      new Response(JSON.stringify(usageBody(OPEN)), {
        headers: { "content-type": "application/json" },
      }),
    );
  });
  return quota.then((window) => {
    // One slash, whether or not the base url ended in one.
    assertEquals(asked, `https://gateway.example/v1${USAGE_PATH}`);
    assertEquals(sent, "Bearer the-key");
    assertEquals(window?.percentage, 12);
  });
});

/** A gateway that cannot be reached must leave work running. */
Deno.test("a gateway that does not answer is not a spent window", async () => {
  assertEquals(
    await fetchGatewayUsage(
      "https://gateway.example/v1",
      "k",
      () => Promise.reject(new Error("x")),
    ),
    undefined,
  );
  assertEquals(
    await fetchGatewayUsage(
      "https://gateway.example/v1",
      "k",
      () => Promise.resolve(new Response("nope", { status: 503 })),
    ),
    undefined,
  );
});
