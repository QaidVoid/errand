<script lang="ts">
  /**
   * Picks a model by the names `--model` and `!model` take.
   *
   * Short names first, then every model the host lists, grouped by provider.
   * Typing filters both, and a name nobody lists can still be used as typed,
   * since a provider may serve more than its listing says.
   */
  import type { ModelCatalog } from "../lib/types.ts";

  let {
    catalog,
    current,
    allowDefault = false,
    disabled = false,
    onPick,
  }: {
    catalog: ModelCatalog | null;
    /** What the button shows, or null for the default. */
    current: string | null;
    /** Whether "the default" is itself a choice, as it is before a session starts. */
    allowDefault?: boolean;
    /** True where nothing can be sent, so a pick would go nowhere. */
    disabled?: boolean;
    onPick: (name: string | null) => void;
  } = $props();

  let open = $state(false);
  let query = $state("");
  let root = $state<HTMLElement | null>(null);
  let search = $state<HTMLInputElement | null>(null);

  const shown = $derived(current ?? (catalog?.default ? `default: ${catalog.default}` : "default model"));

  const groups = $derived.by(() => {
    if (catalog === null) return [];
    const wanted = query.trim().toLowerCase();
    const matches = (name: string) => wanted === "" || name.toLowerCase().includes(wanted);
    const out: { title: string; names: { name: string; note?: string }[] }[] = [];
    const short = Object.entries(catalog.aliases)
      .filter(([name, stands]) => matches(name) || matches(stands))
      .map(([name, stands]) => ({ name, note: stands }));
    if (short.length > 0) out.push({ title: "short names", names: short });
    const byProvider = new Map<string, { name: string }[]>();
    for (const name of catalog.models.filter(matches)) {
      const provider = name.split("/")[0] ?? "";
      byProvider.set(provider, [...(byProvider.get(provider) ?? []), { name }]);
    }
    for (const [provider, names] of byProvider) out.push({ title: provider, names });
    return out;
  });

  const typed = $derived(query.trim());
  const listed = $derived(groups.some((group) => group.names.some((entry) => entry.name === typed)));

  function pick(name: string | null): void {
    open = false;
    query = "";
    onPick(name);
  }

  $effect(() => {
    if (!open) return;
    search?.focus();
    const away = (event: PointerEvent) => {
      if (root !== null && !root.contains(event.target as Node)) open = false;
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        open = false;
      }
    };
    window.addEventListener("pointerdown", away);
    window.addEventListener("keydown", escape, true);
    return () => {
      window.removeEventListener("pointerdown", away);
      window.removeEventListener("keydown", escape, true);
    };
  });
</script>

<div class="picker" bind:this={root}>
  <button
    type="button"
    class="pill"
    aria-haspopup="listbox"
    aria-expanded={open}
    title="Choose the model"
    {disabled}
    onclick={() => (open = !open)}
  >
    <span class="name">{shown}</span>
    <span class="caret" aria-hidden="true">v</span>
  </button>

  {#if open}
    <div class="pop" role="listbox" aria-label="Models">
      <input
        bind:this={search}
        bind:value={query}
        placeholder="Search models"
        spellcheck="false"
        onkeydown={(event) => {
          if (event.key === "Enter" && typed.length > 0) {
            event.preventDefault();
            pick(typed);
          }
        }}
      />
      <div class="list">
        {#if allowDefault && query.trim() === ""}
          <button type="button" class="option" class:on={current === null} onclick={() => pick(null)}>
            <span>default</span>
            {#if catalog?.default}<em>{catalog.default}</em>{/if}
          </button>
        {/if}
        {#each groups as group (group.title)}
          <p class="group">{group.title}</p>
          {#each group.names as entry (entry.name)}
            <button
              type="button"
              class="option"
              class:on={current === entry.name}
              onclick={() => pick(entry.name)}
            >
              <span>{entry.name}</span>
              {#if entry.note}<em>{entry.note}</em>{/if}
            </button>
          {/each}
        {/each}
        {#if typed.length > 0 && !listed}
          <button type="button" class="option" onclick={() => pick(typed)}>
            <span>use "{typed}"</span>
            <em>as typed</em>
          </button>
        {:else if groups.length === 0 && catalog !== null && typed.length === 0}
          <p class="empty">The host lists no models. Type one to use it as named.</p>
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .picker {
    position: relative;
    flex: 0 1 auto;
    min-width: 0;
  }

  .pill {
    display: flex;
    align-items: center;
    gap: 6px;
    max-width: min(260px, 100%);
    padding: 4px 10px;
    border: 1px solid var(--line);
    border-radius: 999px;
    font-family: var(--mono);
    font-size: 11.5px;
    color: var(--muted);
  }

  .pill:hover,
  .pill[aria-expanded="true"] {
    background: var(--fill-hover);
    color: var(--ink);
  }

  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .caret {
    font-size: 9px;
    color: var(--faint);
  }

  .pop {
    position: absolute;
    z-index: 15;
    bottom: calc(100% + 8px);
    left: 0;
    display: flex;
    flex-direction: column;
    width: min(360px, 86vw);
    max-height: min(420px, 60vh);
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--menu);
    box-shadow: 0 16px 40px -12px rgb(0 0 0 / 45%);
    overflow: hidden;
  }

  input {
    margin: 8px;
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--composer-bg);
    color: var(--ink);
    font: inherit;
    font-size: 13px;
  }

  .list {
    overflow-y: auto;
    padding: 0 6px 6px;
  }

  .group {
    margin: 8px 8px 4px;
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ghost);
  }

  .option {
    display: flex;
    align-items: baseline;
    gap: 10px;
    width: 100%;
    padding: 6px 8px;
    border-radius: 7px;
    text-align: left;
    font-family: var(--mono);
    font-size: 12.5px;
    color: var(--ink);
  }

  .option:hover {
    background: var(--hover);
  }

  .option.on {
    background: var(--accent-soft);
    color: var(--accent);
  }

  .option em {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-style: normal;
    color: var(--faint);
  }

  .empty {
    margin: 8px;
    font-size: 12.5px;
    color: var(--faint);
  }
</style>
