import { assertEquals, assertStringIncludes } from "@std/assert";
import {
  MESSAGE_LIMIT,
  splitMessage,
  THREAD_NAME_LIMIT,
  threadName,
  tokens,
  toolLine,
  truncate,
  usageSummary,
} from "./render.ts";

Deno.test("text that fits is one message, and nothing is one message of nothing", () => {
  assertEquals(splitMessage("short"), ["short"]);
  assertEquals(splitMessage(""), []);
});

Deno.test("a long text is split at line boundaries, and every piece fits", () => {
  const text = Array.from({ length: 400 }, (_unused, index) => `line ${index}`).join("\n");

  const pieces = splitMessage(text);

  assertEquals(pieces.length > 1, true);
  for (const piece of pieces) assertEquals([...piece].length <= MESSAGE_LIMIT, true);
  assertEquals(pieces.join("\n"), text);
});

/** A byte split would cut a character in half and post a broken glyph. */
Deno.test("splitting counts code points, never bytes", () => {
  const wide = "\u{1F50C}".repeat(1_500);

  const pieces = splitMessage(wide, 100);

  for (const piece of pieces) {
    assertEquals([...piece].length <= 100, true);
    assertEquals(piece.includes("\u{FFFD}"), false);
  }
  assertEquals(pieces.join(""), wide);
});

/**
 * Each message has to stand on its own, so a split inside a fence closes it
 * and reopens it with the same language.
 */
Deno.test("a split inside a code fence repairs the fence on both sides", () => {
  const code = Array.from({ length: 300 }, (_unused, index) => `  const x${index} = ${index};`);
  const text = ["before", "```ts", ...code, "```", "after"].join("\n");

  const pieces = splitMessage(text);

  assertEquals(pieces.length > 1, true);
  for (const piece of pieces) {
    const fences = (piece.match(/```/g) ?? []).length;
    assertEquals(fences % 2, 0, `unbalanced fences in: ${piece.slice(0, 40)}`);
  }
  assertStringIncludes(pieces[1] ?? "", "```ts");
});

Deno.test("a single line longer than the limit is broken up before anything else", () => {
  const pieces = splitMessage("x".repeat(5_000));

  assertEquals(pieces.length >= 3, true);
  for (const piece of pieces) assertEquals([...piece].length <= MESSAGE_LIMIT, true);
});

Deno.test("a thread is named after the project and what was asked", () => {
  const name = threadName("demo", "fix the failing test\nand explain why");

  assertStringIncludes(name, "demo");
  assertStringIncludes(name, "fix the failing test");
  assertEquals(name.includes("explain why"), false);
  assertEquals([...name].length <= THREAD_NAME_LIMIT, true);
});

Deno.test("a very long ask is cut to the limit without splitting a character", () => {
  const name = threadName("demo", "\u{1F50C}".repeat(200));

  assertEquals([...name].length <= THREAD_NAME_LIMIT, true);
  assertEquals(name.includes("\u{FFFD}"), false);
});

/** The cut is on the content; the note about it is what makes the cut visible. */
Deno.test("truncating keeps the limit and says what it dropped", () => {
  assertEquals(truncate("short", 20), "short");

  const cut = truncate("x".repeat(50), 10);
  assertEquals(cut.startsWith("x".repeat(10)), true);
  assertStringIncludes(cut, "40 more characters");
});

Deno.test("a tool call reads as the tool and what it acted on", () => {
  assertStringIncludes(toolLine("bash", "ls -la"), "`bash`");
  assertStringIncludes(toolLine("bash", "ls -la"), "`ls -la`");
  assertStringIncludes(toolLine("read", undefined), "`read`");
});

/** A backtick in the target would end the code span and spill markup. */
Deno.test("a backtick in what a tool acted on cannot break the line", () => {
  const line = toolLine("bash", "echo `whoami`");

  assertEquals(line.includes("`whoami`"), false);
  assertEquals((line.match(/`/g) ?? []).length % 2, 0);
});

Deno.test("a target spanning lines is flattened onto one", () => {
  assertEquals(toolLine("bash", "one\n  two").includes("\n"), false);
});

Deno.test("counts are shown with the magnitude a reader can compare", () => {
  assertEquals(tokens(0), "0");
  assertEquals(tokens(999), "999");
  assertEquals(tokens(1_500), "1.5k");
  assertEquals(tokens(1_500_000), "1.5M");
  assertEquals(tokens(1_500_000_000), "1.5B");
  assertEquals(tokens(123_400), "123k");
});

/** Rounding up must not report a value in the magnitude below its own. */
Deno.test("a count that rounds past its magnitude carries up", () => {
  assertEquals(tokens(999_999), "1.0M");
  assertEquals(tokens(999_999_999), "1.0B");
});

Deno.test("usage says what a session cost in terms a reader can act on", () => {
  const line = usageSummary({
    input: 240_000,
    cacheRead: 214_000,
    totalTokens: 264_000,
    cost: 0.41,
    contextTokens: 118_000,
    contextWindow: 1_000_000,
  });

  assertStringIncludes(line, "264k tokens");
  assertStringIncludes(line, "47% cached");
  assertStringIncludes(line, "$0.41");
});

/** Without the share, a number of tokens says nothing about how much is left. */
Deno.test("context is reported as a share of what the model holds", () => {
  const line = usageSummary({
    input: 1,
    cacheRead: 0,
    totalTokens: 1,
    cost: 0,
    contextTokens: 500_000,
    contextWindow: 1_000_000,
  });

  assertStringIncludes(line, "50%");
});
