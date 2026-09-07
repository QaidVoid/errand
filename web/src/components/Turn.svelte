<script lang="ts">
  /**
   * One exchange in a conversation.
   *
   * A separator and then what happened, rather than a box around it. A
   * conversation is read in order, so nothing here folds: what folds is a tool
   * call, which hides a detail rather than a whole exchange.
   */
  import { clock, exact } from "../lib/format.ts";
  import { render } from "../lib/markdown.ts";
  import { marking } from "../lib/search.ts";
  import { callCount, type Turn } from "../lib/turns.ts";
  import Diff from "./Diff.svelte";
  import ToolCall from "./ToolCall.svelte";

  let {
    turn,
    running,
    waiting,
    needle = "",
    linked,
    onOpenFile,
    command,
  }: {
    turn: Turn;
    /** True when this is the exchange the session is working on now. */
    running: boolean;
    /**
     * True when this exchange has been admitted but is not working yet.
     *
     * Separate from running because a reader who cannot tell them apart will
     * interrupt a turn that is only queued.
     */
    waiting?: boolean;
    /**
     * What a search is looking for, marked wherever it appears.
     *
     * Marked here rather than in the text before it is drawn, because this is
     * where the rendered form exists: a match inside a message that went
     * through markdown is marked the same way as one in a tool line.
     */
    needle?: string;
    /** True when the address names this exchange, so it says which one it is. */
    linked?: boolean;
    onOpenFile?: ((path: string) => void) | undefined;
    command: { open: boolean; at: number };
  } = $props();

  /**
   * Whether this group is an exchange at all.
   *
   * Turn zero is a session's opening notices, recorded before anybody had
   * asked for anything, so it carries no number and gets no gutter.
   */
  const numbered = $derived(turn.number !== undefined && turn.number > 0);

  const calls = $derived(callCount(turn));
  const changed = $derived(
    turn.items.reduce(
      (total, item) => total + (item.kind === "call" ? item.call.changes.length : 0),
      0,
    ),
  );

  /**
   * Which calls print their time.
   *
   * A call prints it only when it differs from the call above it, so that a
   * run of commands at one minute says the minute once. What varies is what a
   * reader is left looking at.
   */
  const timed = $derived.by(() => {
    let last: string | undefined;
    return turn.items.map((item) => {
      if (item.kind !== "call") return false;
      const shown = clock(item.call.entry.at);
      const first = shown !== last;
      last = shown;
      return first;
    });
  });

  /**
   * A stable identity for an item, so a redraw updates the nodes it has rather
   * than rebuilding every one of them.
   */
  function itemKey(item: (typeof turn.items)[number], index: number): string {
    if (item.kind === "call") return item.call.entry.tool?.id ?? `call-${index}`;
    return `${item.entry.kind}-${item.entry.at}-${index}`;
  }
</script>

<section
  use:marking={{ needle, revision: turn.items.length }}
  class="turn"
  class:numbered
  class:running
  class:linked
  id={numbered ? `turn-${turn.number}` : undefined}
