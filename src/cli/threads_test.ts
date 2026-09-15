import { assertEquals, assertStringIncludes } from "@std/assert";
import { createLogger } from "../log.ts";
import { type ThreadRecord, ThreadRegistry } from "../session/registry.ts";
import { type Deps, findThread, runThreads } from "./threads.ts";

const NOW = 1_700_000_000_000;

function record(overrides: Partial<ThreadRecord> = {}): ThreadRecord {
  return {
    threadId: "thread-aaaa",
    sessionId: "s-1",
    stateDir: "/state/s-1",
    projectName: "demo",
    projectPath: "/projects/demo",
    ownerId: "u-1",
    guests: [],
    updatedAt: NOW - 3_600_000,
    ...overrides,
  };
}

/** A registry in a temporary file, since the commands write to it. */
async function harness(
  records: ThreadRecord[] = [],
  sizes: Record<string, number> = {},
  projects: Record<string, { name: string; path: string } | undefined> = {},
) {
  const root = await Deno.makeTempDir({ prefix: "errand-cli-" });
  const registry = new ThreadRegistry(`${root}/threads.json`, createLogger({}, () => {}));
  for (const entry of records) registry.remember(entry);

  const written: string[] = [];
  const removed: string[] = [];
  const deps: Deps = {
    registry,
    sizeOf: (stateDir) => Promise.resolve(sizes[stateDir]),
    remove: (stateDir) => {
      removed.push(stateDir);
      return Promise.resolve();
    },
    write: (line) => written.push(line),
    now: () => NOW,
    stateRoot: "/state",
    projectOf: (stateDir) => Promise.resolve(projects[stateDir]),
  };

  return {
    deps,
    registry,
    removed,
    out: () => written.join("\n"),
    cleanup: () => Deno.remove(root, { recursive: true }),
  };
}

Deno.test("an empty index says so rather than printing a header", async () => {
  const h = await harness();
  assertEquals(await runThreads(["list"], h.deps), 0);
  assertEquals(h.out(), "no threads are remembered");
  await h.cleanup();
});

Deno.test("listing shows each thread, its project, and what it holds", async () => {
  const h = await harness([record()], { "/state/s-1": 2 * 1024 * 1024 });

  await runThreads(["list"], h.deps);

  assertStringIncludes(h.out(), "thread-aaaa");
  assertStringIncludes(h.out(), "demo");
  assertStringIncludes(h.out(), "2.0M");
  assertStringIncludes(h.out(), "1h");
  await h.cleanup();
});

Deno.test("a thread whose data is gone is listed as gone, not as empty", async () => {
  const h = await harness([record()]);
  await runThreads(["list"], h.deps);
  assertStringIncludes(h.out(), "gone");
  await h.cleanup();
});

Deno.test("a thread can be named by a part of its id, when only one matches", () => {
  const records = [record(), record({ threadId: "thread-bbbb" })];

  assertEquals((findThread(records, "thread-aa") as ThreadRecord).threadId, "thread-aaaa");
  assertEquals(findThread(records, "nothing"), undefined);
  assertEquals("ambiguous" in (findThread(records, "thread-") ?? {}), true);
});

/**
 * Chat ids are snowflakes: nineteen digits differing only near the end, so a
 * prefix matches everything and the tail is what anybody copies.
 */
Deno.test("the tail of a snowflake identifies it, where the head cannot", () => {
  const records = [
    record({ threadId: "1546185772997157045" }),
    record({ threadId: "1546185772997199999" }),
  ];

  assertEquals((findThread(records, "157045") as ThreadRecord).threadId, "1546185772997157045");
  assertEquals((findThread(records, "199999") as ThreadRecord).threadId, "1546185772997199999");
  assertEquals("ambiguous" in (findThread(records, "15461857729971") ?? {}), true);
});

Deno.test("showing a thread prints what it is and where it lives", async () => {
  const h = await harness([record({ guests: ["u-2"] })], { "/state/s-1": 4096 });

  assertEquals(await runThreads(["show", "thread-aa"], h.deps), 0);

  assertStringIncludes(h.out(), "session   s-1");
  assertStringIncludes(h.out(), "/projects/demo");
  assertStringIncludes(h.out(), "4.0K");
  assertStringIncludes(h.out(), "guests    u-2");
  await h.cleanup();
});

Deno.test("an ambiguous name is refused rather than guessed at", async () => {
  const h = await harness([record(), record({ threadId: "thread-bbbb", sessionId: "s-2" })]);

  assertEquals(await runThreads(["show", "thread-"], h.deps), 1);
  assertStringIncludes(h.out(), "matches 2 threads");
  await h.cleanup();
});

Deno.test("forgetting stops a thread resuming and keeps its data", async () => {
  const h = await harness([record()], { "/state/s-1": 10 });

  assertEquals(await runThreads(["forget", "thread-aaaa"], h.deps), 0);

  assertEquals(h.registry.get("thread-aaaa"), undefined);
  assertEquals(h.removed, []);
  assertStringIncludes(h.out(), "its data is still at /state/s-1");
  await h.cleanup();
});

