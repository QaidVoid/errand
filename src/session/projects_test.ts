import { assertEquals, assertThrows } from "@std/assert";
import { join } from "@std/path";
import {
  ensureProjectDirectory,
  isValidProjectName,
  ProjectEscapeError,
  selectProject,
} from "./projects.ts";

Deno.test("a leading name picks the project and leaves the rest as the prompt", () => {
  const chosen = selectProject("errand: fix the failing test", "/projects", "loose");

  assertEquals(chosen.name, "errand");
  assertEquals(chosen.path, join("/projects", "errand"));
  assertEquals(chosen.prompt, "fix the failing test");
  assertEquals(chosen.wasExplicit, true);
});

Deno.test("a message with no prefix works in the fallback project", () => {
  const chosen = selectProject("  just do the thing  ", "/projects", "session-7");

  assertEquals(chosen.name, "session-7");
  assertEquals(chosen.prompt, "just do the thing");
  assertEquals(chosen.wasExplicit, false);
});

/**
 * A name that does not validate has to stay prompt text. Treating it as a
 * selection anyway is how a malformed name redirects a session instead of
 * merely failing to choose one.
 */
Deno.test("a prefix that is not a valid name is ordinary text", () => {
  for (const text of ["..: escape", ".hidden: go", "a b: spaced"]) {
    const chosen = selectProject(text, "/projects", "loose");
    assertEquals(chosen.wasExplicit, false, text);
    assertEquals(chosen.name, "loose", text);
    assertEquals(chosen.prompt, text.trim(), text);
  }
});

/** A URL is the everyday case of a colon that names nothing. */
Deno.test("a colon inside a sentence does not select a project", () => {
  const chosen = selectProject("read https://example.com/x and say what it does", "/p", "loose");

  assertEquals(chosen.wasExplicit, false);
  assertEquals(chosen.prompt, "read https://example.com/x and say what it does");
});

Deno.test("a name is one path segment and never reaches for another", () => {
  assertEquals(isValidProjectName("errand"), true);
  assertEquals(isValidProjectName("errand.v2_final-1"), true);
  for (const bad of ["", ".", "..", ".git", "a/b", "a\\b", "-lead", "x".repeat(65)]) {
    assertEquals(isValidProjectName(bad), false, bad);
  }
});

Deno.test("the project directory is created under the root", async () => {
  const root = await Deno.makeTempDir({ prefix: "errand-projects-" });
  try {
    const chosen = selectProject("demo: go", root, "loose");
    ensureProjectDirectory(chosen, root);

    assertEquals(Deno.statSync(chosen.path).isDirectory, true);
    ensureProjectDirectory(chosen, root);
  } finally {
    await Deno.remove(root, { recursive: true });
  }
});

/**
 * The name is a clean segment and the check still has to fail: only the
 * resolved path shows that the directory is a symlink out of the root.
 */
Deno.test("a project that is a symlink out of the root is refused", async () => {
  const root = await Deno.makeTempDir({ prefix: "errand-projects-" });
  const elsewhere = await Deno.makeTempDir({ prefix: "errand-elsewhere-" });
  try {
    Deno.symlinkSync(elsewhere, join(root, "escapee"));
    const chosen = selectProject("escapee: go", root, "loose");

    assertThrows(() => ensureProjectDirectory(chosen, root), ProjectEscapeError);
  } finally {
    await Deno.remove(root, { recursive: true });
    await Deno.remove(elsewhere, { recursive: true });
  }
});

Deno.test("a message that opens with a link is a prompt, not a project called https", () => {
  // Reported: the first such message made a shared `https` project with a live
  // session, and every later link-first message was refused because that
  // project was busy.
  const selection = selectProject(
    "https://github.com/QaidVoid/errand look at this",
    "/srv/projects",
    "s-1",
  );

  assertEquals(selection.wasExplicit, false);
  assertEquals(selection.name, "s-1");
  assertEquals(selection.prompt, "https://github.com/QaidVoid/errand look at this");
});

Deno.test("every scheme is left alone, not just https", () => {
  for (const url of ["http://x.dev", "ssh://git@x.dev/r", "ftp://x.dev", "file:///tmp/x"]) {
    const selection = selectProject(url, "/srv/projects", "fallback");
    assertEquals(selection.wasExplicit, false, url);
    assertEquals(selection.prompt, url);
  }
});

Deno.test("a name is still a name when slashes are not what follows the colon", () => {
  // The fix keys on `://`, so a prompt whose text merely begins with slashes
  // after a space still selects its project.
  const selection = selectProject("notes: //TODO tidy this up", "/srv/projects", "fallback");

  assertEquals(selection.wasExplicit, true);
  assertEquals(selection.name, "notes");
  assertEquals(selection.prompt, "//TODO tidy this up");
});

Deno.test("an ordinary project prefix is unaffected", () => {
  const selection = selectProject("errand: add a test", "/srv/projects", "fallback");

  assertEquals(selection.wasExplicit, true);
  assertEquals(selection.name, "errand");
  assertEquals(selection.prompt, "add a test");
});
