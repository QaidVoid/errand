import { assertEquals } from "@std/assert";
import type { LimitsConfig } from "../config/schema.ts";
import { type Clock, Scheduler, type Ticket } from "./scheduler.ts";

const LIMITS: LimitsConfig = {
  maxConcurrentTurns: 2,
  maxLiveSessions: 3,
  maxQueueLength: 3,
  maxQueueWaitMs: 1_000,
};

/** A clock a test moves by hand, so nothing waits for real time. */
function testClock(): Clock & { advance(ms: number): void } {
  let now = 1_000_000;
  let next = 1;
  const timers = new Map<number, { at: number; run: () => void }>();

  return {
    now: () => now,
    setTimeout(handler: () => void, ms: number) {
      const handle = next;
      next += 1;
      timers.set(handle, { at: now + ms, run: handler });
      return handle;
    },
    clearTimeout(handle: unknown) {
      timers.delete(handle as number);
    },
    advance(ms: number) {
      now += ms;
      for (const [handle, timer] of [...timers]) {
        if (timer.at > now) continue;
        timers.delete(handle);
        timer.run();
      }
    },
  };
}

function entry(sessionId: string, seen: string[]) {
  const admitted: Ticket[] = [];
  return {
    admitted,
    entry: {
      sessionId,
      onAdmitted: (ticket: Ticket) => {
        admitted.push(ticket);
        seen.push(`admitted:${sessionId}`);
      },
      onExpired: () => seen.push(`expired:${sessionId}`),
      onPositionChanged: (position: number) => seen.push(`position:${sessionId}:${position}`),
    },
  };
}

Deno.test("admits up to the cap and queues the rest", () => {
  const scheduler = new Scheduler(LIMITS, testClock());
  const seen: string[] = [];

  assertEquals(scheduler.submit(entry("a", seen).entry).status, "admitted");
  assertEquals(scheduler.submit(entry("b", seen).entry).status, "admitted");

  const third = scheduler.submit(entry("c", seen).entry);
  assertEquals(third.status, "queued");
  assertEquals(third.status === "queued" ? third.position : 0, 1);
  assertEquals(scheduler.turnsInFlight, 2);
});

Deno.test("a full queue refuses rather than growing without bound", () => {
  const scheduler = new Scheduler(LIMITS, testClock());
  const seen: string[] = [];
  for (let i = 0; i < 2 + LIMITS.maxQueueLength; i += 1) {
    scheduler.submit(entry(`s${i}`, seen).entry);
  }

  const refused = scheduler.submit(entry("over", seen).entry);
  assertEquals(refused.status, "rejected");
  assertEquals(
    refused.status === "rejected" ? refused.reason.includes("queue is full") : false,
    true,
  );
});

Deno.test("releasing a slot admits the next in line", () => {
  const scheduler = new Scheduler(LIMITS, testClock());
  const seen: string[] = [];
  const first = scheduler.submit(entry("a", seen).entry);
  scheduler.submit(entry("b", seen).entry);
  scheduler.submit(entry("c", seen).entry);

  if (first.status !== "admitted") throw new Error("expected the first to be admitted");
  assertEquals(scheduler.release(first.ticket), true);

  assertEquals(seen.includes("admitted:c"), true);
  assertEquals(scheduler.queueLength, 0);
});

/** A slot released twice would let the cap drift upward without bound. */
Deno.test("a ticket released twice is refused the second time", () => {
  const scheduler = new Scheduler(LIMITS, testClock());
  const outcome = scheduler.submit(entry("a", []).entry);
  if (outcome.status !== "admitted") throw new Error("expected admission");

  assertEquals(scheduler.release(outcome.ticket), true);
  assertEquals(scheduler.release(outcome.ticket), false);
  assertEquals(scheduler.turnsInFlight, 0);
});

Deno.test("a session that ends takes its queued prompts with it", () => {
  const scheduler = new Scheduler(LIMITS, testClock());
  const seen: string[] = [];
  scheduler.submit(entry("a", seen).entry);
  scheduler.submit(entry("b", seen).entry);
  scheduler.submit(entry("gone", seen).entry);
  scheduler.submit(entry("stays", seen).entry);

  assertEquals(scheduler.cancelSession("gone"), 1);
  assertEquals(scheduler.queueLength, 1);
  assertEquals(seen.includes("position:stays:1"), true);
});

Deno.test("a prompt that waited too long expires instead of being sent", () => {
  const clock = testClock();
  const scheduler = new Scheduler(LIMITS, clock);
  const seen: string[] = [];
  scheduler.submit(entry("a", seen).entry);
  scheduler.submit(entry("b", seen).entry);
  scheduler.submit(entry("late", seen).entry);

  clock.advance(LIMITS.maxQueueWaitMs + 1);

  assertEquals(seen.includes("expired:late"), true);
  assertEquals(scheduler.queueLength, 0);
});

/**
 * The limit belongs to the account, so one session hitting it is information
 * about all of them.
 */
Deno.test("rate limiting pauses admission for every session, then lifts", () => {
  const clock = testClock();
  const scheduler = new Scheduler(LIMITS, clock, 5_000);
  const seen: string[] = [];

  scheduler.noteRateLimit();
  assertEquals(scheduler.pausedBecause, "provider backoff");

  const held = scheduler.submit(entry("a", seen).entry);
  assertEquals(held.status, "queued");

  clock.advance(5_001);
  assertEquals(scheduler.pausedBecause, null);
  assertEquals(seen.includes("admitted:a"), true);
});

Deno.test("a second rate limit backs off further, and success decays it", () => {
  const clock = testClock();
  const scheduler = new Scheduler(LIMITS, clock, 1_000, 60_000);

  scheduler.noteRateLimit();
  scheduler.noteRateLimit();
  assertEquals(scheduler.backoffRemainingMs, 2_000);

  clock.advance(2_001);
  scheduler.noteSuccess();
  scheduler.noteRateLimit();
  assertEquals(scheduler.backoffRemainingMs, 1_000);
});

Deno.test("live sessions are capped, and starts are spaced apart", () => {
  const clock = testClock();
  const scheduler = new Scheduler(LIMITS, clock, 5_000, 300_000, 750);

  assertEquals(scheduler.reserveSession()?.delayMs, 0);
  assertEquals(scheduler.reserveSession()?.delayMs, 750);
  assertEquals(scheduler.reserveSession()?.delayMs, 1_500);
  assertEquals(scheduler.reserveSession(), null);

  scheduler.releaseSession();
  assertEquals(scheduler.sessions, 2);
  assertEquals(scheduler.reserveSession() !== null, true);
});

Deno.test("shutdown stops the timers so the daemon can exit", () => {
  const clock = testClock();
  const scheduler = new Scheduler(LIMITS, clock);
  const seen: string[] = [];
  scheduler.submit(entry("a", seen).entry);
  scheduler.submit(entry("b", seen).entry);
  scheduler.submit(entry("c", seen).entry);

  scheduler.shutdown();
  clock.advance(LIMITS.maxQueueWaitMs * 10);

  assertEquals(scheduler.queueLength, 0);
  assertEquals(seen.includes("expired:c"), false);
});