/** It deletes the agent's history, and nothing else keeps a copy. */
Deno.test("removing asks first, and does nothing until it is confirmed", async () => {
  const h = await harness([record()], { "/state/s-1": 1024 });

  assertEquals(await runThreads(["remove", "thread-aaaa"], h.deps), 1);

  assertStringIncludes(h.out(), "this deletes /state/s-1 and its 1.0K");
  assertStringIncludes(h.out(), "--yes");
  assertEquals(h.removed, []);
  assertEquals(h.registry.get("thread-aaaa") !== undefined, true);
  await h.cleanup();
});

Deno.test("removing with consent deletes the data and forgets the thread", async () => {
  const h = await harness([record()], { "/state/s-1": 1024 });

  assertEquals(await runThreads(["remove", "thread-aaaa", "--yes"], h.deps), 0);

  assertEquals(h.removed, ["/state/s-1"]);
  assertEquals(h.registry.get("thread-aaaa"), undefined);
  await h.cleanup();
});

/** Forgetting a thread whose data survived would strand the data. */
Deno.test("a failed delete leaves the thread remembered", async () => {
  const h = await harness([record()], { "/state/s-1": 1024 });
  h.deps.remove = () => Promise.reject(new Error("permission denied"));

  assertEquals(await runThreads(["remove", "thread-aaaa", "--yes"], h.deps), 1);

  assertStringIncludes(h.out(), "could not delete");
  assertEquals(h.registry.get("thread-aaaa") !== undefined, true);
  await h.cleanup();
});

Deno.test("pruning forgets only the threads whose data is already gone", async () => {
  const h = await harness(
    [record(), record({ threadId: "thread-bbbb", sessionId: "s-2", stateDir: "/state/s-2" })],
    { "/state/s-2": 512 },
  );

  assertEquals(await runThreads(["prune"], h.deps), 0);

  assertEquals(h.registry.get("thread-aaaa"), undefined);
  assertEquals(h.registry.get("thread-bbbb") !== undefined, true);
  assertStringIncludes(h.out(), "forgot 1");
  await h.cleanup();
});

Deno.test("a command nobody has heard of prints what there is", async () => {
  const h = await harness();

  assertEquals(await runThreads(["destroy-everything"], h.deps), 2);

  assertStringIncludes(h.out(), "no threads command called destroy-everything");
  assertStringIncludes(h.out(), "usage: errand threads");
  await h.cleanup();
});

Deno.test("a command that needs a thread says so instead of guessing", async () => {
  const h = await harness([record()]);

  assertEquals(await runThreads(["show"], h.deps), 2);
  assertEquals(await runThreads(["remove"], h.deps), 2);
  assertStringIncludes(h.out(), "say which thread");
  await h.cleanup();
});

Deno.test("reviving puts a forgotten thread back so it can be resumed", async () => {
  const h = await harness([], { "/state/s-9": 4096 }, {
    "/state/s-9": { name: "demo", path: "/projects/demo" },
  });

  const code = await runThreads(
    ["revive", "s-9", "--thread", "thread-zzzz", "--owner", "u-7"],
    h.deps,
  );

  assertEquals(code, 0);
  const put = h.registry.get("thread-zzzz");
  assertEquals(put?.sessionId, "s-9");
  assertEquals(put?.projectPath, "/projects/demo");
  assertEquals(put?.ownerId, "u-7");
  assertStringIncludes(h.out(), "post in the thread to resume it");
  await h.cleanup();
});

/** Neither is written down in the session, so neither is guessed at. */
Deno.test("reviving without a thread or an owner is refused", async () => {
  const h = await harness([], { "/state/s-9": 10 }, {
    "/state/s-9": { name: "demo", path: "/projects/demo" },
  });

  assertEquals(await runThreads(["revive", "s-9", "--thread", "thread-zzzz"], h.deps), 2);
  assertStringIncludes(h.out(), "both --thread and --owner are needed");
  await h.cleanup();
});

Deno.test("reviving a session with nothing on disk is refused", async () => {
  const h = await harness();

  const code = await runThreads(["revive", "gone", "--thread", "t", "--owner", "u"], h.deps);

  assertEquals(code, 1);
  assertStringIncludes(h.out(), "nothing on disk");
  await h.cleanup();
});

Deno.test("reviving onto a thread that is already remembered changes nothing", async () => {
  const h = await harness([record()], { "/state/s-9": 10 }, {
    "/state/s-9": { name: "demo", path: "/projects/demo" },
  });

  const code = await runThreads(
    ["revive", "s-9", "--thread", "thread-aaaa", "--owner", "u"],
    h.deps,
  );

  assertEquals(code, 1);
  assertStringIncludes(h.out(), "already remembered");
  await h.cleanup();
});
