import { assertEquals } from "@std/assert";
import { createLogger, type LogLevel } from "../log.ts";
import { OPENING_SCAN_BYTES, Transcript } from "./transcript.ts";

async function withTranscript(
  run: (transcript: Transcript, path: string, lines: [LogLevel, string][]) => void | Promise<void>,
): Promise<void> {
  const root = await Deno.makeTempDir({ prefix: "errand-transcript-" });
  const path = `${root}/transcript.jsonl`;
  const lines: [LogLevel, string][] = [];
  try {
    await run(new Transcript(path, createLogger({}, (l, m) => lines.push([l, m]))), path, lines);
  } finally {
    await Deno.remove(root, { recursive: true });
  }
}

Deno.test("what is appended comes back, in order, with its turn", () =>
  withTranscript((transcript) => {
    transcript.append({ call: "post", text: "first" }, 1, 1_000);
    transcript.append({ call: "activity", line: "ran something" }, 2, 2_000);

    const stored = transcript.read();

    assertEquals(stored.entries.length, 2);
    assertEquals(stored.entries[0]?.turn, 1);
    assertEquals(stored.entries[0]?.at, 1_000);
    assertEquals(stored.entries[1]?.entry.call, "activity");
    assertEquals(stored.dropped, 0);
  }));

Deno.test("a session with no transcript reads empty rather than failing", () =>
  withTranscript((_transcript, path) => {
    const missing = new Transcript(`${path}.nowhere`);
    assertEquals(missing.read(), { entries: [], dropped: 0 });
    assertEquals(missing.opening(), undefined);
  }));

/** A crash mid-write tears the last line; the rest of the session survives. */
Deno.test("a torn line is skipped and everything around it is kept", () =>
  withTranscript((transcript, path) => {
    transcript.append({ call: "post", text: "before" }, 1, 1_000);
    Deno.writeTextFileSync(path, '{"at":2000,"turn":1,"entry":{"call":"po', { append: true });
    Deno.writeTextFileSync(path, "\n", { append: true });
    transcript.append({ call: "post", text: "after" }, 1, 3_000);

    const stored = transcript.read();

    assertEquals(stored.entries.map((held) => held.entry), [
      { call: "post", text: "before" },
      { call: "post", text: "after" },
    ]);
  }));

Deno.test("anything that is not an entry is ignored", () =>
  withTranscript((transcript, path) => {
    Deno.writeTextFileSync(path, '"a bare string"\n{"no":"entry"}\n[]\n\n', { append: true });
    transcript.append({ call: "post", text: "real" }, 1, 1_000);

    assertEquals(transcript.read().entries.length, 1);
  }));

Deno.test("only the most recent are read back, and the gap is admitted", () =>
  withTranscript((transcript) => {
    for (let index = 0; index < 10; index += 1) {
      transcript.append({ call: "post", text: `line ${index}` }, 1, index);
    }

    const stored = transcript.read(4);

    assertEquals(stored.entries.length, 4);
    assertEquals(stored.dropped, 6);
    assertEquals(stored.entries[0]?.entry, { call: "post", text: "line 6" });
  }));

/** A transcript written before turns were kept still reads. */
Deno.test("an entry with no turn is still an entry", () =>
  withTranscript((transcript, path) => {
    Deno.writeTextFileSync(
      path,
      `${JSON.stringify({ at: 1, entry: { call: "post", text: "old" } })}\n`,
      { append: true },
    );

    const stored = transcript.read();
    assertEquals(stored.entries[0]?.turn, undefined);
    assertEquals(stored.entries[0]?.entry, { call: "post", text: "old" });
  }));

Deno.test("what a session was first asked is read from its transcript", () =>
  withTranscript((transcript) => {
    transcript.append({ call: "notice", text: "ready", level: "started" }, 0, 1_000);
    transcript.append({ call: "prompt", author: "amelia", text: "  cache the secret  " }, 1, 2_000);
    transcript.append({ call: "prompt", author: "amelia", text: "and then this" }, 2, 3_000);

    assertEquals(transcript.opening(), "cache the secret");
  }));

Deno.test("a session that was never asked anything has no opening", () =>
  withTranscript((transcript) => {
    transcript.append({ call: "notice", text: "started", level: "started" }, 0, 1_000);
    transcript.append({ call: "notice", text: "it failed", level: "ended" }, 0, 2_000);

    assertEquals(transcript.opening(), undefined);
  }));

/**
 * Proof that the scan is bounded: a prompt past the head is not found. It is
 * read once per stopped session whenever they are listed, so reading the whole
 * file would cost the length of every session.
 *
 * A read landing mid-line must also not hand a half-entry to the parser.
 */
Deno.test("only the head is scanned, cut back to the last whole line", () =>
  withTranscript((transcript) => {
    const filler = "y".repeat(OPENING_SCAN_BYTES);
    transcript.append({ call: "post", text: filler }, 0, 1_000);
    transcript.append({ call: "prompt", author: "amelia", text: "after the big one" }, 1, 2_000);

    // The prompt is past the scanned head, so it is not found, and nothing
    // half-read is mistaken for an entry.
    assertEquals(transcript.opening(), undefined);
  }));

Deno.test("a transcript that cannot be written says so and does not throw", () =>
  withTranscript((_transcript, path, lines) => {
    const blocked = new Transcript(
      `${path}/nested/transcript.jsonl`,
      createLogger({}, (level, line) => lines.push([level, line])),
    );

    blocked.append({ call: "post", text: "nowhere to go" }, 1, 1_000);

    assertEquals(lines.length, 1);
    assertEquals(lines[0]?.[0], "warn");
  }));
