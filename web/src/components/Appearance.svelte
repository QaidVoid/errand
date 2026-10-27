<script lang="ts">
  /**
   * Chooses a theme, and makes new ones.
   *
   * A theme a reader makes starts from a bundled one and changes some of its
   * colours, which is exactly a kage theme file, so one can be imported from
   * kage and exported back to it.
   */
  import {
    apply,
    BUNDLED,
    type BundledName,
    type CustomTheme,
    formatTheme,
    isColor,
    names,
    paint,
    parseTheme,
    resolve,
    save,
    settings,
    showing,
    SYSTEM,
    ThemeError,
    TOKENS,
    type Token,
  } from "../lib/themes.ts";

  let { onClose }: { onClose: () => void } = $props();

  let held = $state(settings());
  /** The theme being edited, as a working copy until it is saved. */
  let editing = $state<CustomTheme | null>(null);
  /** The name it had when editing began, so a rename replaces it. */
  let editingFrom = $state<string | null>(null);
  let importing = $state(false);
  let importName = $state("");
  let importSource = $state("");
  let problem = $state<string | null>(null);

  const all = $derived(names(held));
  const bundledNames = Object.keys(BUNDLED) as BundledName[];

  function keep(next = held): void {
    held = next;
    save($state.snapshot(next));
    apply($state.snapshot(next));
  }

  function choose(name: string): void {
    keep({ ...held, choice: name });
  }

  function setPair(side: "dark" | "light", name: string): void {
    keep({ ...held, pair: { ...held.pair, [side]: name } });
  }

  function swatches(name: string): string[] {
    const colors = resolve(name, held).colors;
    return [colors.bg, colors.surface, colors.ink, colors.accent];
  }

  function freeName(stem: string): string {
    const taken = new Set(all.map((theme) => theme.name));
    if (!taken.has(stem)) return stem;
    for (let at = 2; ; at++) if (!taken.has(`${stem}-${at}`)) return `${stem}-${at}`;
  }

  function startNew(): void {
    const current = showing(held);
    const base = (bundledNames as string[]).includes(current)
      ? (current as BundledName)
      : (held.custom.find((theme) => theme.name === current)?.base ?? "kage-shadow");
    edit({ name: freeName("my-theme"), base, colors: {} }, null);
  }

  function edit(theme: CustomTheme, from: string | null): void {
    problem = null;
    editing = structuredClone($state.snapshot(theme));
    editingFrom = from;
  }

  function preview(theme: CustomTheme): Record<Token, string> {
    return { ...BUNDLED[theme.base].colors, ...theme.colors };
  }

  $effect(() => {
    if (editing === null) return;
    const theme = $state.snapshot(editing);
    paint({ label: theme.name, dark: BUNDLED[theme.base].dark, colors: preview(theme) });
  });

  function setColor(token: Token, value: string): void {
    if (editing === null) return;
    const colors = { ...editing.colors };
    const trimmed = value.trim();
    if (trimmed === "" || trimmed.toLowerCase() === BUNDLED[editing.base].colors[token]) {
      delete colors[token];
    } else if (isColor(trimmed)) {
      colors[token] = trimmed.toLowerCase();
    } else {
      return;
    }
    editing = { ...editing, colors };
  }

  function saveEdit(): void {
    if (editing === null) return;
    const name = editing.name.trim();
    if (!/^[A-Za-z0-9_-]+$/.test(name)) {
      problem = "a name is letters, digits, - and _";
      return;
    }
    if (name !== editingFrom && all.some((theme) => theme.name === name)) {
      problem = `there is already a theme called ${name}`;
      return;
    }
    const theme = { ...$state.snapshot(editing), name };
    const custom = held.custom.filter((other) => other.name !== editingFrom);
    custom.push(theme);
    const renamed = (held.choice === editingFrom || editingFrom === null) ? name : held.choice;
    keep({ ...held, custom, choice: renamed });
    editing = null;
  }

  function cancelEdit(): void {
    editing = null;
    apply($state.snapshot(held));
  }

  function remove(name: string): void {
    const custom = held.custom.filter((theme) => theme.name !== name);
    const choice = held.choice === name ? SYSTEM : held.choice;
    const pair = {
      dark: held.pair.dark === name ? "kage-shadow" : held.pair.dark,
      light: held.pair.light === name ? "kage-dawn" : held.pair.light,
    };
    keep({ ...held, custom, choice, pair });
  }

  function exportTheme(theme: CustomTheme): void {
    const blob = new Blob([formatTheme(theme)], { type: "application/toml" });
    const link = document.createElement("a");
    link.href = URL.createObjectURL(blob);
    link.download = `${theme.name}.toml`;
    link.click();
    URL.revokeObjectURL(link.href);
  }

  async function readFile(event: Event): Promise<void> {
    const file = (event.currentTarget as HTMLInputElement).files?.[0];
    if (file === undefined) return;
    importSource = await file.text();
    if (importName.trim() === "") importName = file.name.replace(/\.toml$/i, "");
  }

  function finishImport(): void {
    problem = null;
    try {
      const theme = parseTheme(freeName(importName.trim() || "imported"), importSource);
      importing = false;
      importSource = "";
      importName = "";
      edit(theme, null);
    } catch (error) {
      problem = error instanceof ThemeError ? error.message : String(error);
    }
  }

  function close(): void {
    if (editing !== null) cancelEdit();
    onClose();
  }
