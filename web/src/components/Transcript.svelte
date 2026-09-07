<script lang="ts">
  /**
   * A session, read as turns.
   *
   * A conversation in order. Search and filtering narrow what is shown without
   * changing what is there, and both say what they hide. Nothing folds: an
   * exchange is read where it falls, and only a tool call hides a detail.
   */
  import { untrack } from "svelte";
  import { type Lens, survives, through } from "../lib/lens.ts";
  import { countIn, needleOf } from "../lib/search.ts";
  import { read } from "../lib/turns.ts";
  import type { Entry } from "../lib/types.ts";
  import Turn from "./Turn.svelte";

  let {
    entries,
    busy,
    waiting,
    ended,
    dropped = 0,
    onOpenFile,
    onReading,
    wanted,
    search = "",
    lens = $bindable("all"),
    onFound,
  }: {
    entries: Entry[];
    busy: boolean;
    waiting: string | null;
    ended: boolean;
    dropped?: number;
    onOpenFile?: ((path: string) => void) | undefined;
    /**
     * Says which exchange is in view, and how many there are.
     *
     * Reported on scroll as well as on a move, because a reader who scrolls
     * has moved through the session just as surely as one who pressed a key.
     */
    onReading?: ((at: number, of: number) => void) | undefined;
    /**
     * An exchange named in the address, by its number.
     *
     * Applied once the session it belongs to has arrived, since a link is
     * opened before there is anything to scroll to.
     */
    wanted?: number | undefined;
    search?: string;
    /** What the session is narrowed to. */
    lens?: Lens;
    /** Says how many matches there are, so the search box can report them. */
    onFound?: ((matches: number, exchanges: number) => void) | undefined;
  } = $props();

  const reading = $derived(read(entries));

  /** Everything in a turn as one string, so a search can look at all of it. */
  function textOf(turn: (typeof reading.turns)[number]): string {
    const parts: string[] = [turn.asked?.text ?? ""];
    for (const item of turn.items) {
      if (item.kind === "call") {
        parts.push(item.call.entry.line, item.call.result?.output ?? "");
        for (const change of item.call.changes) parts.push(change.path, change.body);
        continue;
      }
      const entry = item.entry;
      if ("text" in entry) parts.push(entry.text);
      if (entry.kind === "aside") parts.push(entry.author);
      if (entry.kind === "delegation") {
        const { question, answer, refused, model, describes } = entry.delegated;
        parts.push(question, answer ?? "", refused ?? "", model ?? "", describes ?? "");
      }
      if (entry.kind === "diff") parts.push(entry.path, entry.body);
    }
    return parts.join("\n");
  }

  const needle = $derived(needleOf(search));

  /**
   * The exchanges to show, already narrowed.
   *
   * The lens takes items out of an exchange rather than taking the exchange
   * out of the session, so pressing one visibly changes what is in front of
   * the reader. An exchange left with nothing in it goes.
   */
  const shown = $derived.by(() =>
    reading.turns
      .map((turn, index) => ({ turn: through(lens, turn), index, whole: turn }))
      .filter(({ whole }) => survives(lens, whole))
      .filter(({ turn }) => needle.length === 0 || countIn(textOf(turn), needle) > 0)
  );

  /** How many times the term occurs in what is being shown. */
  const found = $derived(
    needle.length === 0
      ? 0
      : shown.reduce((total, { turn }) => total + countIn(textOf(turn), needle), 0),
  );

  $effect(() => {
    const matches = found;
    const exchanges = shown.length;
    untrack(() => onFound?.(matches, exchanges));
  });

  const hidden = $derived(reading.turns.length - shown.length);

  export function turnCount(): number {
    return reading.turns.length;
  }

  /**
   * Moves to the exchange with this number.
   *
   * By its own identity rather than by position, because the groups a session
   * holds and the exchanges a reader can reach are not the same list: a
   * session opens with notices that belong to no exchange, and a filter hides
   * some of the rest.
   */
  export function goToNumber(number: number): boolean {
    const mark = pane?.querySelector(`#turn-${number}`);
    if (mark === undefined || mark === null) return false;
    mark.scrollIntoView({ block: "start" });
    queueMicrotask(report);
    return true;
  }

  /** Moves to an exchange, which is how a reader reaches one directly. */
  export function goTo(index: number): void {
    const reachable = pane?.querySelectorAll(".turn.numbered").length ?? 0;
    if (reachable === 0) return;
    const bounded = Math.max(0, Math.min(reachable - 1, index));
    at = bounded;
    queueMicrotask(() => {
      const marks = pane?.querySelectorAll(".turn.numbered");
      const mark = marks?.[bounded];
      if (mark === undefined) pane?.scrollTo({ top: 0 });
      else mark.scrollIntoView({ block: "start" });
      onReading?.(bounded + 1, marks?.length ?? 0);
    });
  }

  /** The turn a reader is on, so next and previous mean something. */
  let at = $state(0);

  /** The exchange the address named, once it has been found and shown. */
  let linked = $state<number | null>(null);
  let applied = $state<number | null>(null);

  /*
   * An address names an exchange that may not exist: a link to a turn beyond
   * the end of the session, or one from a session that was compacted. It is
   * left alone in that case, which leaves the session opening where it
   * normally would.
   */
  $effect(() => {
    const asked = wanted;
    const turns = reading.turns;
    untrack(() => {
      if (asked === undefined || applied === asked || turns.length === 0) return;
      applied = asked;
      // Marked before it is reached, so the element exists to be scrolled to.
      linked = asked;
      queueMicrotask(() => {
        if (goToNumber(asked)) pinned = false;
        else linked = null;
      });
    });
  });

  export function step(by: number): void {
    goTo(at + by);
  }

  /** Instructs every tool call at once, which is separate from the turns. */
  let command = $state({ open: false, at: 0 });

  export function allCalls(open: boolean): void {
    command = { open, at: command.at + 1 };
  }

  let pane = $state<HTMLElement | null>(null);
  let pinned = $state(true);

  /** Only follow the tail when the reader is already at it. */
  function onScroll(): void {
    if (pane === null) return;
    pinned = pane.scrollHeight - pane.scrollTop - pane.clientHeight < 80;
    report();
  }

  /**
   * The exchange at the top of the view, which is the one being read.
   *
   * Measured from the pane rather than tracked, so that scrolling, jumping,
   * and following the tail all agree without three ways of counting.
   */
  function report(): void {
    if (pane === null) return;
    const marks = [...pane.querySelectorAll(".turn.numbered")];
    if (marks.length === 0) {
      onReading?.(0, 0);
      return;
    }
    const top = pane.getBoundingClientRect().top;
    let seen = 0;
    marks.forEach((mark, index) => {
      if (mark.getBoundingClientRect().top - top <= 1) seen = index;
    });
    at = seen;
    onReading?.(seen + 1, marks.length);
  }

  export function toEnd(): void {
    pinned = true;
    if (pane !== null) pane.scrollTop = pane.scrollHeight;
  }

  /** Output has arrived that the reader has scrolled away from. */
  let arrived = $state(false);
  let counted = $state(0);

  $effect(() => {
    const count = entries.length;
    untrack(() => {
      if (count === counted) return;
      counted = count;
      if (pinned && pane !== null) pane.scrollTop = pane.scrollHeight;
      else if (!pinned) arrived = true;
    });
  });

  $effect(() => {
    const following = pinned;
    untrack(() => {
      if (following && arrived) arrived = false;
    });
  });
