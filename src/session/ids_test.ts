import { assertEquals, assertMatch, assertNotEquals } from "@std/assert";
import { sessionId, sessionToken, TOKEN_LENGTH } from "./ids.ts";
import type { ProjectSelection } from "./projects.ts";

function selection(name: string, wasExplicit: boolean): ProjectSelection {
  return { name, path: `/srv/${name}`, prompt: "go", wasExplicit };
}

Deno.test("a token is the declared length, and only lower case base 36", () => {
  for (let i = 0; i < 500; i++) {
    const token = sessionToken();
    assertEquals(token.length, TOKEN_LENGTH);
    assertMatch(token, /^[0-9a-z]+$/);
  }
});

Deno.test("tokens do not repeat, and do not follow a counter", () => {
  // The old generator prefixed an in-memory counter that restarted with the
  // daemon, so the first session of every run began with the same character.
  const drawn = new Set<string>();
  const firsts = new Set<string>();
  for (let i = 0; i < 2_000; i++) {
    const token = sessionToken();
    drawn.add(token);
    firsts.add(token[0] as string);
  }
  assertEquals(drawn.size, 2_000);
  assertEquals(firsts.size > 20, true);
});

Deno.test("every digit of the alphabet turns up, so none is quietly unreachable", () => {
  // 256 is not a multiple of 36. Folding the high bytes instead of redrawing
  // them would make 0 to 3 appear about a seventh more often than the rest.
  const seen = new Set<string>();
  for (let i = 0; i < 20_000; i++) for (const c of sessionToken()) seen.add(c);
  assertEquals(seen.size, 36);
});

Deno.test("a named project is written into the id, ahead of the token", () => {
  assertEquals(sessionId(selection("errand", true), "6a82hff"), "errand-6a82hff");
});

Deno.test("an unnamed session is its token alone, not the token twice", () => {
  // Its project directory is already named after the token.
  assertEquals(sessionId(selection("6a82hff", false), "6a82hff"), "6a82hff");
});

Deno.test("two sessions in one project stay distinct", () => {
  const first = sessionId(selection("errand", true), sessionToken());
  const second = sessionId(selection("errand", true), sessionToken());
  assertNotEquals(first, second);
});
