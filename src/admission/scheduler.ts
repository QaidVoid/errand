/**
 * Bounds concurrent turns and live sessions, and absorbs provider backoff.
 *
 * Two independent caps, because the costs are independent. A session with a
 * turn in flight costs provider load; a session that merely exists costs
 * memory. One number would force a bad trade in both directions.
 *
 * A slot covers a whole turn rather than an individual model request, because
 * from outside the agent a request inside a tool loop is not visible. A turn is
 * the coarsest unit that can be observed and the finest that can be
 * controlled, so it is the unit.
 */

import type { LimitsConfig } from "../config/schema.ts";

/** Injected so tests drive time rather than wait for it. */
export interface Clock {
  now(): number;
  setTimeout(handler: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

const systemClock: Clock = {
  now: () => Date.now(),
  setTimeout: (handler: () => void, ms: number) => setTimeout(handler, ms),
  clearTimeout: (handle: unknown) => clearTimeout(handle as number),
};

/** Held for the duration of one turn, and released exactly once. */
export class Ticket {
  private releasedAt: number | null = null;

  constructor(readonly sessionId: string) {}

  /** True once this ticket has been released. */
  get isReleased(): boolean {
    return this.releasedAt !== null;
  }

  /** Marks the ticket spent. Returns false on a second release. */
  markReleased(at: number): boolean {
    if (this.releasedAt !== null) return false;
    this.releasedAt = at;
    return true;
  }
}

/** How a submitted prompt was disposed of. */
export type SubmitOutcome =
  | { status: "admitted"; ticket: Ticket }
  | { status: "queued"; position: number }
  | { status: "rejected"; reason: string };

/** A prompt waiting for a turn slot. */
export interface QueueEntry {
  sessionId: string;
  /** Called when a slot frees and the prompt should be sent. */
  onAdmitted: (ticket: Ticket) => void;
  /** Called when the prompt waited longer than the configured maximum. */
  onExpired: () => void;
  /** Called when the prompt's place in the queue changes. */
  onPositionChanged?: (position: number) => void;
}

interface PendingEntry extends QueueEntry {
  enqueuedAt: number;
  lastReportedPosition: number;
}

/** Why the scheduler is currently refusing to admit work. */
export type PauseReason = "provider backoff";

/** Bounds turns in flight, live sessions, and the queue between them. */
export class Scheduler {
  private inFlight = 0;
  private liveSessions = 0;
  private readonly queue: PendingEntry[] = [];

  private backoffUntil = 0;
  private backoffMs: number;
  private backoffTimer: unknown = null;
  private lastStartAt = 0;
  private sweepTimer: unknown = null;

  /**
   * @param baseBackoffMs First pause after the provider signals rate limiting.
   * @param maxBackoffMs Ceiling the pause grows to on recurrence.
   * @param startIntervalMs Minimum spacing between session starts, so a burst
   *   of messages does not launch every sandbox at the same instant.
   */
  constructor(
    private readonly limits: LimitsConfig,
    private readonly clock: Clock = systemClock,
    private readonly baseBackoffMs = 5_000,
    private readonly maxBackoffMs = 300_000,
    private readonly startIntervalMs = 750,
  ) {
    this.backoffMs = baseBackoffMs;
  }

  /** Sessions currently holding a slot for a running turn. */
  get turnsInFlight(): number {
    return this.inFlight;
  }

  /** Prompts currently waiting for a slot. */
  get queueLength(): number {
    return this.queue.length;
  }

  /** Sessions that exist, running a turn or not. */
  get sessions(): number {
    return this.liveSessions;
  }

  /** Set while the scheduler is refusing to admit work, with the reason. */
  get pausedBecause(): PauseReason | null {
    return this.clock.now() < this.backoffUntil ? "provider backoff" : null;
  }

  /** How long the current backoff will last, for reporting. */
  get backoffRemainingMs(): number {
    return Math.max(0, this.backoffUntil - this.clock.now());
  }

  /**
   * Reserves capacity for a new session.
   *
   * @returns how long to wait before starting, or null when the cap is reached.
   */
  reserveSession(): { delayMs: number } | null {
    if (this.liveSessions >= this.limits.maxLiveSessions) return null;
    this.liveSessions += 1;

    const now = this.clock.now();
    const earliest = this.lastStartAt + this.startIntervalMs;
    const delayMs = Math.max(0, earliest - now);
    this.lastStartAt = Math.max(now, earliest);
    return { delayMs };
  }

  /** Releases a session's reservation when it ends. */
  releaseSession(): void {
    if (this.liveSessions > 0) this.liveSessions -= 1;
  }

  /** Why a session was refused, in words worth posting back to the channel. */
  sessionRefusedReason(): string {
    return `the session limit of ${this.limits.maxLiveSessions} is reached, so this message did not start one`;
  }

  /** Submits a prompt, admitting it now or queueing it behind the cap. */
  submit(entry: QueueEntry): SubmitOutcome {
    if (this.canAdmitNow()) {
      this.inFlight += 1;
      return { status: "admitted", ticket: new Ticket(entry.sessionId) };
    }

    if (this.queue.length >= this.limits.maxQueueLength) {
      return {
        status: "rejected",
        reason:
          `the queue is full at ${this.limits.maxQueueLength} waiting prompts, so this message was not accepted`,
      };
    }

    const pending: PendingEntry = {
      ...entry,
      enqueuedAt: this.clock.now(),
      lastReportedPosition: this.queue.length + 1,
    };
    this.queue.push(pending);
    this.scheduleSweep();
    return { status: "queued", position: pending.lastReportedPosition };
  }

  /**
   * Releases a turn slot. Every exit path calls this: completion, abort,
   * error, sandbox death, and session termination.
   *
   * @returns false when the ticket was already released, which is a bug in the
   *   caller rather than something to absorb silently.
   */
  release(ticket: Ticket): boolean {
    if (!ticket.markReleased(this.clock.now())) return false;
    if (this.inFlight > 0) this.inFlight -= 1;
    this.pump();
    return true;
  }

  /** Drops a session's queued prompts when the session ends. */
  cancelSession(sessionId: string): number {
    let removed = 0;
    for (let index = this.queue.length - 1; index >= 0; index -= 1) {
      if (this.queue[index]?.sessionId !== sessionId) continue;
      this.queue.splice(index, 1);
      removed += 1;
    }
    if (removed > 0) this.reportPositions();
    return removed;
  }

  /**
   * Records that the provider signalled rate limiting.
   *
   * Admission stops globally, not just for the session that noticed. The limit
   * is per account, so a limit one session hit is information about all of
   * them, and backing off only the one that noticed leaves the rest pushing
   * into the same wall.
   */
  noteRateLimit(): void {
    const now = this.clock.now();
    if (now < this.backoffUntil) {
      this.backoffMs = Math.min(this.backoffMs * 2, this.maxBackoffMs);
    }
    this.backoffUntil = now + this.backoffMs;

    if (this.backoffTimer !== null) this.clock.clearTimeout(this.backoffTimer);
    this.backoffTimer = this.clock.setTimeout(() => {
      this.backoffTimer = null;
      this.pump();
    }, this.backoffMs);
  }

  /** Records a turn that completed without rate limiting, decaying the pause. */
  noteSuccess(): void {
    if (this.clock.now() < this.backoffUntil) return;
    this.backoffMs = Math.max(this.baseBackoffMs, Math.floor(this.backoffMs / 2));
  }

  /** Drops prompts that have waited longer than the configured maximum. */
  expireStale(): number {
    const cutoff = this.clock.now() - this.limits.maxQueueWaitMs;
    let expired = 0;
    for (let index = this.queue.length - 1; index >= 0; index -= 1) {
      const entry = this.queue[index];
      if (entry === undefined || entry.enqueuedAt > cutoff) continue;
      this.queue.splice(index, 1);
      expired += 1;
      entry.onExpired();
    }
    if (expired > 0) this.reportPositions();
    return expired;
  }

  /** Stops every timer, so the daemon can exit. */
  shutdown(): void {
    if (this.backoffTimer !== null) this.clock.clearTimeout(this.backoffTimer);
    if (this.sweepTimer !== null) this.clock.clearTimeout(this.sweepTimer);
    this.backoffTimer = null;
    this.sweepTimer = null;
    this.queue.length = 0;
  }

  private canAdmitNow(): boolean {
    return this.inFlight < this.limits.maxConcurrentTurns && this.pausedBecause === null;
  }

  private pump(): void {
    while (this.queue.length > 0 && this.canAdmitNow()) {
      const next = this.queue.shift();
      if (next === undefined) break;
      this.inFlight += 1;
      next.onAdmitted(new Ticket(next.sessionId));
    }
    this.reportPositions();
  }

  /**
   * Tells each waiting prompt its position, but only when it changed, so a
   * thread gets one message updated rather than a message per shuffle.
   */
  private reportPositions(): void {
    for (let index = 0; index < this.queue.length; index += 1) {
      const entry = this.queue[index];
      if (entry === undefined) continue;
      const position = index + 1;
      if (entry.lastReportedPosition === position) continue;
      entry.lastReportedPosition = position;
      entry.onPositionChanged?.(position);
    }
  }

  private scheduleSweep(): void {
    if (this.sweepTimer !== null) return;
    this.sweepTimer = this.clock.setTimeout(
      () => {
        this.sweepTimer = null;
        this.expireStale();
        if (this.queue.length > 0) this.scheduleSweep();
      },
      Math.max(1, Math.floor(this.limits.maxQueueWaitMs / 4)),
    );
  }
}
