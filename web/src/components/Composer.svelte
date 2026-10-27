<script lang="ts">
  /**
   * Where a message is written, with the model and the commands to hand.
   *
   * One composer for both places a person writes: the home page, where what
   * they write starts a session, and a session, where it is sent to it.
   * Typing `!` suggests the commands that start that way, and Tab takes the
   * first of them.
   */
  import type { Snippet } from "svelte";
  import type { CommandInfo, ModelCatalog } from "../lib/types.ts";
  import CommandPicker from "./CommandPicker.svelte";
  import Icon from "./Icon.svelte";
  import ModelPicker from "./ModelPicker.svelte";

  let {
    draft = $bindable(""),
    placeholder,
    disabled = false,
    roomy = false,
    commands = null,
    catalog,
    model,
    allowDefault = false,
    onModel,
    onSend,
    tools,
  }: {
    draft?: string;
    placeholder: string;
    disabled?: boolean;
    /** Taller, for the home page where it is the only thing on screen. */
    roomy?: boolean;
    /** What a session answers to, or null where there is no session yet. */
    commands?: CommandInfo[] | null;
    catalog: ModelCatalog | null;
    model: string | null;
    allowDefault?: boolean;
    onModel: (name: string | null) => void;
    onSend: () => void;
    /** Anything more the toolbar should carry, after the model. */
    tools?: Snippet;
  } = $props();

  let box = $state<HTMLTextAreaElement | null>(null);

  /**
   * Commands that start with what has been typed, while the first word is
   * still being typed. Once a space follows it the command is chosen, and the
   * list would only cover what the argument is being written over.
   */
  const suggestions = $derived(
    commands !== null && /^![^\s]*$/.test(draft)
      ? commands.filter((command) => command.name.startsWith(draft)).slice(0, 8)
      : [],
  );

  function complete(command: CommandInfo): void {
    draft = command.argument === null ? command.name : `${command.name} `;
    box?.focus();
  }

  function onKey(event: KeyboardEvent): void {
    const first = suggestions[0];
    if (event.key === "Tab" && first !== undefined) {
      event.preventDefault();
      complete(first);
      return;
    }
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      if (!disabled && draft.trim().length > 0) onSend();
    }
  }
</script>

<div class="wrap">
  {#if suggestions.length > 0}
    <ul class="suggestions" role="listbox" aria-label="Commands">
      {#each suggestions as command, index (command.name)}
        <li role="option" aria-selected={index === 0}>
          <button type="button" onclick={() => complete(command)}>
            <code>{command.name}{command.argument === null ? "" : ` ${command.argument}`}</code>
            <span>{command.summary}</span>
          </button>
        </li>
      {/each}
    </ul>
  {/if}

  <form
    class="composer"
    class:roomy
    onsubmit={(event) => {
      event.preventDefault();
      if (!disabled && draft.trim().length > 0) onSend();
    }}
  >
    <textarea
      bind:this={box}
      bind:value={draft}
      onkeydown={onKey}
      rows={roomy ? 3 : 1}
      {disabled}
      {placeholder}
      aria-label="Message"
    ></textarea>
    <div class="toolbar">
      {#if commands !== null}
        <CommandPicker {commands} onPick={complete} />
      {/if}
      <ModelPicker {catalog} current={model} {allowDefault} onPick={onModel} />
      {@render tools?.()}
      <button
        class="send"
        type="submit"
        disabled={disabled || draft.trim().length === 0}
        aria-label="Send"><Icon name="send" /></button
      >
    </div>
  </form>
</div>

<style>
  .wrap {
    max-width: var(--measure);
    margin: 0 auto;
  }

  .composer {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 10px 10px 8px 14px;
    border: 1px solid var(--composer-line);
    border-radius: 18px;
    background: var(--composer-bg);
    box-shadow: 0 8px 28px -12px rgb(0 0 0 / 30%);
    transition: border-color 120ms ease;
  }

  .composer:focus-within {
    border-color: var(--composer-focus-line);
  }

  textarea {
    min-height: 26px;
    max-height: 240px;
    padding: 2px 4px 2px 0;
    border: none;
    background: transparent;
    color: var(--ink);
    font: inherit;
    resize: none;
    field-sizing: content;
  }

  /* On the home page there is room below and a heading above, so menus open
     downward rather than over the heading. */
  .roomy :global(.pop) {
    top: calc(100% + 8px);
    bottom: auto;
  }

  .roomy textarea {
    min-height: 72px;
    font-size: 15px;
  }

  textarea:focus {
    outline: none;
  }

  textarea::placeholder {
    color: var(--faint);
  }

  .toolbar {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    margin-left: -6px;
  }

  .send {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    margin-left: auto;
    flex-shrink: 0;
    border-radius: 10px;
    background: var(--send-bg);
    color: var(--send-icon);
    transition: background 120ms ease;
  }

  .send:hover {
    background: var(--send-bg-hover);
  }

  .send:disabled {
    background: var(--send-bg-off);
    color: var(--send-icon-off);
    cursor: default;
  }

  .suggestions {
    margin: 0 0 6px;
    padding: 4px;
    list-style: none;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--menu);
    box-shadow: 0 12px 32px -12px rgb(0 0 0 / 35%);
  }

  .suggestions button {
    display: flex;
    align-items: baseline;
    gap: 12px;
    width: 100%;
    padding: 6px 10px;
    border-radius: 8px;
    text-align: left;
  }

  .suggestions li[aria-selected="true"] button,
  .suggestions button:hover {
    background: var(--hover);
  }

  .suggestions code {
    flex-shrink: 0;
    color: var(--accent);
    font-size: 12.5px;
  }

  .suggestions span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12.5px;
    color: var(--muted);
  }
</style>
