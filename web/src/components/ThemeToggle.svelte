<script lang="ts">
  /**
   * Chooses a theme, or hands the choice back to the system.
   *
   * Three states rather than a switch, because "follow the system" is a real
   * answer and a two-way switch has nowhere to put it.
   */
  import { apply, type Choice, choice, current, watch } from "../lib/theme.ts";

  let chosen = $state<Choice>(choice());
  let showing = $state(current());

  $effect(() => watch((theme) => (showing = theme)));

  function pick(next: Choice): void {
    chosen = next;
    apply(next);
    showing = current();
  }

  const options: { value: Choice; label: string }[] = [
    { value: "light", label: "light" },
    { value: "system", label: "auto" },
    { value: "dark", label: "dark" },
  ];
</script>

<fieldset class="theme" aria-label="Theme">
  {#each options as option}
    <button
      class:on={chosen === option.value}
      aria-pressed={chosen === option.value}
      title={option.value === "system" ? `Follow the system, now ${showing}` : option.label}
      onclick={() => pick(option.value)}
    >
      {option.label}
    </button>
  {/each}
</fieldset>

<style>
  .theme {
    display: flex;
    gap: 2px;
    margin: 0;
    padding: 2px;
    border: 1px solid var(--line-soft);
    border-radius: var(--radius-sm);
    background: var(--bg-inset);
  }

  button {
    padding: 2px 7px;
    border-radius: 4px;
    font-size: 11px;
    color: var(--text-faint);
  }

  button:hover {
    color: var(--text);
  }

  button.on {
    background: var(--bg-raised);
    color: var(--text);
  }
</style>
