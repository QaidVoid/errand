import { assertEquals, assertStringIncludes } from "@std/assert";
import {
  fetchQuota,
  isSpent,
  metersUsage,
  QUOTA_TTL_MS,
  QUOTA_URL,
  QuotaGate,
  quotaMessage,
  quotaStatus,
  readQuota,
  spentMessage,
} from "./zai.ts";

function answer(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

function quotaBody(percentage: number, resetsAt: number): unknown {
  return {
    data: {
      limits: [
        { type: "TOOL_CALL_LIMIT", percentage: 3, nextResetTime: 1 },
        { type: "TOKENS_LIMIT", percentage, nextResetTime: resetsAt },
      ],
    },
  };
}

Deno.test("the token window is read out of the answer, not the tool one", () => {
  assertEquals(readQuota(quotaBody(42, 1_700_000)), { percentage: 42, resetsAt: 1_700_000 });
});

/**
 * A shape that changed must not read as a spent quota: that would stop every
 * session on the host until somebody noticed.
 */
Deno.test("anything unrecognised is read as nothing rather than as spent", () => {
  for (const body of [null, {}, { data: {} }, { data: { limits: "no" } }, "nope"]) {
    assertEquals(readQuota(body), undefined, JSON.stringify(body));
  }
  assertEquals(
    readQuota({ data: { limits: [{ type: "TOKENS_LIMIT", percentage: "42" }] } }),
    undefined,
  );
  assertEquals(
    readQuota({ data: { limits: [{ type: "OTHER", percentage: 1, nextResetTime: 2 }] } }),
    undefined,
  );
});

Deno.test("a window is spent only once it is all the way spent", () => {
  assertEquals(isSpent({ percentage: 99.9, resetsAt: 0 }), false);
  assertEquals(isSpent({ percentage: 100, resetsAt: 0 }), true);
  assertEquals(isSpent({ percentage: 140, resetsAt: 0 }), true);
});

Deno.test("only the provider that meters this way is asked", () => {
  assertEquals(metersUsage("zai"), true);
  assertEquals(metersUsage("zai-coding-cn"), true);
  assertEquals(metersUsage("anthropic"), false);
});

/** The key goes raw, not as a bearer token: that is what the endpoint takes. */
Deno.test("the quota is asked for with the key as it is", async () => {
  let seen: { url: string; auth: string | null } | undefined;

  await fetchQuota("secret-key", (url, init) => {
    seen = { url, auth: new Headers(init.headers).get("authorization") };
    return Promise.resolve(answer(quotaBody(10, 5)));
  });

  assertEquals(seen?.url, QUOTA_URL);
  assertEquals(seen?.auth, "secret-key");
});

/**
 * An unreachable provider must leave work running: refusing everything is a
 * far worse way to be wrong than one failed turn.
 */
Deno.test("a provider that cannot be reached says nothing, not no", async () => {
  assertEquals(await fetchQuota("k", () => Promise.reject(new Error("offline"))), undefined);
  assertEquals(await fetchQuota("k", () => Promise.resolve(answer({}, 500))), undefined);
  assertEquals(await fetchQuota("k", () => Promise.resolve(new Response("not json"))), undefined);
});

Deno.test("an unspent window is reused briefly, then asked about again", async () => {
  let asked = 0;
  let now = 1_000;
  const gate = new QuotaGate("k", () => {
    asked += 1;
    return Promise.resolve(answer(quotaBody(20, 9_999_999)));
  }, () => now);

  await gate.current();
  await gate.current();
  assertEquals(asked, 1);

  now += QUOTA_TTL_MS + 1;
  await gate.current();
  assertEquals(asked, 2);
});

/** Nothing can change before it rolls over, so there is nothing to ask. */
Deno.test("a spent window is not asked about again until it resets", async () => {
  let asked = 0;
  let now = 1_000;
  const gate = new QuotaGate("k", () => {
    asked += 1;
    return Promise.resolve(answer(quotaBody(100, 500_000)));
  }, () => now);

  await gate.current();
  now += 400_000;
  await gate.current();
  assertEquals(asked, 1);

  now = 500_001;
  await gate.current();
  assertEquals(asked, 2);
});

Deno.test("an answer that could not be had is not held on to", async () => {
  let asked = 0;
  const gate = new QuotaGate("k", () => {
    asked += 1;
    return Promise.reject(new Error("offline"));
  });

  assertEquals(await gate.current(), undefined);
  assertEquals(await gate.current(), undefined);
  assertEquals(asked, 2);
});

Deno.test("forgetting makes the next question reach the provider", async () => {
  let asked = 0;
  const gate = new QuotaGate("k", () => {
    asked += 1;
    return Promise.resolve(answer(quotaBody(20, 9_999_999)));
  });

  await gate.current();
  gate.forget();
  await gate.current();

  assertEquals(asked, 2);
});

/** The number somebody is deciding on is what is left, not what is spent. */
Deno.test("the usage line says what is left and when it comes back", () => {
  const line = quotaMessage({ percentage: 42.4, resetsAt: 0 }, "in 2 hours");

  assertStringIncludes(line, "58% of the provider's usage window is left");
  assertStringIncludes(line, "resets in 2 hours");
});

Deno.test("a spent window says so rather than saying zero percent is left", () => {
  const line = quotaMessage({ percentage: 100, resetsAt: 0 }, "in 10 minutes");

  assertStringIncludes(line, "is spent");
  assertStringIncludes(line, "in 10 minutes");
});

Deno.test("a refusal says when to come back", () => {
  assertStringIncludes(spentMessage("in 3 hours"), "resets in 3 hours");
});

Deno.test("the status says what is left, and says spent without a percentage", () => {
  assertEquals(
    quotaStatus({ percentage: 42, resetsAt: 0 }, "in 2h"),
    "58% usage left, resets in 2h",
  );
  assertEquals(
    quotaStatus({ percentage: 100, resetsAt: 0 }, "in 30m"),
    "usage spent, back in 30m",
  );
});

Deno.test("a percentage past the end does not become a negative remainder", () => {
  // The provider has reported over 100 before; the status must not read
  // "-4% usage left".
  assertEquals(
    quotaStatus({ percentage: 104, resetsAt: 0 }, "in 1h"),
    "usage spent, back in 1h",
  );
});
