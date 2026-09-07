import { assertEquals } from "@std/assert";
import { hostPathUnder, within } from "./paths.ts";

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

Deno.test("a path the agent sees becomes a path on the host", () => {
  assertEquals(hostPathUnder("/workspace", ROOT, "/workspace/src/a.ts"), "/projects/demo/src/a.ts");
  assertEquals(hostPathUnder("/workspace", ROOT, "src/a.ts"), "/projects/demo/src/a.ts");
  assertEquals(hostPathUnder("/workspace", ROOT, "/workspace"), ROOT);
});

/** A leading separator is not the host's root, or a tool call could read it. */
Deno.test("an absolute path outside the workspace is read as project-relative", () => {
  assertEquals(hostPathUnder("/workspace", ROOT, "/etc/passwd"), "/projects/demo/etc/passwd");
});

Deno.test("a path that climbs out of the project has no host path", () => {
  assertEquals(hostPathUnder("/workspace", ROOT, "/workspace/../../secrets"), undefined);
  assertEquals(hostPathUnder("/workspace", ROOT, "../secrets"), undefined);
  assertEquals(hostPathUnder("/workspace", ROOT, "   "), undefined);
});
