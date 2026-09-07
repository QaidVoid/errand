import { assertEquals } from "@std/assert";
import { parseSize } from "./size.ts";

Deno.test("a size is read as bytes, with or without a suffix", () => {
  assertEquals(parseSize("1024"), 1024);
  assertEquals(parseSize("512m"), 512 * 1024 ** 2);
  assertEquals(parseSize("4g"), 4 * 1024 ** 3);
  assertEquals(parseSize("1.5g"), Math.floor(1.5 * 1024 ** 3));
});

Deno.test("the spellings people actually use all work", () => {
  assertEquals(parseSize("4G"), parseSize("4g"));
  assertEquals(parseSize("4gb"), parseSize("4g"));
  assertEquals(parseSize(" 4 g "), parseSize("4g"));
});

/** A limit that validates and is then not applied is worse than a refusal. */
Deno.test("anything that is not a size is refused rather than guessed", () => {
  assertEquals(parseSize("four gigs"), undefined);
  assertEquals(parseSize("12x"), undefined);
  assertEquals(parseSize(""), undefined);
  assertEquals(parseSize("-3g"), undefined);
});