>
  {#if numbered}
    <div class="mark">
      <a class="number" href={`#turn-${turn.number}`} title="Link to this exchange"
        >{turn.number}</a
      >
    </div>
  {/if}

  <div class="body">
    {#if numbered}
      <div class="between">
        <span class="says">
          {#if waiting}
            <span class="held">waiting</span>
          {:else if running}
            <span class="working">working</span>
          {/if}
          {#if turn.failures > 0}
            <span class="failed">{turn.failures} failed</span>
          {/if}
          {#if calls > 0}
            <span>{calls} {calls === 1 ? "call" : "calls"}</span>
          {/if}
          {#if changed > 0}
            <span>{changed} changed</span>
          {/if}
          {#if turn.cost !== undefined}
            <span>{turn.cost}</span>
          {/if}
          <time datetime={new Date(turn.at).toISOString()} title={exact(turn.at)}>
            {clock(turn.at)}
          </time>
        </span>
      </div>
    {/if}

    {#if turn.asked !== undefined}
      <div class="asked" class:matching={needle.length > 0}>
        <span class="who">{turn.asked.author}</span>
        <p class="said">{turn.asked.text}</p>
      </div>
    {/if}

    {#each turn.items as item, index (itemKey(item, index))}
      {#if item.kind === "call"}
        <div class="item">
          <ToolCall
            tool={item.call.entry.tool}
            line={item.call.entry.line}
            result={item.call.result}
            at={item.call.entry.at}
            showTime={timed[index]}
            {command}
            {onOpenFile}
          />
          {#each item.call.changes as change}
            <div class="change">
              <Diff
                path={change.path}
                added={change.added}
                removed={change.removed}
                body={change.body}
                onOpen={onOpenFile}
              />
            </div>
          {/each}
        </div>
      {:else if item.entry.kind === "message"}
        <!-- Sanitised in render(), which is what makes this safe to insert. -->
        <div class="item message">{@html render(item.entry.text)}</div>
      {:else if item.entry.kind === "aside"}
        <p class="item aside">
          <span class="who">{item.entry.author}</span>
          <span class="text">{item.entry.text}</span>
          <span class="unheard">not sent to the agent</span>
        </p>
      {:else if item.entry.kind === "thinking"}
        <details class="item thinking">
          <summary>thought for a moment</summary>
          <p>{item.entry.text}</p>
        </details>
      {:else if item.entry.kind === "diff"}
        <div class="item">
          <Diff
            path={item.entry.path}
            added={item.entry.added}
            removed={item.entry.removed}
            body={item.entry.body}
            onOpen={onOpenFile}
          />
        </div>
      {:else if item.entry.kind === "reply"}
        <details class="item reply">
          <summary>{item.entry.command}</summary>
          <pre>{item.entry.text}</pre>
        </details>
      {:else if item.entry.kind === "notice"}
        <p class="item notice {item.entry.level}">{item.entry.text}</p>
      {:else if item.entry.kind === "file"}
        <p class="item notice">attached {item.entry.name} ({item.entry.size} bytes)</p>
      {/if}
    {/each}
  </div>
</section>

<style>
  /*
   * An exchange is a unit, and the gutter is what says so.
   *
   * A rule down the side rather than a box: a border on four sides makes a long
   * session read as a feed of unrelated items, and it competes with the tool
   * calls, which have edges of their own.
   */
  .turn {
    display: grid;
    grid-template-columns: 1fr;
    min-width: 0;
  }

  .turn.numbered {
    grid-template-columns: 2.25rem 1fr;
  }

  .body {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
  }

  /*
   * Sticky, so that a reader who is somewhere inside an exchange that fills
   * more than the screen can still see which one they are in.
   */
  .mark {
    position: relative;
    padding-right: 12px;
  }

  .mark::after {
    content: "";
    position: absolute;
    top: 0;
    bottom: 0;
    right: 5px;
    width: 1px;
    background: var(--line-soft);
  }

  .turn.running .mark::after {
    background: color-mix(in srgb, var(--accent) 45%, var(--line-soft));
  }

  .number {
    position: sticky;
    top: 0;
    display: block;
    font-family: var(--mono);
    font-size: var(--type-receipt);
    line-height: 1;
    padding: 3px 0;
    text-align: right;
    color: var(--text-faint);
    background: var(--bg);
  }

  .turn.running .number {
    color: var(--accent);
  }

  .number:hover {
    color: var(--text);
  }

  /* The exchange an address named, said in the gutter rather than by moving
     anything, so the conversation around it still reads normally. */
  .turn.linked .number {
    color: var(--accent-ink);
    background: var(--accent);
    border-radius: var(--radius-sm);
  }

  .turn.linked .mark::after {
    background: var(--accent);
  }

  .between {
    display: flex;
    justify-content: flex-end;
    min-width: 0;
  }

  .says {
    display: flex;
    align-items: baseline;
    justify-content: flex-end;
    flex-wrap: wrap;
    gap: 4px 10px;
    min-width: 0;
    font-size: var(--type-receipt);
    color: var(--text-faint);
  }

  .working {
    color: var(--accent);
  }

  .held {
    color: var(--warn);
  }

  .failed {
    color: var(--bad);
  }

  .item {
    min-width: 0;
    width: 100%;
  }

  /*
   * What somebody asked, marked by a rail and named. The agent's own words are
   * the body of the session and are left plain: one of the two needs marking,
   * and marking both would be noise on every line.
   */
  .asked {
    width: 100%;
    min-width: 0;
    margin: 4px 0;
    padding: 6px 0 6px 12px;
    border-left: 2px solid var(--accent);
    background: linear-gradient(
      to right,
      color-mix(in srgb, var(--accent) 7%, transparent),
      transparent 60%
    );
  }

  .asked.matching {
    border-left-color: var(--warn);
  }

  .who {
    display: block;
    font-family: var(--mono);
    font-size: var(--type-receipt);
    color: var(--accent);
    margin-bottom: 2px;
  }

  .asked.matching .who {
    color: var(--warn);
  }

  .change {
    margin-top: 4px;
  }

  .said {
    margin: 0;
    font-size: var(--type-answer);
    /* Prose holds itself to a readable line. That is a property of a paragraph,
       and it changes the width of nothing around it. */
    max-width: var(--measure);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    color: var(--text);
  }

  .message {
    min-width: 0;
    overflow-wrap: anywhere;
    font-size: var(--type-answer);
  }

  /* Prose only. A diff, a table, or a code block wants the width it is given. */
  .message :global(p),
  .message :global(li),
  .message :global(blockquote) {
    max-width: var(--measure);
  }

  .message :global(p) {
    margin: 0 0 8px;
  }

  .message :global(p:last-child) {
    margin-bottom: 0;
  }

  .message :global(pre) {
    margin: 8px 0;
    padding: 10px 12px;
    max-width: 100%;
    overflow-x: auto;
    background: var(--bg-inset);
    border: 1px solid var(--line-soft);
    border-radius: var(--radius);
  }

  .message :global(code) {
    font-family: var(--mono);
    font-size: var(--type-work);
  }

  .message :global(:not(pre) > code) {
    padding: 1px 5px;
    background: var(--bg-raised);
    border-radius: 4px;
  }

  .message :global(pre code) {
    background: none;
    padding: 0;
  }

  .message :global(ul),
  .message :global(ol) {
    margin: 8px 0;
    padding-left: 22px;
  }

  .message :global(a) {
    color: var(--accent);
  }

  .message :global(blockquote) {
    margin: 8px 0;
    padding-left: 12px;
    border-left: 2px solid var(--line);
    color: var(--text-dim);
  }

  .message :global(h1),
  .message :global(h2),
  .message :global(h3),
  .message :global(h4) {
    /* Told apart by weight, since prose already holds the largest step. */
    margin: 12px 0 6px;
    font-size: var(--type-answer);
    font-weight: 700;
  }

  .message :global(table) {
    border-collapse: collapse;
    margin: 8px 0;
    display: block;
    max-width: 100%;
    overflow-x: auto;
  }

  .message :global(th),
  .message :global(td) {
    border: 1px solid var(--line);
    padding: 4px 8px;
    text-align: left;
  }

  /* Said in the thread, not to the agent, so it reads as a note beside the
     conversation rather than as part of it. */
  .aside {
    display: flex;
    align-items: baseline;
    gap: 8px;
    margin: 0;
    min-width: 0;
    padding-left: 12px;
    border-left: 2px dashed var(--line);
    font-size: var(--type-work);
    color: var(--text-faint);
  }

  .aside .text {
    flex: 1;
    min-width: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .aside .unheard {
    flex-shrink: 0;
    font-size: var(--type-receipt);
    font-style: italic;
  }

  .thinking {
    min-width: 0;
    border-left: 2px solid var(--line);
    padding-left: 12px;
  }

  .aside .who {
    display: inline;
    margin: 0;
    flex-shrink: 0;
    color: var(--text-dim);
  }

  .thinking summary {
    font-size: var(--type-work);
    color: var(--text-faint);
    cursor: pointer;
    font-style: italic;
  }

  .thinking p {
    margin: 6px 0 0;
    max-width: var(--measure);
    font-size: var(--type-work);
    color: var(--text-faint);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .reply {
    border: 1px solid var(--line-soft);
    border-radius: var(--radius);
    background: var(--bg-inset);
    overflow: hidden;
  }

  .reply summary {
    padding: 6px 12px;
    font-family: var(--mono);
    font-size: var(--type-receipt);
    color: var(--text-faint);
    cursor: pointer;
  }

  .reply pre {
    margin: 0;
    padding: 10px 12px;
    max-width: 100%;
    max-height: 340px;
    overflow: auto;
    font-size: var(--type-work);
    color: var(--text-dim);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .notice {
    margin: 0;
    font-size: var(--type-receipt);
    color: var(--text-faint);
  }

  .notice.warning {
    color: var(--warn);
  }

  /*
   * On a narrow screen the rule and its indent cost about a fourteenth of the
   * width, and grouping there already comes from proximity. The number stays,
   * because identity is the part that cannot be inferred.
   */
  @media (max-width: 560px) {
    .turn.numbered {
      grid-template-columns: 1fr;
    }

    .mark {
      padding: 0;
      order: -1;
    }

    .mark::after {
      display: none;
    }

    .number {
      position: static;
      display: inline-block;
      text-align: left;
      padding: 0;
      background: none;
    }
  }

  @media (max-width: 720px) {
    .says time {
      display: none;
    }
  }
</style>
