import { assertEquals } from "@std/assert";
import { join } from "@std/path";
import { prepareRecordDir, recordDir, transcriptPath } from "./record.ts";
import { TRANSCRIPT_FILENAME } from "./transcript.ts";

/**
 * The state directory is handed to the session as `/state`, writable. What the
 * daemon records about a session must therefore not be inside it.
 */
Deno.test("the record sits beside the state directory, never inside it", () => {
  const state = "/var/lib/errand/abc";
  assertEquals(recordDir(state).startsWith(`${state}/`), false);
  assertEquals(recordDir(state), "/var/lib/errand/abc.record");
});

Deno.test("a transcript left in the older place is moved out of reach", async () => {
  const root = await Deno.makeTempDir({ prefix: "errand-record-" });
  const state = join(root, "session");
  try {
    Deno.mkdirSync(state, { recursive: true });
    const legacy = join(state, TRANSCRIPT_FILENAME);
    Deno.writeTextFileSync(legacy, '{"turn":1}\n');
    // Read before the move still finds the history.
    assertEquals(transcriptPath(state), legacy);

    prepareRecordDir(state);

    const placed = join(recordDir(state), TRANSCRIPT_FILENAME);
    assertEquals(transcriptPath(state), placed);
    assertEquals(Deno.readTextFileSync(placed), '{"turn":1}\n');
    // Gone from the directory the session can write.
    assertEquals(transcriptPath(state).startsWith(`${state}/`), false);
  } finally {
    await Deno.remove(root, { recursive: true });
  }
});
