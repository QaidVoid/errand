import { assertEquals } from "@std/assert";
import { ALL_CHARS, glyph, prefixed, PREFIXES, reaction, REACTIONS } from "./chars.ts";

Deno.test("every entry renders to the character it names", () => {
  assertEquals(reaction("accepted"), "\u23f3");
  assertEquals(reaction("succeeded"), "\u2705");
  assertEquals(reaction("failed"), "\u274c");
  assertEquals(reaction("interrupted"), "\u23f9\ufe0f");
  assertEquals(glyph(PREFIXES.tool), "\u{1F527}");
});

Deno.test("a status line leads with the glyph for its state", () => {
  assertEquals(
    prefixed("warning", "the provider is backing off"),
    "\u26a0\ufe0f the provider is backing off",
  );
});

/** Each names one state, so two states cannot share a character. */
Deno.test("no character is used for more than one state", () => {
  const rendered = ALL_CHARS.map(glyph);
  assertEquals(new Set(rendered).size, rendered.length);
});

Deno.test("every entry says what it means and what it is called", () => {
  for (const entry of ALL_CHARS) {
    assertEquals(entry.name.length > 0, true);
    assertEquals(entry.meaning.length > 0, true);
    assertEquals(entry.codepoints.length > 0, true);
  }
});

/** The table is the whole set, so nothing may reach chat from anywhere else. */
Deno.test("the table covers every reaction and prefix there is", () => {
  assertEquals(ALL_CHARS.length, Object.keys(REACTIONS).length + Object.keys(PREFIXES).length);
});

/** Declared as codepoints, so the file holding them is itself ASCII. */
Deno.test("the table is written without a single literal glyph", async () => {
  const source = await Deno.readTextFile(new URL("./chars.ts", import.meta.url));
  assertEquals([...source].every((character) => character.codePointAt(0)! <= 127), true);
});