</script>

<div class="scrim" role="presentation" onclick={close}></div>
<div class="dialog" role="dialog" aria-modal="true" aria-label="Appearance">
  <header>
    <h2>{editing === null ? "Appearance" : "Edit theme"}</h2>
    <button class="ghost" onclick={close} aria-label="Close">close</button>
  </header>

  {#if editing !== null}
    <div class="editor">
      <div class="row">
        <label>
          <span>name</span>
          <input bind:value={editing.name} spellcheck="false" />
        </label>
        <label>
          <span>based on</span>
          <select bind:value={editing.base}>
            {#each bundledNames as name}
              <option value={name}>{BUNDLED[name].label}</option>
            {/each}
          </select>
        </label>
      </div>
      <p class="hint">
        Leave a colour blank to keep the base's. Colours are <code>#rrggbb</code> or
        <code>#rrggbbaa</code>, the same as a kage theme's <code>[gui]</code> table.
      </p>
      <div class="tokens">
        {#each TOKENS as token}
          {@const value = editing.colors[token] ?? ""}
          {@const shown = value || BUNDLED[editing.base].colors[token]}
          <label class="token" class:changed={value !== ""}>
            <span class="swatch" style="background: {shown}"></span>
            <span class="key">{token}</span>
            <input
              value={value}
              placeholder={BUNDLED[editing.base].colors[token]}
              spellcheck="false"
              onchange={(event) => setColor(token, event.currentTarget.value)}
            />
            <input
              class="picker"
              type="color"
              value={shown.slice(0, 7)}
              aria-label="Pick {token}"
              oninput={(event) => setColor(token, event.currentTarget.value + shown.slice(7))}
            />
          </label>
        {/each}
      </div>
      {#if problem !== null}<p class="problem" role="alert">{problem}</p>{/if}
      <footer>
        <button class="ghost" onclick={cancelEdit}>cancel</button>
        <button class="primary" onclick={saveEdit}>save and use</button>
      </footer>
    </div>
  {:else if importing}
    <div class="importer">
      <p class="hint">
        Paste a kage theme, or pick its file. Its <code>base</code> and <code>[gui]</code>
        colours are used; the terminal's <code>[colors]</code> and <code>[groups]</code> are not.
      </p>
      <label>
        <span>name</span>
        <input bind:value={importName} placeholder="imported" spellcheck="false" />
      </label>
      <input type="file" accept=".toml,text/plain" onchange={readFile} />
      <textarea
        bind:value={importSource}
        rows="10"
        spellcheck="false"
        placeholder={'base = "kimi-dark"\n\n[gui]\naccent = "#fe8019"'}
      ></textarea>
      {#if problem !== null}<p class="problem" role="alert">{problem}</p>{/if}
      <footer>
        <button class="ghost" onclick={() => (importing = false)}>cancel</button>
        <button class="primary" disabled={importSource.trim() === ""} onclick={finishImport}
          >import</button
        >
      </footer>
    </div>
  {:else}
    <section>
      <h3>Theme</h3>
      <div class="cards">
        <button
          class="card"
          class:on={held.choice === SYSTEM}
          aria-pressed={held.choice === SYSTEM}
          onclick={() => choose(SYSTEM)}
        >
          <span class="preview split">
            {#each [held.pair.dark, held.pair.light] as name}
              <span class="half">
                {#each swatches(name) as color}<i style="background: {color}"></i>{/each}
              </span>
            {/each}
          </span>
          <span class="label">system</span>
        </button>
        {#each all as theme (theme.name)}
          <button
            class="card"
            class:on={held.choice === theme.name}
            aria-pressed={held.choice === theme.name}
            onclick={() => choose(theme.name)}
          >
            <span class="preview">
              {#each swatches(theme.name) as color}<i style="background: {color}"></i>{/each}
            </span>
            <span class="label">{theme.label}</span>
          </button>
        {/each}
      </div>
    </section>

    <section>
      <h3>System follows</h3>
      <div class="row">
        {#each [["dark", true], ["light", false]] as const as [side, dark]}
          <label>
            <span>on a {side} desktop</span>
            <select value={held.pair[side]} onchange={(event) => setPair(side, event.currentTarget.value)}>
              {#each all.filter((theme) => theme.dark === dark) as theme}
                <option value={theme.name}>{theme.label}</option>
              {/each}
            </select>
          </label>
        {/each}
      </div>
    </section>

    <section>
      <h3>Your themes</h3>
      {#if held.custom.length === 0}
        <p class="hint">Themes you make are kept in this browser.</p>
      {/if}
      <ul class="custom">
        {#each held.custom as theme (theme.name)}
          <li>
            <span class="name">{theme.name}</span>
            <span class="base">on {BUNDLED[theme.base].label}</span>
            <button class="ghost" onclick={() => edit(theme, theme.name)}>edit</button>
            <button class="ghost" onclick={() => exportTheme(theme)}>export</button>
            <button class="ghost danger" onclick={() => remove(theme.name)}>delete</button>
          </li>
        {/each}
      </ul>
      <div class="actions">
        <button class="ghost" onclick={startNew}>new theme</button>
        <button
          class="ghost"
          onclick={() => {
            problem = null;
            importing = true;
          }}>import a kage theme</button
        >
      </div>
    </section>
  {/if}
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 20;
    background: var(--overlay);
    backdrop-filter: blur(2px);
  }

  .dialog {
    position: fixed;
    z-index: 21;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    display: flex;
    flex-direction: column;
    width: min(640px, 94vw);
    max-height: 86vh;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: 14px;
    background: var(--menu);
    box-shadow: 0 24px 64px rgb(0 0 0 / 40%);
  }

  header {
    position: sticky;
    top: 0;
    z-index: 1;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 20px 12px;
    background: var(--menu);
    border-bottom: 1px solid var(--subtle);
  }

  h2 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
    color: var(--ink-strong);
  }

  h3 {
    margin: 0 0 10px;
    font-size: 11.5px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--faint);
  }

  section,
  .editor,
  .importer {
    padding: 16px 20px;
  }

  section + section {
    border-top: 1px solid var(--subtle);
  }

  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(132px, 1fr));
    gap: 10px;
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 8px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--fill);
    text-align: left;
  }

  .card:hover {
    border-color: var(--line-strong);
    background: var(--fill-hover);
  }

  .card.on {
    border-color: var(--accent-bd);
    box-shadow: 0 0 0 1px var(--accent-bd);
    background: var(--accent-soft);
  }

  .preview {
    display: flex;
    height: 34px;
    overflow: hidden;
    border-radius: 6px;
    border: 1px solid var(--subtle);
  }

  .preview i,
  .half i {
    flex: 1;
  }

  .preview.split {
    gap: 2px;
    background: var(--line);
  }

  .half {
    display: flex;
    flex: 1;
  }

  .label {
    font-size: 12.5px;
    color: var(--ink);
  }

  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 1;
    min-width: 160px;
    font-size: 12px;
    color: var(--muted);
  }

  input,
  select,
  textarea {
    padding: 7px 9px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--composer-bg);
    color: var(--ink);
    font: inherit;
    font-size: 13px;
  }

  textarea {
    resize: vertical;
    font-family: var(--mono);
    font-size: 12px;
  }

  .hint {
    margin: 0 0 10px;
    font-size: 12.5px;
    color: var(--faint);
  }

  .hint code {
    padding: 1px 4px;
    border-radius: 4px;
    background: var(--code-inline);
  }

  .tokens {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: 4px 12px;
    margin-top: 6px;
  }

  .token {
    flex-direction: row;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .token .key {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    font-family: var(--mono);
    font-size: 11.5px;
    color: var(--faint);
  }

  .token.changed .key {
    color: var(--accent);
  }

  .token input:not(.picker) {
    width: 104px;
    padding: 4px 6px;
    font-family: var(--mono);
    font-size: 11.5px;
  }

  .swatch {
    width: 16px;
    height: 16px;
    flex-shrink: 0;
    border-radius: 4px;
    border: 1px solid var(--line-strong);
  }

  .picker {
    width: 26px;
    height: 24px;
    padding: 0;
    border: none;
    background: none;
    cursor: pointer;
  }

  .custom {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin: 0 0 10px;
    padding: 0;
    list-style: none;
  }

  .custom li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 8px;
    border-radius: 8px;
    background: var(--fill);
  }

  .custom .name {
    font-size: 13px;
    color: var(--ink);
  }

  .custom .base {
    margin-right: auto;
    font-size: 12px;
    color: var(--faint);
  }

  .actions,
  footer {
    display: flex;
    gap: 8px;
  }

  footer {
    justify-content: flex-end;
    margin-top: 14px;
  }

  .importer {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .ghost {
    padding: 5px 10px;
    border-radius: 8px;
    font-size: 12.5px;
    color: var(--muted);
  }

  .ghost:hover {
    background: var(--hover);
    color: var(--ink);
  }

  .ghost.danger:hover {
    color: var(--danger);
  }

  .primary {
    padding: 6px 14px;
    border-radius: 8px;
    background: var(--send-bg);
    color: var(--send-icon);
    font-size: 13px;
    font-weight: 600;
  }

  .primary:hover {
    background: var(--send-bg-hover);
  }

  .primary:disabled {
    background: var(--send-bg-off);
    color: var(--send-icon-off);
    cursor: default;
  }

  .problem {
    margin: 10px 0 0;
    font-size: 12.5px;
    color: var(--danger);
  }
</style>
