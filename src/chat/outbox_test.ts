import { assertEquals, assertStringIncludes } from "@std/assert";
import { createLogger, type LogLevel } from "../log.ts";
import { DEFAULT_MAX_BUFFERED, Outbox } from "./outbox.ts";

function outbox(maxBuffered = DEFAULT_MAX_BUFFERED) {
  const lines: [LogLevel, string][] = [];
  const gaps: number[] = [];
  const box = new Outbox(
    createLogger({}, (level, line) => lines.push([level, line])),
    (count) => {
      gaps.push(count);
      return Promise.resolve();
    },
    maxBuffered,
  );
  return { box, gaps, lines };
}

Deno.test("what is queued runs, in the order it was queued", async () => {
  const { box } = outbox();
  const ran: number[] = [];

  for (const index of [1, 2, 3]) {
    box.enqueue(() => {
      ran.push(index);
      return Promise.resolve();
    });
  }
  await box.flush();

  assertEquals(ran, [1, 2, 3]);
  assertEquals(box.pending, 0);
});

/** Two overlapping actions on one thread would otherwise interleave. */
Deno.test("a slow task holds the ones behind it until it finishes", async () => {
  const { box } = outbox();
  const ran: string[] = [];
  let release = (): void => {};
  const slow = new Promise<void>((resolve) => (release = resolve));

  box.enqueue(async () => {
    await slow;
    ran.push("slow");
  });
  box.enqueue(() => {
    ran.push("fast");
    return Promise.resolve();
  });

  assertEquals(ran, []);
  release();
  await box.flush();
  assertEquals(ran, ["slow", "fast"]);
});

/** Output posted into a closed connection is lost, so it waits instead. */
Deno.test("nothing is attempted while the connection is down", async () => {
  const { box } = outbox();
  const ran: string[] = [];

  box.setConnected(false);
  box.enqueue(() => {
    ran.push("held");
    return Promise.resolve();
  });
  await box.flush();
  assertEquals(ran, []);

  box.setConnected(true);
  await box.flush();
  assertEquals(ran, ["held"]);
});

/** A failed action stays at the head, or a reconnect reorders the thread. */
Deno.test("a task that fails is retried in place when the connection returns", async () => {
  const { box, lines } = outbox();
  const attempts: string[] = [];
  let failing = true;

  box.enqueue(() => {
    attempts.push("first");
    if (failing) return Promise.reject(new Error("the gateway went"));
    return Promise.resolve();
  });
  box.enqueue(() => {
    attempts.push("second");
    return Promise.resolve();
  });
  await box.flush();

  assertEquals(attempts, ["first"]);
  assertEquals(lines.some(([level]) => level === "warn"), true);

  failing = false;
  box.setConnected(true);
  await box.flush();

  assertEquals(attempts, ["first", "first", "second"]);
});

/** A disconnected daemon must not grow without limit. */
Deno.test("a full buffer drops the oldest and keeps the newest", async () => {
  const { box, gaps } = outbox(3);
  const ran: number[] = [];

  box.setConnected(false);
  for (const index of [1, 2, 3, 4, 5]) {
    box.enqueue(() => {
      ran.push(index);
      return Promise.resolve();
    });
  }
  assertEquals(box.pending, 3);
  assertEquals(box.droppedCount, 2);

  box.setConnected(true);
  await box.flush();

  assertEquals(ran, [3, 4, 5]);
  // A truncated conversation is never passed off as a complete one.
  assertEquals(gaps, [2]);
});

Deno.test("a gap is announced once, not on every drain after it", async () => {
  const { box, gaps } = outbox(1);

  box.setConnected(false);
  for (let index = 0; index < 3; index += 1) box.enqueue(() => Promise.resolve());
  box.setConnected(true);
  await box.flush();
  await box.flush();
  box.enqueue(() => Promise.resolve());
  await box.flush();

  assertEquals(gaps, [2]);
});

Deno.test("closing discards what is queued and takes nothing more", async () => {
  const { box } = outbox();
  const ran: string[] = [];

  box.setConnected(false);
  box.enqueue(() => {
    ran.push("queued");
    return Promise.resolve();
  });
  box.close();
  box.enqueue(() => {
    ran.push("after");
    return Promise.resolve();
  });

  box.setConnected(true);
  await box.flush();

  assertEquals(ran, []);
  assertEquals(box.isClosed, true);
  assertEquals(box.pending, 0);
});

/** A caller must not see an empty-looking outbox while a drain is in flight. */
Deno.test("flushing waits for a drain already running", async () => {
  const { box } = outbox();
  const ran: string[] = [];
  let release = (): void => {};
  const slow = new Promise<void>((resolve) => (release = resolve));

  box.enqueue(async () => {
    await slow;
    ran.push("slow");
  });
  const flushing = box.flush();
  release();
  await flushing;

  assertEquals(ran, ["slow"]);
});

Deno.test("a failure to announce a gap is reported and not lost", async () => {
  const lines: [LogLevel, string][] = [];
  const box = new Outbox(
    createLogger({}, (level, line) => lines.push([level, line])),
    () => Promise.reject(new Error("the thread is gone")),
    1,
  );

  box.setConnected(false);
  box.enqueue(() => Promise.resolve());
  box.enqueue(() => Promise.resolve());
  box.setConnected(true);
  await box.flush();

  assertStringIncludes(lines.map(([, line]) => line).join("\n"), "reporting dropped messages");
});
