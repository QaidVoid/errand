<script lang="ts">
  /**
   * One tool call.
   *
   * Collapsed it is a single line that never wraps, so a run of calls reads as
   * a list rather than a wall. Opened it shows the whole call and whatever the
   * tool produced, so nothing is lost by folding it away.
   */
  import { clock, exact } from "../lib/format.ts";
  import type { ToolActivity, ToolResult } from "../lib/types.ts";

  let {
    tool,
    line,
    result,
    at,
    showTime,
    command,
    onOpenFile,
  }: {
    tool?: ToolActivity | undefined;
    /** The rendered line, used when there are no parts to lay out. */
    line: string;
    result?: ToolResult | undefined;
    at: number;
    /**
     * Whether to print the time on the folded line.
     *
     * False when the call above ran at the same minute, because a column of
     * one repeated value is texture rather than information. The exact time is
     * still on the call, which is what opening it shows.
     */
    showTime?: boolean | undefined;
    /** A request from above to open or close every call at once. */
    command: { open: boolean; at: number };
    onOpenFile?: ((path: string) => void) | undefined;
  } = $props();

  let open = $state(false);

  // Follows an expand or collapse all, without a later redraw undoing whatever
  // was chosen for this one afterwards.
  let followed = $state(-1);
  $effect(() => {
    if (command.at === followed) return;
    followed = command.at;
    open = command.open;
  });

  const failed = $derived(result?.failed ?? tool?.failed ?? false);
  const output = $derived(result?.output.trim() ?? "");

  /** True while a call has started and nothing has come back for it yet. */
  const running = $derived(tool?.id !== undefined && result === undefined && !failed);

  /**
   * A glyph per kind of tool, so a run of calls can be scanned by shape.
   *
   * ASCII only, like everything else in this repository.
   */
  function glyph(name: string | undefined): string {
    if (name === undefined) return "*";
    const lowered = name.toLowerCase();
    if (/(edit|write|create|apply|patch)/.test(lowered)) return "+";
    if (/(read|cat|view|open)/.test(lowered)) return ">";
    if (/(search|grep|find|glob|ls|list)/.test(lowered)) return "*";
    if (/(bash|shell|run|exec|command)/.test(lowered)) return "$";
    if (/(fetch|http|web|url)/.test(lowered)) return "@";
    if (/(think|plan|todo|task)/.test(lowered)) return "~";
    return "-";
  }

  /** Which colour a tool takes, by what it does to the project. */
  function kind(name: string | undefined): string {
    if (name === undefined) return "other";
    const lowered = name.toLowerCase();
    if (/(edit|write|create|apply|patch)/.test(lowered)) return "writes";
    if (/(bash|shell|run|exec|command)/.test(lowered)) return "runs";
    if (/(read|cat|view|open|search|grep|find|glob|ls|list)/.test(lowered)) return "reads";
    return "other";
  }

  /** The workspace as the agent sees it, which is not a path a reader wants. */
  const WORKSPACE = /^\/workspace\/?/;

  /** The target as shown: the workspace prefix is noise on every row. */
  const target = $derived(tool?.target?.replace(WORKSPACE, "") ?? "");

  /** A target that names a file in the project, so it can be opened. */
  const path = $derived.by(() => {
    if (tool?.target === undefined || onOpenFile === undefined) return undefined;
    if (kind(tool?.name) === "runs" || /\s/.test(target)) return undefined;
    return target.length === 0 ? undefined : target;
  });

  /**
   * A one line account of how it went, so a folded call still says something.
   *
   * The first line of the output is usually the summary a tool writes for
   * itself, and beyond that only the size is worth stating.
   */
  const summary = $derived.by(() => {
    if (running) return "running";
    if (output.length === 0) return failed ? "failed" : "";

    const lines = output.split("\n");
    const first = lines[0] ?? "";
    if (lines.length === 1 && first.length <= 60) return first;
    return `${lines.length} lines`;
  });
</script>

