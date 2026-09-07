<script lang="ts">
  /**
   * One file, read.
   *
   * The numbers are their own column beside the code rather than woven into it.
   * A highlighted file is one block whose spans cross line boundaries, so
   * splitting it into a row per line would mean reopening whatever a block
   * comment or a multi-line string left open.
   */
  import { api } from "../lib/api.ts";
  import { humanSize } from "../lib/format.ts";
  import { highlight, languageOf } from "../lib/highlight.ts";
  import type { FileContents } from "../lib/types.ts";

  let {
    session,
    file,
    onClose,
    onOpenPath,
  }: {
    session: string;
    file: FileContents;
    onClose: () => void;
    /** Opens a directory in the tree, for walking back up the path. */
    onOpenPath?: ((path: string) => void) | undefined;
  } = $props();

  const lines = $derived(file.text.split("\n"));

  /** The grammar in force, or undefined when the file is read as plain text. */
  const language = $derived(languageOf(file.language));
  const marked = $derived(language === undefined ? undefined : highlight(file.text, file.language));

  /** Each ancestor of the file, so the path leads back to where it came from. */
  const trail = $derived.by(() => {
    const parts = file.path.split("/").filter((part) => part.length > 0);
    const crumbs: { name: string; path: string }[] = [];
    for (let index = 0; index < parts.length - 1; index++) {
      crumbs.push({ name: parts[index] as string, path: parts.slice(0, index + 1).join("/") });
    }
    return { crumbs, name: parts[parts.length - 1] ?? file.path };
  });
</script>

<header class="bar">
  <button class="back" onclick={onClose} aria-label="Back to the file tree">back</button>
  <nav class="trail" aria-label="Path">
    <button onclick={() => onOpenPath?.("")}>project</button>
    {#each trail.crumbs as crumb}
      <span class="sep" aria-hidden="true">/</span>
      <button onclick={() => onOpenPath?.(crumb.path)}>{crumb.name}</button>
    {/each}
    <span class="sep" aria-hidden="true">/</span>
    <span class="here">{trail.name}</span>
  </nav>
  <a class="get" href={api.downloadUrl(session, file.path)} download>download</a>
</header>

<p class="about">
  <span>{humanSize(file.size)}</span>
  <span>{language ?? "plain text"}</span>
  {#if !file.binary}
    <span>{lines.length} lines</span>
  {/if}
</p>

{#if file.binary}
  <p class="stated">This file is not text. Download it to look at it.</p>
{:else}
  <div class="source">
    <div class="numbers" aria-hidden="true">
      {#each lines as _line, index}
        <span>{index + 1}</span>
      {/each}
    </div>
    {#if marked === undefined}
      <pre class="code"><code>{file.text}</code></pre>
    {:else}
      <!-- Escaped by the highlighter, which wraps the text without altering it. -->
      <pre class="code"><code>{@html marked}</code></pre>
    {/if}
  </div>
  {#if file.truncated}
    <p class="stated">
      Shown up to the daemon's limit. Download the file for the rest.
      <a href={api.downloadUrl(session, file.path)} download>download</a>
    </p>
  {/if}
{/if}

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 12px;
    border-bottom: 1px solid var(--line-soft);
    font-size: var(--type-work);
  }

  .trail {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 3px;
    overflow-x: auto;
    white-space: nowrap;
    font-family: var(--mono);
  }

  .trail button {
    flex-shrink: 0;
    color: var(--text-faint);
  }

  .trail button:hover {
    color: var(--accent);
    text-decoration: underline;
  }

  .sep {
    color: var(--text-faint);
    flex-shrink: 0;
  }

  .here {
    color: var(--text);
    flex-shrink: 0;
  }

  .back,
  .get {
    color: var(--text-faint);
    flex-shrink: 0;
  }

  .back:hover,
  .get:hover {
    color: var(--text);
  }

  .about {
    display: flex;
    gap: 14px;
    margin: 0;
    padding: 6px 12px;
    border-bottom: 1px solid var(--line-soft);
    font-size: var(--type-receipt);
    color: var(--text-faint);
  }

  /* One scroller holding both columns, so the numbers cannot drift from the
     lines they belong to. */
  .source {
    display: flex;
    flex: 1;
    min-height: 0;
    overflow: auto;
    font-family: var(--mono);
    font-size: var(--type-work);
    line-height: 1.55;
  }

  .numbers {
    position: sticky;
    left: 0;
    display: flex;
    flex-direction: column;
    flex-shrink: 0;
    padding: 8px 10px 8px 12px;
    text-align: right;
    color: var(--text-faint);
    background: var(--bg-inset);
    user-select: none;
  }

  .code {
    margin: 0;
    padding: 8px 14px 8px 0;
    flex: 1;
    min-width: 0;
    white-space: pre;
    color: var(--text-dim);
  }

  .stated {
    margin: 0;
    padding: 10px 12px;
    border-top: 1px solid var(--line-soft);
    color: var(--text-faint);
    font-size: var(--type-work);
  }
</style>
