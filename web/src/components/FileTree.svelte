<script lang="ts">
  /**
   * A session's project, browsed one directory at a time.
   *
   * A directory at a time rather than an expanding tree: it keeps the path
   * always visible, which is the thing the old tree could not tell you.
   */
  import { untrack } from "svelte";
  import { api } from "../lib/api.ts";
  import { humanSize } from "../lib/format.ts";
  import type { TreeNode } from "../lib/types.ts";

  let {
    session,
    at = $bindable(""),
    onOpen,
  }: {
    session: string;
    /** The directory being shown, relative to the project root. */
    at?: string;
    onOpen: (path: string) => void;
  } = $props();

  let nodes = $state<TreeNode[]>([]);
  let failed = $state<string | null>(null);
  let loading = $state(false);

  /** Directories first, then files, each by name, so a project always reads
      the same way rather than in whatever order it came back in. */
  const ordered = $derived(
    [...nodes].sort((a, b) => {
      if (a.directory !== b.directory) return a.directory ? -1 : 1;
      return a.name.localeCompare(b.name);
    }),
  );

  /** Each ancestor of where we are, so the path leads back. */
  const trail = $derived(
    at
      .split("/")
      .filter((part) => part.length > 0)
      .map((name, index, parts) => ({ name, path: parts.slice(0, index + 1).join("/") })),
  );

  async function load(path: string): Promise<void> {
    loading = true;
    failed = null;
    try {
      nodes = await api.tree(session, path);
    } catch (error) {
      failed = error instanceof Error ? error.message : String(error);
      nodes = [];
    } finally {
      loading = false;
    }
  }

  function go(path: string): void {
    at = path;
  }

  // Depends on the session and where we are, and nothing it writes.
  $effect(() => {
    const where = at;
    const which = session;
    untrack(() => void load(where === "" ? "" : where).then(() => which));
  });
</script>

<nav class="trail" aria-label="Path">
  <button onclick={() => go("")} class:here={at === ""}>project</button>
  {#each trail as crumb}
    <span class="sep" aria-hidden="true">/</span>
    <button onclick={() => go(crumb.path)} class:here={crumb.path === at}>{crumb.name}</button>
  {/each}
</nav>

<div class="listing">
  {#if failed !== null}
    <p class="stated bad">{failed}</p>
  {:else if loading && nodes.length === 0}
    <p class="stated">Reading.</p>
  {:else if ordered.length === 0}
    <p class="stated">This directory is empty.</p>
  {:else}
    {#if at !== ""}
      <button
        class="row up"
        onclick={() => go(trail.length > 1 ? (trail[trail.length - 2]?.path ?? "") : "")}
      >
        <span class="glyph" aria-hidden="true">^</span>
        <span class="name">up</span>
      </button>
    {/if}

    {#each ordered as node (node.path)}
      <button
        class="row"
        class:dir={node.directory}
        onclick={() => (node.directory ? go(node.path) : onOpen(node.path))}
        title={node.path}
      >
        <span class="glyph" aria-hidden="true">{node.directory ? ">" : "-"}</span>
        <span class="name">{node.name}</span>
        {#if !node.directory}
          <span class="what">{humanSize(node.size)}</span>
        {/if}
      </button>
    {/each}
  {/if}
</div>

<style>
  .trail {
    display: flex;
    align-items: baseline;
    gap: 3px;
    padding: 10px 12px;
    border-bottom: 1px solid var(--line-soft);
    overflow-x: auto;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--type-work);
    flex-shrink: 0;
  }

  .trail button {
    flex-shrink: 0;
    color: var(--text-faint);
  }

  .trail button:hover {
    color: var(--accent);
    text-decoration: underline;
  }

  .trail button.here {
    color: var(--text);
  }

  .sep {
    color: var(--text-faint);
    flex-shrink: 0;
  }

  .listing {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 6px 0;
  }

  .stated {
    margin: 8px 12px;
    font-size: var(--type-work);
    color: var(--text-faint);
  }

  .stated.bad {
    color: var(--bad);
  }

  .row {
    display: flex;
    align-items: baseline;
    gap: 8px;
    width: 100%;
    min-width: 0;
    padding: 4px 12px;
    text-align: left;
    font-family: var(--mono);
    font-size: var(--type-work);
    color: var(--text-dim);
  }

  .row:hover {
    background: var(--bg-raised);
    color: var(--text);
  }

  .glyph {
    width: 9px;
    flex-shrink: 0;
    color: var(--text-faint);
  }

  .row.dir .glyph {
    color: var(--accent);
  }

  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .row.dir .name {
    color: var(--text);
  }

  .what {
    flex-shrink: 0;
    font-size: var(--type-receipt);
    color: var(--text-faint);
  }

  .up .name {
    color: var(--text-faint);
  }
</style>
