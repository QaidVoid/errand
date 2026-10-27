<script lang="ts">
  /**
   * Lists what a session answers to, grouped as the thread's help groups it.
   *
   * Picking one puts it in the composer rather than sending it, since most
   * take an argument and the rest are worth a look before they run.
   */
  import type { CommandInfo } from "../lib/types.ts";
  import Icon from "./Icon.svelte";

  let { commands, onPick }: { commands: CommandInfo[]; onPick: (command: CommandInfo) => void } =
    $props();

  let open = $state(false);
  let query = $state("");
  let root = $state<HTMLElement | null>(null);
  let search = $state<HTMLInputElement | null>(null);

  const groups = $derived.by(() => {
    const wanted = query.trim().toLowerCase().replace(/^!/, "");
    const out = new Map<string, CommandInfo[]>();
    for (const command of commands) {
      const found =
        wanted === "" ||
        command.name.slice(1).startsWith(wanted) ||
        command.summary.toLowerCase().includes(wanted);
      if (found) out.set(command.group, [...(out.get(command.group) ?? []), command]);
    }
    return [...out];
  });

  function pick(command: CommandInfo): void {
    open = false;
    query = "";
    onPick(command);
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
    class="trigger"
    aria-haspopup="listbox"
    aria-expanded={open}
    title="Commands"
    aria-label="Commands"
    onclick={() => (open = !open)}><Icon name="command" /></button
  >

  {#if open}
    <div class="pop" role="listbox" aria-label="Commands">
      <input
        bind:this={search}
        bind:value={query}
        placeholder="Search commands"
        spellcheck="false"
        onkeydown={(event) => {
          const first = groups[0]?.[1][0];
          if (event.key === "Enter" && first !== undefined) {
            event.preventDefault();
            pick(first);
          }
        }}
      />
      <div class="list">
        {#each groups as [title, listed] (title)}
          <p class="group">{title}</p>
          {#each listed as command (command.name)}
            <button type="button" class="option" onclick={() => pick(command)}>
              <code>{command.name}{command.argument === null ? "" : ` ${command.argument}`}</code>
              <span>{command.summary}</span>
            </button>
          {/each}
        {:else}
          <p class="empty">Nothing matches.</p>
        {/each}
      </div>
    </div>
  {/if}
</div>

<style>
  .picker {
    position: relative;
  }

  .trigger {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: 8px;
    color: var(--muted);
  }

  .trigger:hover,
  .trigger[aria-expanded="true"] {
    background: var(--hover);
    color: var(--ink);
  }

  .pop {
    position: absolute;
    z-index: 15;
    bottom: calc(100% + 8px);
    left: 0;
    display: flex;
    flex-direction: column;
    width: min(440px, 88vw);
    max-height: min(440px, 60vh);
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
    gap: 12px;
    width: 100%;
    padding: 6px 8px;
    border-radius: 7px;
    text-align: left;
  }

  .option:hover {
    background: var(--hover);
  }

  code {
    flex-shrink: 0;
    font-size: 12.5px;
    color: var(--accent);
  }

  .option span {
    min-width: 0;
    font-size: 12.5px;
    color: var(--muted);
  }

  .empty {
    margin: 8px;
    font-size: 12.5px;
    color: var(--faint);
  }
</style>
