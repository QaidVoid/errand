import { assertEquals, assertStringIncludes } from "@std/assert";
import { Scheduler } from "../admission/scheduler.ts";
import type { LimitsConfig } from "../config/schema.ts";
import { isRefused } from "./delegation.ts";
import { TurnDelegations } from "./delegate.ts";

const LIMITS: LimitsConfig = {
  maxConcurrentTurns: 2,
  maxLiveSessions: 3,
  maxQueueLength: 3,
  maxQueueWaitMs: 1_000,
};

const ENDPOINT = {
  baseUrl: "https://provider.test/v4",
  model: "cheap-model",
  credential: "secret",
};

function sources() {
  return {
    projectRoot: "/projects/demo",
    readFile: () => Promise.resolve("line one\nline two failed\nline three"),
    outputOf: (id: string) => (id === "c1" ? "3 failed" : undefined),
    attachment: () => undefined,
  };
}

/** Records what was sent, and answers with whatever the test wants. */
function sender(answer: unknown, status = 200) {
  const sent: { url: string; body: Record<string, unknown> }[] = [];
  const send = (url: string, init: RequestInit) => {
    sent.push({ url, body: JSON.parse(String(init.body)) });
    return Promise.resolve(
      new Response(JSON.stringify(answer), {
        status,
        headers: { "content-type": "application/json" },
      }),
    );
  };
  return { sent, send };
}

const REPLY = {
  choices: [{ message: { content: "  line two failed  " } }],
  usage: { total_tokens: 412 },
};

function delegations(overrides: Record<string, unknown> = {}) {
  const scheduler = new Scheduler(LIMITS);
  const { sent, send } = sender(REPLY);
  const turn = new TurnDelegations({
    sessionId: "s-1",
    endpoint: ENDPOINT,
    scheduler,
    sources: sources(),
    deadlineMs: 5_000,
    perTurn: 2,
    send,
    ...overrides,
  });
  return { turn, scheduler, sent };
}

Deno.test("a delegation is asked of the cheap model and answered", async () => {
  const { turn, sent } = delegations();

  const answer = await turn.run({ question: "which test failed?", path: "out.log" });

  assertEquals(isRefused(answer), false);
  if (isRefused(answer)) return;
  assertEquals(answer.text, "line two failed");
  assertEquals(answer.model, "cheap-model");
  assertEquals(answer.describes, "out.log");
  assertEquals(answer.tokens, 412);
  assertEquals(answer.keptOut, "line one\nline two failed\nline three".length);
  assertEquals(sent[0]?.url, "https://provider.test/v4/chat/completions");
});

/** The delegated model is shown one artefact and told nothing else. */
Deno.test("the request carries the question and the content, and no tools", async () => {
  const { turn, sent } = delegations();
  await turn.run({ question: "which test failed?", path: "out.log" });

  const body = sent[0]?.body ?? {};
  assertEquals(Object.keys(body).sort(), ["messages", "model"]);
  assertEquals((body.messages as unknown[]).length, 1);

  const text = JSON.stringify(body.messages);
  assertStringIncludes(text, "which test failed?");
  assertStringIncludes(text, "line two failed");
  assertEquals(text.includes("tool"), false);
});

Deno.test("a turn may delegate only so many times", async () => {
  const { turn } = delegations({ perTurn: 1 });

  assertEquals(isRefused(await turn.run({ question: "q", path: "a.log" })), false);
  const second = await turn.run({ question: "q", path: "b.log" });

  assertEquals(isRefused(second), true);
  assertEquals(isRefused(second) ? second.refused.includes("already delegated") : false, true);
});

/** A malformed request is a mistake, not a spent allowance. */
Deno.test("a refused request does not spend the turn's allowance", async () => {
  const { turn } = delegations({ perTurn: 1 });

  await turn.run({ question: "nothing to look at" });
  assertEquals(turn.remaining, 1);

  assertEquals(isRefused(await turn.run({ question: "q", path: "a.log" })), false);
  assertEquals(turn.remaining, 0);
});

Deno.test("nothing is asked while the provider is being backed off", async () => {
  const { turn, scheduler, sent } = delegations();
  scheduler.noteRateLimit();

  const answer = await turn.run({ question: "q", path: "a.log" });

  assertEquals(isRefused(answer) ? answer.refused.includes("backed off") : false, true);
  assertEquals(sent.length, 0);
});

Deno.test("no free slot means the work stays with the session's own model", async () => {
  const { turn, scheduler, sent } = delegations();
  scheduler.tryAdmit("other");
  scheduler.tryAdmit("other");

  const answer = await turn.run({ question: "q", path: "a.log" });

  assertEquals(isRefused(answer) ? answer.refused.includes("no free slot") : false, true);
  assertEquals(sent.length, 0);
});

Deno.test("the slot is given back, whether the model answered or not", async () => {
  const { turn, scheduler } = delegations();
  await turn.run({ question: "q", path: "a.log" });
  assertEquals(scheduler.turnsInFlight, 0);

  const failing = delegations({ send: () => Promise.reject(new Error("network is down")) });
  await failing.turn.run({ question: "q", path: "a.log" });
  assertEquals(failing.scheduler.turnsInFlight, 0);
});

Deno.test("a provider refusal is reported, not thrown", async () => {
  const { send } = sender({ error: { message: "rate limited" } }, 429);
  const { turn } = delegations({ send });

  const answer = await turn.run({ question: "q", path: "a.log" });

  assertEquals(isRefused(answer) ? answer.refused.includes("refused the question") : false, true);
});

Deno.test("an empty answer is treated as no answer", async () => {
  const { send } = sender({ choices: [{ message: { content: "   " } }] });
  const { turn } = delegations({ send });

  const answer = await turn.run({ question: "q", path: "a.log" });

  assertEquals(isRefused(answer) ? answer.refused.includes("no answer") : false, true);
});

Deno.test("a delegation that never answers is abandoned rather than awaited", async () => {
  const { turn, scheduler } = delegations({
    deadlineMs: 20,
    send: (_url: string, init: RequestInit) =>
      new Promise<Response>((_resolve, reject) => {
        init.signal?.addEventListener("abort", () => reject(new Error("aborted")));
      }),
  });

  const answer = await turn.run({ question: "q", path: "a.log" });

  assertEquals(isRefused(answer) ? answer.refused.includes("in time") : false, true);
  assertEquals(scheduler.turnsInFlight, 0);
});
