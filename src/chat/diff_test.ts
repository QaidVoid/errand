import { assertEquals, assertStringIncludes } from "@std/assert";
import { CONTEXT_LINES, fileDiff, MAX_DIFF_LINES, renderDiff } from "./diff.ts";

Deno.test("a file that did not change has nothing to show", () => {
  assertEquals(fileDiff("same\ntext", "same\ntext"), {
    empty: true,
    added: 0,
    removed: 0,
    body: "",
  });
});

Deno.test("a changed line reads as one added and one removed", () => {
  const diff = fileDiff("one\ntwo\nthree", "one\nTWO\nthree");

  assertEquals(diff.added, 1);
  assertEquals(diff.removed, 1);
  assertStringIncludes(diff.body, "-two");
  assertStringIncludes(diff.body, "+TWO");
  assertStringIncludes(diff.body, " one");
});

/** The point of a diff: a small change in a large file reads as a small one. */
Deno.test("unchanged regions are dropped apart from a little context", () => {
  const before = Array.from({ length: 200 }, (_unused, index) => `line ${index}`);
  const after = [...before];
  after[100] = "changed";

  const diff = fileDiff(before.join("\n"), after.join("\n"));

  const shown = diff.body.split("\n").filter((line) => line !== "@@");
  assertEquals(shown.length, 2 * CONTEXT_LINES + 2);
  assertStringIncludes(diff.body, "+changed");
});

/** Two changes far apart must not read as one continuous region. */
Deno.test("a gap between changes is marked rather than silently closed", () => {
  const before = Array.from({ length: 60 }, (_unused, index) => `line ${index}`);
  const after = [...before];
  after[5] = "first";
  after[40] = "second";

  const diff = fileDiff(before.join("\n"), after.join("\n"));

  assertEquals(diff.body.split("\n").filter((line) => line === "@@").length, 1);
});

Deno.test("a very large change is cut and says how much it left out", () => {
  const before = Array.from({ length: 300 }, (_unused, index) => `old ${index}`).join("\n");
  const after = Array.from({ length: 300 }, (_unused, index) => `new ${index}`).join("\n");

  const diff = fileDiff(before, after);

  const body = diff.body.split("\n");
  assertEquals(body.length, MAX_DIFF_LINES + 1);
  assertStringIncludes(body[body.length - 1] ?? "", "further line(s) not shown");
  assertEquals(diff.added, 300);
});

/** A minified file is one line, and would otherwise be posted whole. */
Deno.test("a single enormous line is clipped, not posted entire", () => {
  const diff = fileDiff("short", "x".repeat(5_000));

  for (const line of diff.body.split("\n")) {
    assertEquals(line.length < 300, true);
  }
  assertStringIncludes(diff.body, "...");
});

Deno.test("an added file is all additions", () => {
  const diff = fileDiff("", "one\ntwo");

  assertEquals(diff.removed, 1);
  assertEquals(diff.added, 2);
});

Deno.test("the message says which file and by how much", () => {
  const message = renderDiff("src/main.ts", fileDiff("one", "two"));

  assertStringIncludes(message, "`src/main.ts` +1 -1");
  assertStringIncludes(message, "```diff");
  assertEquals((message.match(/```/g) ?? []).length, 2);
});