</script>

<div class="wrap">
  <div class="pane" bind:this={pane} onscroll={onScroll}>
    {#if !reading.grouped && reading.turns.length > 0}
      <p class="note">
        This session was recorded before turns were kept, so it reads in order rather than grouped.
      </p>
    {/if}

    {#if dropped > 0}
      <p class="note">{dropped} earlier line(s) were not kept.</p>
    {/if}

    {#if reading.turns.length === 0}
      <p class="note empty">
        Nothing yet. What the agent says, runs, and changes appears here as it happens.
      </p>
    {:else if shown.length === 0}
      <p class="note">
        {needle.length > 0 ? `Nothing here says "${search.trim()}".` : "Nothing to show under this filter."}
        Clear it to read the whole session.
      </p>
    {/if}

    {#each shown as { turn, index } (index)}
      <Turn
        {turn}
        linked={turn.number !== undefined && turn.number === linked}
        running={busy && waiting === null && index === reading.turns.length - 1}
        waiting={waiting !== null && index === reading.turns.length - 1}
        {needle}
        {onOpenFile}
        {command}
      />
    {/each}

    {#if hidden > 0}
      <p class="note">
        {hidden} exchange{hidden === 1 ? "" : "s"} not shown{needle.length > 0
          ? " by this search"
          : " by this filter"}.
      </p>
    {/if}

    {#if waiting !== null}
      <p class="note">{waiting}</p>
    {/if}

    {#if ended}
      <p class="note ended">This session has ended.</p>
    {/if}
  </div>

  {#if arrived}
    <button class="behind" onclick={toEnd}>New output below</button>
  {/if}
</div>

<style>
  .wrap {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .pane {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 14px 18px 24px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    scroll-behavior: smooth;
  }

  .note {
    margin: 0;
    color: var(--text-faint);
    font-size: 12.5px;
  }

  .note.empty {
    margin: auto;
    max-width: 42ch;
    text-align: center;
  }

  .note.ended {
    border-top: 1px solid var(--line-soft);
    padding-top: 12px;
  }

  .behind {
    position: absolute;
    left: 50%;
    bottom: 14px;
    transform: translateX(-50%);
    padding: 6px 14px;
    border-radius: 999px;
    background: var(--accent);
    color: var(--accent-ink);
    font-size: 12px;
    font-weight: 600;
    box-shadow: 0 4px 14px var(--overlay);
  }

  @media (max-width: 720px) {
    .pane {
      padding: 12px 10px 20px;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .pane {
      scroll-behavior: auto;
    }
  }
</style>
