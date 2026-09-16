import { assertEquals } from "@std/assert";
import { join } from "@std/path";
import {
  prepareRecordDir,
  recordDir,
  transcriptPath,
  withdrawFromAgentSession,
  withdrawFromRecord,
} from "./record.ts";
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

/** A session's two directories, with a prompt worth taking back in each. */
async function withSession(
  run: (stateDir: string, recordDir: string, said: string) => Promise<void>,
): Promise<void> {
  const stateDir = await Deno.makeTempDir({ prefix: "errand-withdraw-" });
  const recordDir = `${stateDir}.record`;
  const said = "my token is ghp_TOPSECRET123";
  await Deno.mkdir(recordDir, { recursive: true });
  await Deno.mkdir(`${stateDir}/sessions`, { recursive: true });
  await Deno.writeTextFile(
    `${recordDir}/transcript.jsonl`,
    [
      JSON.stringify({ at: 1, entry: { call: "prompt", author: "a", text: "hello", id: "m-1" } }),
      JSON.stringify({ at: 2, entry: { call: "prompt", author: "a", text: said, id: "m-2" } }),
      JSON.stringify({ at: 3, entry: { call: "post", text: "sure, noted" } }),
    ].join("\n"),
  );
  await Deno.writeTextFile(
    `${stateDir}/sessions/s.jsonl`,
    [
      JSON.stringify({ type: "session", version: 3 }),
      JSON.stringify({
        type: "message",
        id: "a1",
        parentId: "root",
        message: { role: "user", content: [{ type: "text", text: `<context>\n${said}` }] },
      }),
      JSON.stringify({
        type: "message",
        id: "a2",
        parentId: "a1",
        message: { role: "assistant", content: [{ type: "text", text: "sure, noted" }] },
      }),
    ].join("\n"),
  );
  try {
    await run(stateDir, recordDir, said);
  } finally {
    await Deno.remove(stateDir, { recursive: true }).catch(() => {});
    await Deno.remove(recordDir, { recursive: true }).catch(() => {});
  }
}

/**
 * The words are the whole point: what a person deletes soonest is what they
 * should not have sent, and it lives in two files after the chat forgets it.
 */
Deno.test("a withdrawn message leaves both copies", () =>
  withSession(async (stateDir, recordDir, said) => {
    const removed = await withdrawFromRecord(stateDir, "m-2");
    assertEquals(removed, said, "the text is reported, because the agent knows no chat id");
    assertEquals(await withdrawFromAgentSession(stateDir, removed as string), true);

    const transcript = await Deno.readTextFile(`${recordDir}/transcript.jsonl`);
    assertEquals(transcript.includes("ghp_TOPSECRET123"), false);
    assertEquals(
      transcript.includes('"withdrawn": true') || transcript.includes('"withdrawn":true'),
      true,
    );
    // A transcript that quietly drops a turn reads as one that never had it.
    assertEquals(transcript.includes("hello"), true);
    assertEquals(transcript.includes("sure, noted"), true);

    const stored = await Deno.readTextFile(`${stateDir}/sessions/s.jsonl`);
    assertEquals(stored.includes("ghp_TOPSECRET123"), false);
    // The file is a chain by parentId; stranding the rest would be worse than
    // leaving the text.
    assertEquals(stored.includes('"parentId":"a1"') || stored.includes('"parentId": "a1"'), true);
    for (const line of stored.split("\n")) JSON.parse(line);
  }));

Deno.test("a message nobody sent here withdraws nothing", () =>
  withSession(async (stateDir) => {
    assertEquals(await withdrawFromRecord(stateDir, "not-a-message"), undefined);
    assertEquals(await withdrawFromAgentSession(stateDir, "words never said"), false);
  }));

/** Repairing the agent's file is not a withdrawal's business. */
Deno.test("a line that cannot be parsed is carried across untouched", () =>
  withSession(async (stateDir, _recordDir, said) => {
    const path = `${stateDir}/sessions/s.jsonl`;
    await Deno.writeTextFile(path, `${await Deno.readTextFile(path)}\n{ this is not json`);
    assertEquals(await withdrawFromAgentSession(stateDir, said), true);
    const stored = await Deno.readTextFile(path);
    assertEquals(stored.includes("{ this is not json"), true);
    assertEquals(stored.includes("ghp_TOPSECRET123"), false);
  }));
