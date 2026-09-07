import { assertEquals, assertThrows } from "@std/assert";
import { join } from "@std/path";
import {
  languageFor,
  looksBinary,
  NotAFileError,
  readDirectory,
  readFileForDisplay,
} from "./files.ts";

async function withProject(run: (root: string) => void | Promise<void>): Promise<void> {
  const root = await Deno.makeTempDir({ prefix: "errand-files-" });
  try {
    await run(root);
  } finally {
    await Deno.remove(root, { recursive: true });
  }
}

Deno.test("a directory lists directories first, then by name", () =>
  withProject((root) => {
    Deno.mkdirSync(join(root, "src"));
    Deno.mkdirSync(join(root, "docs"));
    Deno.writeTextFileSync(join(root, "readme.md"), "hello");
    Deno.writeTextFileSync(join(root, "deno.json"), "{}");

    const entries = readDirectory(root, "");

    assertEquals(entries.map((entry) => entry.name), ["docs", "src", "deno.json", "readme.md"]);
    assertEquals(entries[0]?.directory, true);
    assertEquals(entries[3]?.size, 5);
  }));

/** A surface asks for a path again, so the path it was given has to work. */
Deno.test("each entry carries the path to ask for it by", () =>
  withProject((root) => {
    Deno.mkdirSync(join(root, "src"));
    Deno.writeTextFileSync(join(root, "src", "main.ts"), "");

    assertEquals(readDirectory(root, "")[0]?.path, "src");
    assertEquals(readDirectory(join(root, "src"), "src")[0]?.path, "src/main.ts");
  }));

Deno.test("an empty directory lists nothing rather than failing", () =>
  withProject((root) => {
    assertEquals(readDirectory(root, ""), []);
  }));

Deno.test("a text file is read with the language to show it in", () =>
  withProject((root) => {
    Deno.writeTextFileSync(join(root, "main.ts"), "const x = 1;\n");

    const contents = readFileForDisplay(join(root, "main.ts"), "main.ts");

    assertEquals(contents.text, "const x = 1;\n");
    assertEquals(contents.language, "ts");
    assertEquals(contents.binary, false);
    assertEquals(contents.truncated, false);
    assertEquals(contents.size, 13);
  }));

/** Pasting an image into a thread as mojibake helps nobody. */
Deno.test("a file with a NUL byte is reported as binary, not as text", () =>
  withProject((root) => {
    Deno.writeFileSync(join(root, "logo.png"), new Uint8Array([0x89, 0x50, 0, 0x1a]));

    const contents = readFileForDisplay(join(root, "logo.png"), "logo.png");

    assertEquals(contents.binary, true);
    assertEquals(contents.text, "");
  }));

Deno.test("a long file is cut, and says the whole size it was cut from", () =>
  withProject((root) => {
    Deno.writeTextFileSync(join(root, "big.txt"), "x".repeat(5_000));

    const contents = readFileForDisplay(join(root, "big.txt"), "big.txt", 100);

    assertEquals(contents.truncated, true);
    assertEquals(contents.text.length, 100);
    assertEquals(contents.size, 5_000);
  }));

/**
 * A limit counted in bytes can land inside a character. Decoding the half of
 * it that was read would end the shown text with a replacement mark.
 */
Deno.test("a cut landing inside a character does not show half of one", () =>
  withProject((root) => {
    Deno.writeTextFileSync(join(root, "wide.txt"), "ab\u{1F50C}cd");

    const contents = readFileForDisplay(join(root, "wide.txt"), "wide.txt", 4);

    assertEquals(contents.text, "ab");
    assertEquals(contents.text.includes("\uFFFD"), false);
  }));

Deno.test("a directory asked for as a file says so rather than reading it", () =>
  withProject((root) => {
    Deno.mkdirSync(join(root, "src"));

    assertThrows(() => readFileForDisplay(join(root, "src"), "src"), NotAFileError);
  }));

Deno.test("an extension picks the language, and an unknown one picks none", () => {
  assertEquals(languageFor("src/main.ts"), "ts");
  assertEquals(languageFor("Makefile"), "");
  assertEquals(languageFor("notes.MD"), "md");
});

Deno.test("only a NUL in the first block makes content binary", () => {
  assertEquals(looksBinary(new TextEncoder().encode("plain text")), false);
  assertEquals(looksBinary(new Uint8Array([0])), true);
  const late = new Uint8Array(9_000).fill(0x61);
  late[8_500] = 0;
  assertEquals(looksBinary(late), false);
});
