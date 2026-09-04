import { assertEquals, assertThrows } from "@std/assert";
import { LineFramer, RecordTooLargeError } from "./framing.ts";

const encoder = new TextEncoder();
const bytes = (text: string): Uint8Array => encoder.encode(text);

Deno.test("a record split across reads is reassembled", () => {
  const framer = new LineFramer();

  assertEquals(framer.push(bytes('{"type":"pro')), []);
  assertEquals(framer.push(bytes('mpt"}\n')), ['{"type":"prompt"}']);
});

Deno.test("several records in one chunk all come out, in order", () => {
  const framer = new LineFramer();
  assertEquals(framer.push(bytes("one\ntwo\nthree\n")), ["one", "two", "three"]);
});

Deno.test("chunking does not change what comes out", () => {
  const whole = 'a\n{"b":1}\nc\n';
  const expected = ["a", '{"b":1}', "c"];

  for (const size of [1, 2, 3, 5, 11]) {
    const framer = new LineFramer();
    const records: string[] = [];
    const raw = bytes(whole);
    for (let at = 0; at < raw.length; at += size) {
      records.push(...framer.push(raw.subarray(at, at + size)));
    }
    assertEquals(records, expected, `chunked by ${size}`);
  }
});

Deno.test("a carriage return before the line feed is not part of the record", () => {
  const framer = new LineFramer();
  assertEquals(framer.push(bytes("one\r\ntwo\r\n")), ["one", "two"]);
});

/**
 * The reason framing works on bytes: these are legal inside a JSON string and
 * every general purpose line reader treats them as newlines, so a record
 * containing one would be split in half by a text-based reader.
 */
Deno.test("a line separator inside a string is not a record boundary", () => {
  const framer = new LineFramer();
  const record = `{"text":"before\u2028after\u2029end"}`;

  assertEquals(record.includes("\u2028"), true, "it must really contain one");
  assertEquals(framer.push(bytes(`${record}\n`)), [record]);
});

Deno.test("a multi-byte character split across chunks survives", () => {
  const framer = new LineFramer();
  const raw = bytes(`{"text":"\u{1F50C}"}\n`);

  const first = framer.push(raw.subarray(0, 11));
  const second = framer.push(raw.subarray(11));

  assertEquals(first, []);
  assertEquals(second, [`{"text":"\u{1F50C}"}`]);
});

Deno.test("an unterminated record past the ceiling is refused, not buffered", () => {
  const framer = new LineFramer(16);

  assertThrows(() => framer.push(bytes("x".repeat(17))), RecordTooLargeError);
});

Deno.test("what is held for an unfinished record can be seen", () => {
  const framer = new LineFramer();
  framer.push(bytes("half"));
  assertEquals(framer.pendingBytes, 4);

  framer.push(bytes("\n"));
  assertEquals(framer.pendingBytes, 0);
});
