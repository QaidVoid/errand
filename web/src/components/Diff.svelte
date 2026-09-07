<script lang="ts">
  /**
   * A change to one file.
   *
   * Rendered as a real diff rather than a code fence: gutter, per-line tint,
   * and hunk breaks, so scanning it does not mean reading every line.
   */
  let {
    path,
    added,
    removed,
    body,
    onOpen,
  }: {
    path: string;
    added: number;
    removed: number;
    body: string;
    /** Opens the changed file, so a change leads to the thing it changed. */
    onOpen?: ((path: string) => void) | undefined;
  } = $props();

  const lines = $derived(
    body.split("\n").map((line) => ({
      kind: line.startsWith("+") ? "add" : line.startsWith("-") ? "del" : line.startsWith("@@") ? "hunk" : "ctx",
      text: line.startsWith("@@") ? line : line.slice(1),
    })),
  );
</script>

<figure class="diff">
  <figcaption>
    {#if onOpen === undefined}
      <span class="path">{path}</span>
    {:else}
      <button class="path open" onclick={() => onOpen?.(path)} title="Open {path}">{path}</button>
    {/if}
    <span class="counts">
      <span class="add">+{added}</span>
      <span class="del">-{removed}</span>
    </span>
  </figcaption>
  <div class="body">
    {#each lines as line}
      <div class="line {line.kind}">
        <span class="gutter" aria-hidden="true"
          >{line.kind === "add" ? "+" : line.kind === "del" ? "-" : ""}</span
        >
        <span class="text">{line.text}</span>
      </div>
    {/each}
  </div>
</figure>

<style>
  .diff {
    margin: 0;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    overflow: hidden;
    background: var(--bg-inset);
  }

  figcaption {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gap);
    padding: 7px 12px;
    background: var(--bg-raised);
    border-bottom: 1px solid var(--line);
  }

  .path.open {
    text-decoration: underline;
    text-decoration-color: var(--line);
    text-underline-offset: 2px;
  }

  .path.open:hover {
    color: var(--accent);
  }

  .path {
    font-family: var(--mono);
    font-size: var(--type-work);
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .counts {
    display: flex;
    gap: 8px;
    font-family: var(--mono);
    font-size: var(--type-receipt);
    flex-shrink: 0;
  }

  .counts .add {
    color: var(--add-fg);
  }
  .counts .del {
    color: var(--del-fg);
  }

  .body {
    overflow-x: auto;
    padding: 4px 0;
  }

  .line {
    display: grid;
    grid-template-columns: 22px 1fr;
    font-family: var(--mono);
    font-size: var(--type-work);
    line-height: 1.5;
    white-space: pre;
  }

  .gutter {
    text-align: center;
    color: var(--text-faint);
    user-select: none;
  }

  .add {
    background: var(--add-bg);
    color: var(--add-fg);
  }
  .del {
    background: var(--del-bg);
    color: var(--del-fg);
  }
  .ctx {
    color: var(--text-dim);
  }

  .hunk {
    color: var(--text-faint);
    background: var(--line-soft);
    font-size: var(--type-receipt);
  }

  .text {
    padding-right: 12px;
  }
</style>
