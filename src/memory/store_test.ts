import { assertEquals, assertStringIncludes } from "@std/assert";
import {
  DEFAULT_MEMORY_BUDGET,
  MAX_FACT_LENGTH,
  MAX_PROJECT_FACTS,
  memoryInstructions,
  MemoryStore,
  parseNotes,
} from "./store.ts";

function store(): MemoryStore {
  return new MemoryStore(":memory:");
}

Deno.test("a fact told once is remembered and read back", () => {
  const memory = store();
  assertEquals(memory.remember("user", "u1", "prefers jj over git", "s1"), true);

  assertEquals(memory.factsFor("user", "u1").map((held) => held.fact), ["prefers jj over git"]);
  memory.close();
});

/** An agent writing the same fact every session must not grow the block. */
Deno.test("the same fact twice is stored once and reported as nothing new", () => {
  const memory = store();
  memory.remember("user", "u1", "prefers jj", "s1");

  assertEquals(memory.remember("user", "u1", "  prefers   jj  ", "s2"), false);
  assertEquals(memory.factsFor("user", "u1").length, 1);
  memory.close();
});

Deno.test("nothing worth storing is refused rather than stored empty", () => {
  const memory = store();

  assertEquals(memory.remember("user", "u1", "   ", "s1"), false);
  assertEquals(memory.remember("user", "", "a real fact", "s1"), false);
  memory.close();
});

Deno.test("a fact longer than a fact is cut to one", () => {
  const memory = store();
  memory.remember("user", "u1", "x".repeat(500), "s1");

  assertEquals(memory.factsFor("user", "u1")[0]?.fact.length, MAX_FACT_LENGTH);
  memory.close();
});

/** The two scopes are separate memories that happen to share a table. */
Deno.test("what a project knows is not what a person knows", () => {
  const memory = store();
  memory.remember("user", "u1", "likes short commits", "s1");
  memory.remember("project", "demo", "the build is deno task check", "s1");

  assertEquals(memory.factsFor("user", "demo"), []);
  assertEquals(memory.factsFor("project", "demo").length, 1);
  memory.close();
});

Deno.test("facts come back newest first, since the newest is the correction", () => {
  const memory = store();
  memory.remember("user", "u1", "first", "s1");
  memory.remember("user", "u1", "second", "s1");

  assertEquals(memory.factsFor("user", "u1").map((held) => held.fact), ["second", "first"]);
  memory.close();
});

/** A project collects facts from everyone who works on it, so it is pruned. */
Deno.test("a project keeps the newest facts and drops the oldest", () => {
  const memory = store();
  for (let index = 0; index < MAX_PROJECT_FACTS + 10; index += 1) {
    memory.remember("project", "demo", `fact ${index}`, "s1");
  }

  const kept = memory.factsFor("project", "demo", Number.MAX_SAFE_INTEGER);
  assertEquals(kept.length, MAX_PROJECT_FACTS);
  assertEquals(kept[0]?.fact, `fact ${MAX_PROJECT_FACTS + 9}`);
  memory.close();
});

Deno.test("being forgotten removes the facts and the name, and says how many", () => {
  const memory = store();
  memory.rememberUser("u1", "amelia");
  memory.remember("user", "u1", "one", "s1");
  memory.remember("user", "u1", "two", "s1");

  assertEquals(memory.forget("user", "u1"), 2);
  assertEquals(memory.factsFor("user", "u1"), []);
  assertEquals(memory.displayName("u1"), undefined);
  assertEquals(memory.render("u1"), "");
  memory.close();
});

Deno.test("a name is kept and updated when it changes", () => {
  const memory = store();
  memory.rememberUser("u1", "amelia");
  memory.rememberUser("u1", "amelia (away)");

  assertEquals(memory.displayName("u1"), "amelia (away)");
  memory.close();
});

/** Somebody new must cost no context at all. */
Deno.test("nothing known about somebody renders nothing", () => {
  const memory = store();

  assertEquals(memory.render("nobody"), "");
  assertEquals(memory.renderForSpeaker("nobody"), "");
  assertEquals(memory.renderProject("untouched"), "");
  memory.close();
});

Deno.test("what is known is rendered as the agent will read it", () => {
  const memory = store();
  memory.rememberUser("u1", "amelia");
  memory.remember("user", "u1", "prefers jj over git", "s1");

  const block = memory.render("u1");

  assertStringIncludes(block, "You are talking to amelia.");
  assertStringIncludes(block, "- prefers jj over git");
  memory.close();
});

Deno.test("a name nobody recorded still gets the facts", () => {
  const memory = store();
  memory.remember("user", "u1", "prefers jj", "s1");

  assertStringIncludes(memory.render("u1"), "- prefers jj");
  memory.close();
});

/** Every line is paid for in context on every session that person starts. */
Deno.test("the block stops at its budget, keeping the newest", () => {
  const memory = store();
  for (let index = 0; index < 200; index += 1) {
    memory.remember("user", "u1", `fact number ${index} with some words after it`, "s1");
  }

  const block = memory.render("u1");

  assertEquals(block.length <= DEFAULT_MEMORY_BUDGET, true);
  assertStringIncludes(block, "fact number 199");
  assertEquals(block.includes("fact number 0 "), false);
  memory.close();
});

/** Sent with a message rather than in a prompt that was fixed long ago. */
Deno.test("a speaker joining later is introduced on one line", () => {
  const memory = store();
  memory.rememberUser("u2", "brendan");
  memory.remember("user", "u2", "works on the parser", "s1");

  const line = memory.renderForSpeaker("u2");

  assertEquals(line.startsWith("[context: "), true);
  assertEquals(line.includes("\n"), false);
  assertStringIncludes(line, "brendan");
  memory.close();
});

Deno.test("a project's memory names the project it is about", () => {
  const memory = store();
  memory.remember("project", "demo", "the build is deno task check", "s1");

  assertStringIncludes(memory.renderProject("demo"), "about the demo project");
  assertStringIncludes(memory.renderProject("demo"), "- the build is deno task check");
  memory.close();
});

Deno.test("what the agent writes by hand is read however it wrote it", () => {
  assertEquals(
    parseNotes("# heading\n\n- first fact\n* second fact\n  third fact  \n\n"),
    ["first fact", "second fact", "third fact"],
  );
});

Deno.test("the instructions name both files and refuse secrets", () => {
  const instructions = memoryInstructions("/state/remember.md", "/state/project-notes.md");

  assertStringIncludes(instructions, "/state/remember.md");
  assertStringIncludes(instructions, "/state/project-notes.md");
  assertStringIncludes(instructions, "Never record secrets");
});

/** Memory outlives the process, so it has to survive being reopened. */
Deno.test("what was remembered is still there after a restart", async () => {
  const directory = await Deno.makeTempDir({ prefix: "errand-memory-" });
  const path = `${directory}/memory.db`;
  try {
    const first = new MemoryStore(path);
    first.rememberUser("u1", "amelia");
    first.remember("user", "u1", "prefers jj", "s1");
    first.close();

    const second = new MemoryStore(path);
    assertEquals(second.displayName("u1"), "amelia");
    assertEquals(second.factsFor("user", "u1").length, 1);
    second.close();
  } finally {
    await Deno.remove(directory, { recursive: true });
  }
});
