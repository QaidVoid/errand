import { assertEquals, assertStringIncludes } from "@std/assert";
import { createLogger, type LogLevel } from "../log.ts";
import { isRecord, MAX_REMEMBERED, type ThreadRecord, ThreadRegistry } from "./registry.ts";

function record(overrides: Partial<ThreadRecord> = {}): ThreadRecord {
  return {
    threadId: "t-1",
    sessionId: "s-1",
    stateDir: "/state/s-1",
    projectName: "demo",
    projectPath: "/projects/demo",
    ownerId: "u-1",
    guests: [],
    updatedAt: 1_000,
    ...overrides,
  };
}

function collected() {
  const lines: [LogLevel, string][] = [];
  return { lines, log: createLogger({}, (level, line) => lines.push([level, line])) };
}

async function withRegistry(
  run: (
    registry: ThreadRegistry,
    path: string,
    lines: [LogLevel, string][],
  ) => void | Promise<void>,
): Promise<void> {
  const root = await Deno.makeTempDir({ prefix: "errand-registry-" });
  const path = `${root}/threads.json`;
  const { lines, log } = collected();
  try {
    await run(new ThreadRegistry(path, log), path, lines);
  } finally {
    await Deno.remove(root, { recursive: true });
  }
}

Deno.test("what is remembered comes back after a restart", () =>
  withRegistry((registry, path, lines) => {
    registry.remember(record({ guests: ["u-2"] }));

    const reopened = new ThreadRegistry(path, createLogger({}, (l, m) => lines.push([l, m])));
    reopened.load();

    assertEquals(reopened.get("t-1")?.sessionId, "s-1");
    assertEquals(reopened.get("t-1")?.guests, ["u-2"]);
    assertEquals(reopened.size, 1);
  }));

Deno.test("a first run has an empty index and says nothing about it", () =>
  withRegistry((registry, _path, lines) => {
    registry.load();

    assertEquals(registry.size, 0);
    assertEquals(lines.length, 0);
  }));

Deno.test("a thread that is forgotten is not resumed again", () =>
  withRegistry((registry, path, lines) => {
    registry.remember(record());
    registry.forget("t-1");

    const reopened = new ThreadRegistry(path, createLogger({}, (l, m) => lines.push([l, m])));
    reopened.load();
    assertEquals(reopened.get("t-1"), undefined);
  }));

Deno.test("threads are listed most recently used first", () =>
  withRegistry((registry) => {
    registry.remember(record({ threadId: "old", updatedAt: 1 }));
    registry.remember(record({ threadId: "new", updatedAt: 9 }));
    registry.remember(record({ threadId: "mid", updatedAt: 5 }));

    assertEquals(registry.all().map((entry) => entry.threadId), ["new", "mid", "old"]);
  }));

Deno.test("the oldest are dropped once the bound is passed", () =>
  withRegistry((registry) => {
    for (let index = 0; index < MAX_REMEMBERED + 3; index += 1) {
      registry.remember(record({ threadId: `t-${index}`, updatedAt: index }));
    }

    assertEquals(registry.size, MAX_REMEMBERED);
    assertEquals(registry.get("t-0"), undefined);
    assertEquals(registry.get("t-2"), undefined);
    assertEquals(registry.get(`t-${MAX_REMEMBERED + 2}`) !== undefined, true);
  }));

/** The rename is what makes it atomic; nothing may be left half written. */
Deno.test("writing leaves no temporary file behind", () =>
  withRegistry(async (registry, path) => {
    registry.remember(record());

    const left: string[] = [];
    for await (const entry of Deno.readDir(path.replace("/threads.json", ""))) {
      left.push(entry.name);
    }
    assertEquals(left, ["threads.json"]);
  }));

/**
 * It is the only copy of what was there, so it is kept for someone to look at
 * rather than quietly replaced.
 */
Deno.test("an index that will not parse is kept aside, loudly", () =>
  withRegistry(async (registry, path, lines) => {
    await Deno.writeTextFile(path, "{ this is not json");

    registry.load();

    assertEquals(registry.size, 0);
    assertEquals((await Deno.stat(`${path}.broken`)).isFile, true);
    assertEquals(lines[0]?.[0], "error");
    assertStringIncludes(lines[0]?.[1] ?? "", "kept aside");
  }));

Deno.test("entries that are not records are dropped, and the rest survive", () =>
  withRegistry(async (registry, path, lines) => {
    await Deno.writeTextFile(
      path,
      JSON.stringify([record(), { threadId: "half" }, record({ threadId: "t-2" })]),
    );

    registry.load();

    assertEquals(registry.size, 2);
    assertEquals(lines[0]?.[0], "warn");
    assertStringIncludes(lines[0]?.[1] ?? "", "skipped=1");
  }));

/** The cost is invisible until a restart, by which time the threads are gone. */
Deno.test("an index that cannot be written says so at the time", () =>
  withRegistry((_registry, path, lines) => {
    // A file where a directory has to be, so the write cannot succeed.
    Deno.writeTextFileSync(path, "not a directory");
    const blocked = new ThreadRegistry(
      `${path}/nested/threads.json`,
      createLogger({}, (level, line) => lines.push([level, line])),
    );

    blocked.remember(record());

    assertEquals(lines.length, 1);
    assertEquals(lines[0]?.[0], "error");
    assertStringIncludes(lines[0]?.[1] ?? "", "restart will forget threads");
  }));

/** A record written before the model was remembered must still load. */
Deno.test("a record without a model is still a record", () => {
  const older = {
    threadId: "t1",
    sessionId: "s1",
    stateDir: "/tmp/s1",
    projectName: "demo",
    projectPath: "/tmp/demo",
    ownerId: "u1",
    guests: [],
    updatedAt: 1,
  };
  assertEquals(isRecord(older), true);
  assertEquals(isRecord({ ...older, provider: "meta", model: "muse:xhigh" }), true);
  // A model that is not a name is not a record.
  assertEquals(isRecord({ ...older, model: 7 }), false);
});
