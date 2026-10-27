<script lang="ts">
  /**
   * Narrows what each exchange shows, from one small menu.
   *
   * Each choice still says how much it would show, so one with nothing in it
   * is disabled rather than looking broken once picked.
   */
  import { type Lens, LENS_HELP, LENSES } from "../lib/lens.ts";
  import Icon from "./Icon.svelte";

  let { lens = $bindable(), counts }: { lens: Lens; counts: Record<Lens, number> } = $props();

  const LABELS: Record<Lens, string> = {
    all: "Everything",
    said: "Conversation",
    ran: "Tool calls",
    changed: "Changes",
  };

  let open = $state(false);
  let root = $state<HTMLElement | null>(null);

  $effect(() => {
    if (!open) return;
    const away = (event: PointerEvent) => {
      if (root !== null && !root.contains(event.target as Node)) open = false;
    };
    window.addEventListener("pointerdown", away);
    return () => window.removeEventListener("pointerdown", away);
  });
</script>

<div class="menu" bind:this={root}>
  <button
    class="trigger"
    class:narrowed={lens !== "all"}
    aria-haspopup="menu"
    aria-expanded={open}
    title="Show only part of each exchange"
    onclick={() => (open = !open)}
  >
    <Icon name="layers" />
    {#if lens !== "all"}<span>{LABELS[lens]}</span>{/if}
  </button>
  {#if open}
    <div class="pop" role="menu">
      {#each LENSES as value}
        <button
          role="menuitemradio"
          aria-checked={lens === value}
          class:on={lens === value}
          disabled={counts[value] === 0 && value !== "all"}
          onclick={() => {
            lens = value;
            open = false;
          }}
        >
          <span class="label">{LABELS[value]}</span>
          <span class="help">{LENS_HELP[value]}</span>
          <span class="count">{counts[value]}</span>
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .menu {
    position: relative;
  }

  .trigger {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 30px;
    min-width: 30px;
    padding: 0 7px;
    border-radius: 8px;
    font-size: 12px;
    color: var(--muted);
  }

  .trigger:hover,
  .trigger[aria-expanded="true"] {
    background: var(--hover);
    color: var(--ink);
  }

  .trigger.narrowed {
    background: var(--accent-soft);
    color: var(--accent);
  }

  .pop {
    position: absolute;
    z-index: 15;
    top: calc(100% + 6px);
    right: 0;
    width: 280px;
    padding: 4px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--menu);
    box-shadow: 0 16px 40px -12px rgb(0 0 0 / 45%);
  }

  .pop button {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 0 10px;
    width: 100%;
    padding: 7px 10px;
    border-radius: 8px;
    text-align: left;
  }

  .pop button:hover {
    background: var(--hover);
  }

  .pop button.on {
    background: var(--selected);
  }

  .pop button:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .label {
    font-size: 13px;
    color: var(--ink);
  }

  .help {
    grid-row: 2;
    font-size: 11.5px;
    color: var(--faint);
  }

  .count {
    grid-row: 1 / 3;
    align-self: center;
    font-variant-numeric: tabular-nums;
    font-size: 12px;
    color: var(--ghost);
  }
</style>
