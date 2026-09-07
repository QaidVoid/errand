import { assertEquals, assertStringIncludes } from "@std/assert";
import {
  bytes,
  compactionLine,
  dialogLines,
  directoryListing,
  fileView,
  MAX_LISTED_ENTRIES,
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

Deno.test("a byte count is shown in the units a disk quota uses", () => {
  assertEquals(bytes(0), "0 B");
  assertEquals(bytes(999), "999 B");
  assertEquals(bytes(1_500), "1.5 kB");
  assertEquals(bytes(2_400_000), "2.4 MB");
  assertEquals(bytes(3_000_000_000), "3.0 GB");
  assertEquals(bytes(5e15), "5000.0 TB");
});

Deno.test("a listing puts the sizes in one column and marks directories", () => {
  const listing = directoryListing([
    { name: "src", path: "src", directory: true, size: 0 },
    { name: "readme.md", path: "readme.md", directory: false, size: 1_200 },
  ], "demo");

  const rows = (listing.split("```")[1] ?? "").split("\n").filter((row) => row.length > 0);
  assertEquals(rows[0]?.endsWith("src/"), true);
  assertStringIncludes(rows[1] ?? "", "1.2 kB");
  assertEquals((rows[0] ?? "").indexOf("src/"), (rows[1] ?? "").indexOf("readme.md"));
});

Deno.test("an empty directory says so rather than showing an empty block", () => {
  const listing = directoryListing([], "demo/src");

  assertEquals(listing.includes("```"), false);
  assertStringIncludes(listing, "is empty");
});

/** A thread cannot show thousands of entries, and nobody reads them there. */
Deno.test("a very long listing is cut and says how much it left out", () => {
  const many = Array.from({ length: MAX_LISTED_ENTRIES + 20 }, (_unused, index) => ({
    name: `file-${index}`,
    path: `file-${index}`,
    directory: false,
    size: 1,
  }));

  const listing = directoryListing(many, "demo");

  assertStringIncludes(listing, "220 entries");
  assertStringIncludes(listing, "... 20 more");
});

Deno.test("a file is shown fenced in its own language", () => {
  const view = fileView({
    path: "src/main.ts",
    size: 13,
    binary: false,
    truncated: false,
    text: "const x = 1;\n",
    language: "ts",
  });

  assertStringIncludes(view, "```ts");
  assertStringIncludes(view, "const x = 1;");
  assertEquals(view.includes("cut at"), false);
});

Deno.test("a file that was cut says so and says where to get the rest", () => {
  const view = fileView({
    path: "big.txt",
    size: 5_000_000,
    binary: false,
    truncated: true,
    text: "x",
    language: "",
  });

  assertStringIncludes(view, "cut at");
  assertStringIncludes(view, "5.0 MB");
  assertStringIncludes(view, "!file");
});

Deno.test("a binary file is named and offered, not pasted", () => {
  const view = fileView({
    path: "logo.png",
    size: 40_000,
    binary: true,
    truncated: false,
    text: "",
    language: "",
  });

  assertEquals(view.includes("```"), false);
  assertStringIncludes(view, "40.0 kB");
  assertStringIncludes(view, "!file");
});

/** One that freed nothing looks like one that freed half the window. */
Deno.test("a compaction says how much context it actually freed", () => {
  const line = compactionLine({
    success: true,
    data: { tokensBefore: 180_000, estimatedTokensAfter: 42_000 },
  });

  assertStringIncludes(line, "180k");
  assertStringIncludes(line, "42.0k");
});

Deno.test("a compaction that says nothing is still reported as having run", () => {
  assertStringIncludes(compactionLine({ success: true, data: {} }), "compacted the conversation");
});

Deno.test("a compaction the agent refused says so, with its reason", () => {
  const line = compactionLine({ success: false, error: "nothing to compact" });

  assertStringIncludes(line, "did not run");
  assertStringIncludes(line, "nothing to compact");
});

/** A thread has no buttons, so an answer has to be spelled out. */
Deno.test("a choice is numbered so a reply can name one", () => {
  const lines = dialogLines({
    id: "1",
    method: "select",
    title: "Which branch?",
    options: ["main", "develop"],
  });

  assertStringIncludes(lines, "1. main");
  assertStringIncludes(lines, "2. develop");
  assertStringIncludes(lines, "reply with a number");
});

Deno.test("a confirmation says what answers it takes", () => {
  const lines = dialogLines({ id: "1", method: "confirm", title: "Delete it?" });

  assertStringIncludes(lines, "Delete it?");
  assertStringIncludes(lines, "reply yes or no");
});

Deno.test("a question with no options still asks for an answer", () => {
  const lines = dialogLines({
    id: "1",
    method: "input",
    title: "Name it",
    message: "any name will do",
  });

  assertStringIncludes(lines, "any name will do");
  assertStringIncludes(lines, "reply with your answer");
});
