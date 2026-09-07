import { assertEquals, assertStringIncludes } from "@std/assert";
import { join } from "@std/path";
import type { RawAttachment } from "../chat/inbound.ts";
import { ATTACHMENTS_DIR, isImage, receive } from "./attachments.ts";

const LIMITS = { maxBytes: 1_000, maxCount: 3 };

function sent(name: string, size = 4, contentType?: string): RawAttachment {
  return { id: "1", name, url: `https://files.example/${name}`, size, contentType };
}

const CONTENT = new TextEncoder().encode("hello");
const serve = (): Promise<Uint8Array> => Promise.resolve(CONTENT);

async function withProject(run: (root: string) => Promise<void>): Promise<void> {
  const root = await Deno.makeTempDir({ prefix: "errand-attachments-" });
  try {
    await run(root);
  } finally {
    await Deno.remove(root, { recursive: true });
  }
}

Deno.test("a file is written under the project, in a directory of its own", () =>
  withProject(async (root) => {
    const outcome = await receive([sent("notes.txt")], root, LIMITS, serve);

    assertEquals(outcome.refused, []);
    assertEquals(outcome.taken[0]?.path, "attachments/notes.txt");
    assertEquals(Deno.readTextFileSync(join(root, ATTACHMENTS_DIR, "notes.txt")), "hello");
  }));

/** The name is a label. Anything that could make it a location is removed. */
Deno.test("a name aiming out of the project lands inside it anyway", () =>
  withProject(async (root) => {
    const outcome = await receive([sent("../../etc/passwd")], root, LIMITS, serve);

    assertEquals(outcome.taken[0]?.path, "attachments/_.._etc_passwd");
    assertEquals(Array.from(Deno.readDirSync(join(root, ATTACHMENTS_DIR))).length, 1);
  }));

Deno.test("a name that is nothing but punctuation still gets a file", () =>
  withProject(async (root) => {
    const outcome = await receive([sent("...")], root, LIMITS, serve);

    assertEquals(outcome.taken[0]?.path, "attachments/attachment");
  }));

/** Being helpful must never overwrite the work. */
Deno.test("a second file of the same name is numbered, not written over", () =>
  withProject(async (root) => {
    const outcome = await receive(
      [sent("notes.txt"), sent("notes.txt"), sent("notes.txt")],
      root,
      LIMITS,
      serve,
    );

    assertEquals(outcome.taken.map((file) => file.path), [
      "attachments/notes.txt",
      "attachments/notes-2.txt",
      "attachments/notes-3.txt",
    ]);
  }));

Deno.test("what is too large is refused by its claimed size, before it is fetched", () =>
  withProject(async (root) => {
    let fetched = 0;
    const outcome = await receive([sent("huge.bin", 9_999)], root, LIMITS, () => {
      fetched += 1;
      return serve();
    });

    assertEquals(fetched, 0);
    assertStringIncludes(outcome.refused[0]?.reason ?? "", "1000 byte limit");
  }));

/** The size is a claim until the bytes are in hand, so it is checked twice. */
Deno.test("a file that arrives larger than it claimed is still refused", () =>
  withProject(async (root) => {
    const outcome = await receive(
      [sent("small.bin", 4)],
      root,
      LIMITS,
      () => Promise.resolve(new Uint8Array(5_000)),
    );

    assertEquals(outcome.taken, []);
    assertStringIncludes(outcome.refused[0]?.reason ?? "", "byte limit");
    assertEquals(Array.from(Deno.readDirSync(root)).length, 0);
  }));

Deno.test("more files than allowed leaves the ones that fit", () =>
  withProject(async (root) => {
    const many = ["a.txt", "b.txt", "c.txt", "d.txt"].map((name) => sent(name));

    const outcome = await receive(many, root, LIMITS, serve);

    assertEquals(outcome.taken.length, 3);
    assertStringIncludes(outcome.refused[0]?.reason ?? "", "more than 3 file(s)");
  }));

/** One file failing must not lose the message, or the files beside it. */
Deno.test("a file that cannot be fetched is reported and the rest are kept", () =>
  withProject(async (root) => {
    const outcome = await receive(
      [sent("gone.txt"), sent("here.txt")],
      root,
      LIMITS,
      (url) => url.includes("gone") ? Promise.reject(new Error("404")) : serve(),
    );

    assertEquals(outcome.taken.map((file) => file.path), ["attachments/here.txt"]);
    assertStringIncludes(outcome.refused[0]?.reason ?? "", "could not be fetched");
  }));

Deno.test("an image is recognised by what it says it is, or by its name", () => {
  assertEquals(isImage("image/png", "screenshot"), true);
  assertEquals(isImage(undefined, "screenshot.JPEG"), true);
  assertEquals(isImage(undefined, "notes.txt"), false);
  assertEquals(isImage("application/octet-stream", "logo.webp"), true);
});
