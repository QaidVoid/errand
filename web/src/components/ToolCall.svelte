<script lang="ts">
  /**
   * One tool call.
   *
   * Collapsed it is a single line that never wraps, so a run of calls reads as
   * a list rather than a wall. Opened it shows the whole call and whatever the
   * tool produced, so nothing is lost by folding it away.
   */
  import { clock, exact } from "../lib/format.ts";
  import { render } from "../lib/markdown.ts";
  import { agentRuns, duration, glyph, summarize, todos, toolKind } from "../lib/tools.ts";
  import type { ToolActivity, ToolResult } from "../lib/types.ts";

  let {
    tool,
    line,
    result,
    progress,
    at,
    showTime,
    command,
    onOpenFile,
  }: {
    tool?: ToolActivity | null | undefined;
    /** The rendered line, used when there are no parts to lay out. */
    line: string;
    result?: ToolResult | undefined;
    /** The newest line the call reported while it ran. */
    progress?: string | undefined;
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

  /** The workspace as the agent sees it, which is not a path a reader wants. */
  const WORKSPACE = /^\/workspace\/?/;

  /** The target as shown: the workspace prefix is noise on every row. */
  const target = $derived(tool?.target?.replace(WORKSPACE, "") ?? "");

  /** A target that names a file in the project, so it can be opened. */
  const path = $derived.by(() => {
    if (tool?.target === undefined || onOpenFile === undefined) return undefined;
    if (toolKind(tool?.name) !== "reads" && toolKind(tool?.name) !== "writes") return undefined;
    if (/\s/.test(target)) return undefined;
    return target.length === 0 ? undefined : target;
  });

  /** A one line account of how it went, so a folded call still says something. */
  const summary = $derived(running ? (progress ?? "running") : summarize(tool?.name, output, failed));

  /** A todo list's items, when this call reported one. */
  const listed = $derived(tool?.name === "todo_list" ? todos(output) : []);

  /** The agents this call ran and what each reported, when it ran any. */
  const runs = $derived(tool?.name === "agent" || tool?.name === "swarm" ? agentRuns(output) : []);
</script>

<article class="call {toolKind(tool?.name)}" class:failed class:open class:running>
  <button
    class="head"
    aria-expanded={open}
    onclick={() => (open = !open)}
    title={open ? "Collapse" : "Expand"}
  >
    <span class="dot" aria-hidden="true">{glyph(tool?.name)}</span>
    <!-- An older recording says null where a newer one leaves the field out. -->
    {#if tool === undefined || tool === null}
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

      {#if listed.length > 0}
        <div class="full">
          <span class="label">list</span>
          <ul class="todos">
            {#each listed as todo, index (index)}
              <li class={todo.state}>
                <span class="box" aria-hidden="true">{todo.state === "done" ? "x" : todo.state === "doing" ? "~" : " "}</span>
                {todo.text}
              </li>
            {/each}
          </ul>
        </div>
      {:else if runs.length > 0}
        {#each runs as run, index (index)}
          <div class="full">
            <span class="label">{runs.length > 1 ? `agent ${index + 1}` : "agent"}</span>
            <div class="run">
              <p class="about">
                {run.name}{run.state ? `, ${run.state}` : ""}{run.model ? ` on ${run.model}` : ""}{run.tools !== undefined
                  ? `, ${run.tools} tools`
                  : ""}{run.ms !== undefined ? `, ${duration(run.ms)}` : ""}
              </p>
              <!-- Sanitised in render(), which is what makes this safe to insert. -->
              <div class="report">{@html render(run.report)}</div>
            </div>
          </div>
        {/each}
      {:else if output.length > 0}
        <div class="full">
          <span class="label">output</span>
          <pre>{output}</pre>
        </div>
      {:else if progress !== undefined}
        <div class="full">
          <span class="label">{running ? "now" : "last said"}</span>
          <code>{progress}</code>
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
  .runs .dot,
  .asks .dot {
    color: var(--accent);
  }
  .web .dot,
  .agents .dot,
  .media .dot {
    color: var(--text-dim);
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

  .todos {
    margin: 0;
    padding: 0;
    list-style: none;
    font-size: var(--type-work);
    color: var(--text-dim);
  }

  .todos li {
    display: flex;
    gap: 8px;
  }

  .todos .box {
    flex-shrink: 0;
    font-family: var(--mono);
    color: var(--text-faint);
  }

  .todos .done {
    color: var(--text-faint);
    text-decoration: line-through;
  }

  .todos .doing {
    color: var(--text);
  }

  .todos .doing .box {
    color: var(--accent);
  }

  .run {
    min-width: 0;
  }

  .run .about {
    margin: 0 0 4px;
    font-family: var(--mono);
    font-size: var(--type-receipt);
    color: var(--text-faint);
  }

  .run .report {
    max-height: 420px;
    overflow: auto;
    font-size: var(--type-work);
    color: var(--text-dim);
    overflow-wrap: anywhere;
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
