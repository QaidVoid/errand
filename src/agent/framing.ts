/**
 * Line-feed-only record framing for the agent protocol.
 *
 * The agent's protocol uses the line feed as its only record delimiter. Framing
 * therefore happens on bytes, splitting on 0x0A and decoding each record
 * afterwards. Splitting decoded text instead would be wrong: U+2028 and U+2029
 * are legal inside JSON strings, and every general purpose line reader treats
 * them as newlines. That bug only shows up when an agent message happens to
 * contain one, so it is designed out rather than tested for.
 */

const LINE_FEED = 0x0a;
const CARRIAGE_RETURN = 0x0d;

/** Raised when a record grows past the configured ceiling without terminating. */
export class RecordTooLargeError extends Error {
  constructor(readonly limit: number) {
    super(`agent sent a record longer than ${limit} bytes without a line feed`);
    this.name = "RecordTooLargeError";
  }
}

/**
 * Accumulates bytes and yields complete records.
 *
 * Feeding the same bytes in any chunking yields the same records, so a record
 * split across reads is reassembled rather than lost.
 */
export class LineFramer {
  private buffer: Uint8Array = new Uint8Array(0);
  private readonly decoder = new TextDecoder("utf-8");

  /**
   * @param maxRecordBytes Ceiling on one unterminated record. A stream that
   *   exceeds it is a protocol violation, not something to buffer forever.
   */
  constructor(private readonly maxRecordBytes: number = 8 * 1024 * 1024) {}

  /**
   * Feeds a chunk and returns every record it completed.
   *
   * @throws RecordTooLargeError when the pending record exceeds the ceiling.
   */
  push(chunk: Uint8Array): string[] {
    const combined = new Uint8Array(this.buffer.length + chunk.length);
    combined.set(this.buffer, 0);
    combined.set(chunk, this.buffer.length);

    const records: string[] = [];
    let start = 0;

    for (let i = 0; i < combined.length; i += 1) {
      if (combined[i] !== LINE_FEED) continue;
      let end = i;
      if (end > start && combined[end - 1] === CARRIAGE_RETURN) end -= 1;
      records.push(this.decoder.decode(combined.subarray(start, end)));
      start = i + 1;
    }

    this.buffer = combined.subarray(start);
    if (this.buffer.length > this.maxRecordBytes) {
      throw new RecordTooLargeError(this.maxRecordBytes);
    }
    return records;
  }

  /** Bytes held for a record that has not terminated yet. */
  get pendingBytes(): number {
    return this.buffer.length;
  }
}
