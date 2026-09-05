import { assertEquals } from "@std/assert";
import { within } from "./paths.ts";

const ROOT = "/projects/demo";

Deno.test("a path inside the root resolves to an absolute path", () => {
  assertEquals(within(ROOT, "src/main.ts"), "/projects/demo/src/main.ts");
  assertEquals(within(ROOT, "./notes.md"), "/projects/demo/notes.md");
  assertEquals(within(ROOT, "a/../b.txt"), "/projects/demo/b.txt");
});

Deno.test("the root itself is inside it", () => {
  assertEquals(within(ROOT, "."), ROOT);
  assertEquals(within(ROOT, ROOT), ROOT);
});

Deno.test("a path that climbs out is refused", () => {
  assertEquals(within(ROOT, "../other/secret"), undefined);
  assertEquals(within(ROOT, "src/../../escaped"), undefined);
  assertEquals(within(ROOT, "/etc/passwd"), undefined);
});

/** A sibling sharing a prefix is not inside, however similar the string is. */
Deno.test("a sibling with the same prefix is not inside", () => {
  assertEquals(within(ROOT, "/projects/demo-other/file"), undefined);
  assertEquals(within("/projects/demo", "/projects/demoted"), undefined);
});

Deno.test("deep traversal is refused however it is spelled", () => {
  for (const path of ["../..", "a/b/../../../out", "./../out", "a/./../../out"]) {
    assertEquals(within(ROOT, path), undefined, path);
  }
});