<article class="call {kind(tool?.name)}" class:failed class:open class:running>
  <button
    class="head"
    aria-expanded={open}
    onclick={() => (open = !open)}
    title={open ? "Collapse" : "Expand"}
  >
    <span class="dot" aria-hidden="true">{glyph(tool?.name)}</span>
    {#if tool === undefined}
      <span class="target">{line}</span>
    {:else}
      <span class="name">{tool.name}</span>
      <span class="target">{target}</span>
    {/if}
    {#if summary.length > 0}
      <span class="summary">{summary}</span>
    {/if}
    {#if showTime !== false}
      <time datetime={new Date(at).toISOString()} title={exact(at)}>{clock(at)}</time>
    {/if}
    <span class="chevron" aria-hidden="true">{open ? "v" : ">"}</span>
  </button>

  {#if open}
    <div class="detail">
      {#if target.length > 0}
        <div class="full">
          <span class="label">call</span>
          <code>{target}</code>
        </div>
      {/if}

      {#if path !== undefined}
        <div class="full">
          <span class="label"></span>
          <button class="link" onclick={() => onOpenFile?.(path)}>open {path}</button>
        </div>
      {/if}

      <div class="full">
        <span class="label">at</span>
        <span class="when">{exact(at)}</span>
      </div>

      {#if output.length > 0}
        <div class="full">
          <span class="label">output</span>
          <pre>{output}</pre>
        </div>
      {:else if !running}
        <div class="full">
          <span class="label">output</span>
          <span class="none">nothing</span>
        </div>
      {/if}
    </div>
  {/if}
</article>

<style>
  /*
   * A line rather than a card. A run of thirty of these is the texture of an
   * agent working, and thirty bordered boxes is a wall.
   */
  .call {
    min-width: 0;
    border-radius: var(--radius-sm);
  }

  /* One line, always. Everything that does not fit is folded, not wrapped. */
  .head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    width: 100%;
    min-width: 0;
    padding: 1px 6px;
    border-radius: var(--radius-sm);
    font-family: var(--mono);
    font-size: var(--type-work);
    line-height: 1.65;
    text-align: left;
    white-space: nowrap;
  }

  .head:hover {
    background: var(--bg-raised);
  }

  /* A glyph rather than a dot: it says which kind of tool without a legend. */
  .dot {
    flex-shrink: 0;
    width: 10px;
    text-align: center;
    color: var(--text-faint);
  }

  .writes .dot {
    color: var(--ok);
  }
  .runs .dot {
    color: var(--accent);
  }
  .failed .dot {
    color: var(--bad);
  }

  .running .dot {
    animation: pulse 1.1s ease-in-out infinite;
  }

  @keyframes pulse {
    0%,
    100% {
      opacity: 0.3;
    }
    50% {
      opacity: 1;
    }
  }

  .name {
    flex-shrink: 0;
    color: var(--text-dim);
  }

  .failed .name {
    color: var(--bad);
  }

  .target {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--text-dim);
  }

  .summary {
    flex-shrink: 0;
    max-width: 24ch;
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: var(--type-receipt);
    color: var(--text-faint);
  }

  .failed .summary {
    color: var(--bad);
  }

  time {
    flex-shrink: 0;
    font-size: var(--type-receipt);
    color: var(--text-faint);
  }

  .chevron {
    flex-shrink: 0;
    width: 10px;
    text-align: center;
    color: var(--text-faint);
  }

  .detail {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 2px 0 6px 24px;
    padding: 8px 10px;
    border: 1px solid var(--line-soft);
    border-radius: var(--radius-sm);
    background: var(--bg-inset);
  }

  .full {
    display: grid;
    grid-template-columns: 52px minmax(0, 1fr);
    gap: 10px;
    align-items: baseline;
  }

  .label {
    font-size: var(--type-receipt);
    color: var(--text-faint);
    text-align: right;
  }

  .full code,
  .full pre {
    margin: 0;
    min-width: 0;
    max-width: 100%;
    font-family: var(--mono);
    font-size: var(--type-work);
    line-height: 1.5;
    color: var(--text-dim);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .full pre {
    max-height: 320px;
    overflow: auto;
  }

  .when {
    font-family: var(--mono);
    font-size: var(--type-receipt);
    color: var(--text-faint);
  }

  .none {
    font-size: var(--type-work);
    color: var(--text-faint);
    font-style: italic;
  }

  .link {
    justify-self: start;
    font-family: var(--mono);
    font-size: var(--type-work);
    color: var(--accent);
    text-decoration: underline;
    text-underline-offset: 2px;
  }

  /* The clock is the first thing to go when the line is tight. */
  @media (max-width: 620px) {
    time {
      display: none;
    }

    .summary {
      max-width: 12ch;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .running .dot {
      animation: none;
      opacity: 0.6;
    }
  }
</style>
