/**
 * Per-thread ordered work queue that survives a disconnect.
 *
 * Acting straight from an event handler loses ordering the moment two actions
 * overlap, and loses output entirely across a reconnect. Everything a session
 * does to its thread goes through one of these instead: a single chain per
 * thread, buffered while the connection is down, and bounded so a disconnected
 * daemon cannot grow without limit.
 *
 * The queue holds tasks rather than strings because a thread both sends new
 * messages and edits the one it is building up, and those must stay in order
 * with respect to each other.
 *
 * Rate limiting is left to the chat library, which already queues and
 * respects the retry interval. This exists for ordering, for surviving a
 * disconnect, and for bounding memory.
 */

import type { Logger } from "../log.ts";

/** One unit of work against a thread. Throws to signal a retryable failure. */
export type OutboxTask = () => Promise<void>;

/** How many tasks a disconnected outbox holds before it starts dropping. */
export const DEFAULT_MAX_BUFFERED = 500;

/** An ordered, buffered work queue for one thread. */
export class Outbox {
  private readonly buffer: OutboxTask[] = [];
  private connected = true;
  private draining: Promise<void> | null = null;
  private dropped = 0;
  private announced = 0;
  private closed = false;

  /**
   * @param announceDrops Reports a gap to the thread, so a truncated
   *   conversation is never passed off as a complete one.
   */
  constructor(
    private readonly log: Logger,
    private readonly announceDrops: (count: number) => Promise<void>,
    private readonly maxBuffered: number = DEFAULT_MAX_BUFFERED,
  ) {}

  /** Tasks waiting to run. */
  get pending(): number {
    return this.buffer.length;
  }

  /** True once closed, after which nothing more will ever be sent. */
  get isClosed(): boolean {
    return this.closed;
  }

  /** Tasks discarded because the buffer was full. */
  get droppedCount(): number {
    return this.dropped;
  }

  /** Queues a task. Order is preserved against every other queued task. */
  enqueue(task: OutboxTask): void {
    if (this.closed) return;

    // Drop oldest: the newest output is the part a reader still cares about.
    // The count is kept outside the buffer so announcing a gap can never itself
    // push the buffer over its bound.
    while (this.buffer.length >= this.maxBuffered) {
      this.buffer.shift();
      this.dropped += 1;
    }

    this.buffer.push(task);
    void this.drain();
  }

  /**
   * Marks the gateway up or down. While down, tasks buffer instead of being
   * attempted, and the order they were queued in is preserved for the flush.
   */
  setConnected(connected: boolean): void {
    const wasConnected = this.connected;
    this.connected = connected;
    if (connected && !wasConnected) void this.drain();
  }

  /**
   * Waits for everything queued so far to run.
   *
   * Awaits a drain already in progress rather than starting a second one, so a
   * caller cannot observe an empty-looking outbox while a drain is mid-flight.
   */
  async flush(): Promise<void> {
    while (this.draining !== null) await this.draining;
    await this.drain();
  }

  /** Stops accepting work and discards anything still queued. */
  close(): void {
    this.closed = true;
    this.buffer.length = 0;
  }

  private drain(): Promise<void> {
    if (this.draining !== null) return this.draining;
    if (!this.connected) return Promise.resolve();

    const run = this.drainLoop().finally(() => {
      this.draining = null;
    });
    this.draining = run;
    return run;
  }

  private async drainLoop(): Promise<void> {
    while (this.connected && (this.buffer.length > 0 || this.dropped > this.announced)) {
      if (this.dropped > this.announced) {
        const gap = this.dropped - this.announced;
        try {
          await this.announceDrops(gap);
          this.announced = this.dropped;
        } catch (error) {
          this.log.warn("reporting dropped messages failed", { detail: String(error) });
          this.connected = false;
        }
        continue;
      }

      const next = this.buffer[0];
      if (next === undefined) break;
      try {
        await next();
        this.buffer.shift();
      } catch (error) {
        // Left at the head so ordering holds when the gateway returns.
        this.log.warn("a thread action failed", { detail: String(error) });
        this.connected = false;
      }
    }
  }
}
