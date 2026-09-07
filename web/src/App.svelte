<script lang="ts">
  import FileTree from "./components/FileTree.svelte";
  import FileView from "./components/FileView.svelte";
  import SessionList from "./components/SessionList.svelte";
  import ThemeToggle from "./components/ThemeToggle.svelte";
  import Transcript from "./components/Transcript.svelte";
  import { api, streamSession } from "./lib/api.ts";
  import { ago, cacheRate, money, tokens } from "./lib/format.ts";
  import { counts, type Lens, LENS_HELP, LENSES } from "./lib/lens.ts";
  import { read } from "./lib/turns.ts";
  import { nameFrom } from "./lib/naming.ts";
  import type { Entry, FileContents, SessionSummary, Usage } from "./lib/types.ts";

  let sessions = $state<SessionSummary[]>([]);
  let selected = $state<string | null>(null);
  let entries = $state<Entry[]>([]);
  let busy = $state(false);
  let ended = $state(false);
  let waiting = $state<string | null>(null);
  let connected = $state(true);
  /** Whether the selected session has a sandbox running. */
  let live = $state(false);
  /** Entries too old to have been kept, so the gap is admitted to. */
  let dropped = $state(0);
  let usage = $state<Usage | null>(null);
  let draft = $state("");
  let viewing = $state<FileContents | null>(null);
  /** The directory the file pane is showing, shared by the tree and a file. */
  let browsing = $state("");
  let failure = $state<string | null>(null);
  /** What this interface may do, which decides what is worth drawing. */
  let observer = $state(false);
  /** Width of the file pane, kept so a chosen size survives a reload. */
  let filesWidth = $state(Number(localStorage.getItem("files-width") ?? "300"));
  let guildId = $state<string | null>(null);
  /** What the reader is looking for, and what they have narrowed to. */
  let search = $state("");
  let lens = $state<Lens>("all");
  let transcript = $state<ReturnType<typeof Transcript> | null>(null);
  let searchBox = $state<HTMLInputElement | null>(null);
  /** Whether the list of keys is showing. */
  let keysOpen = $state(false);

  /**
   * Whether the search field is open.
   *
   * A field rather than a permanent row: it is a full row on a phone for
   * something used occasionally, and `/` is where a reader reaches for it.
   */
  let searching = $state(false);

  /**
   * Opens the search field and puts the cursor in it.
   *
   * Focused from an effect rather than straight after opening it, because the
   * field does not exist until the next render: focusing before that left the
   * box open with the cursor still on the page behind it, so typing did
   * nothing except trigger the single-key shortcuts.
   */
  function openSearch(): void {
    searching = true;
    wantFocus = true;
  }

  let wantFocus = $state(false);

  $effect(() => {
    const box = searchBox;
    if (!wantFocus || box === null) return;
    wantFocus = false;
    box.focus();
    box.select();
  });

  /** How much the term was found, as the field reports it. */
  let found = $state({ matches: 0, exchanges: 0 });

  /**
   * How much each lens would show.
   *
   * On the control itself, so it says what it will do before it is pressed and
   * so one with nothing to show can be disabled rather than looking broken.
   */
  const lensCounts = $derived(counts(read(entries).turns));

  /**
   * Which exchange is being read, so that moving between them shows.
   *
   * Null until the transcript says, which is the case for a session with no
   * exchanges recorded.
   */
  let reading = $state<{ at: number; of: number } | null>(null);

  /** What each key does, which is also what the help lists. */
  const KEYS: { key: string; does: string }[] = [
    { key: "/", does: "search this session" },
    { key: "j", does: "next exchange" },
    { key: "k", does: "previous exchange" },
    { key: "o", does: "expand every tool call" },
    { key: "u", does: "collapse every tool call" },
    { key: "n", does: "next session" },
    { key: "p", does: "previous session" },
    { key: "g", does: "jump to the end" },
    { key: "?", does: "show these keys" },
    { key: "Escape", does: "clear the search, or close this" },
  ];

  /** True while the keys should type rather than act. */
  function typing(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) return false;
    return (
      target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable
    );
  }

  function moveSession(by: number): void {
    if (sessions.length === 0) return;
    const at = sessions.findIndex((session) => session.id === selected);
    const next = sessions[Math.max(0, Math.min(sessions.length - 1, at + by))];
    if (next !== undefined && next.id !== selected) select(next.id);
  }

  function onKeys(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      if (keysOpen) keysOpen = false;
      else if (drawer !== null) drawer = null;
      else if (search.length > 0) search = "";
      else if (searching) searching = false;
      else if (typing(event.target)) (event.target as HTMLElement).blur();
      return;
    }

    // A composer with focus is being typed into, so the keys are letters.
    if (typing(event.target)) return;
    if (event.metaKey || event.ctrlKey || event.altKey) return;

    const act: Record<string, () => void> = {
      "/": openSearch,
      "?": () => (keysOpen = !keysOpen),
      j: () => transcript?.step(1),
      k: () => transcript?.step(-1),
      o: () => transcript?.allCalls(true),
      u: () => transcript?.allCalls(false),
      n: () => moveSession(1),
      p: () => moveSession(-1),
      g: () => transcript?.toEnd(),
    };

    const run = act[event.key];
    if (run === undefined) return;
    event.preventDefault();
    run();
  }

  /**
   * Which side panes are pulled out.
   *
   * Only used below the width where they fit as columns. The session is never
   * a drawer, because it is the thing being read.
   */
  let drawer = $state<"sessions" | "files" | null>(null);

  let starting = $state(false);
  let newProject = $state("");
  let newPrompt = $state("");

  let stop: (() => void) | null = null;

  /**
   * Entries that have arrived but not yet been drawn.
   *
   * A session is replayed one event per entry, so drawing each as it lands
   * means one full redraw per entry: a thousand of them to open a long session,
   * and one for every line a running agent produces. They are collected and
   * drawn together on the next frame instead.
   */
  let pending: Entry[] = [];
  let flushing = 0;

  function drawSoon(): void {
    if (flushing !== 0) return;
    flushing = requestAnimationFrame(() => {
      flushing = 0;
      if (pending.length === 0) return;
      entries = entries.concat(pending);
      pending = [];
    });
  }

  function stopDrawing(): void {
    if (flushing !== 0) cancelAnimationFrame(flushing);
    flushing = 0;
    pending = [];
  }

  function fail(error: unknown): void {
    failure = error instanceof Error ? error.message : String(error);
  }

  const current = $derived(sessions.find((session) => session.id === selected));

  /** The session named in the address, so a reload or a share reopens it. */
  function inAddress(): string | null {
    return new URLSearchParams(location.search).get("session");
  }

  /**
   * The exchange named in the address, so a link can cite one rather than a
   * whole session. Kept in step with the fragment, which is what the gutter
   * number sets when it is followed.
   */
  function turnInAddress(): number | undefined {
    const found = /^#turn-(\d+)$/.exec(location.hash);
    return found === null ? undefined : Number(found[1]);
  }

  let wantedTurn = $state<number | undefined>(turnInAddress());

  $effect(() => {
    const onHash = (): void => {
      wantedTurn = turnInAddress();
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  });

  function remember(id: string): void {
    if (inAddress() === id) return;
    const url = new URL(location.href);
    url.searchParams.set("session", id);
    // An exchange belongs to the session it is in, so the fragment goes when
    // another session is opened rather than pointing into the wrong one.
    url.hash = "";
    history.pushState({ session: id }, "", url);
    wantedTurn = undefined;
  }

  async function refresh(): Promise<void> {
    try {
      sessions = await api.sessions();
    } catch (error) {
      fail(error);
      return;
    }
    if (selected !== null && !sessions.some((session) => session.id === selected)) return;
    if (selected !== null || sessions.length === 0) return;

    // The address wins over the first in the list, so a reload lands back on
    // the session that was open rather than on whichever sorts first.
    const asked = inAddress();
    select(asked !== null && sessions.some((session) => session.id === asked) ? asked : sessions[0]!.id);
  }

  function select(id: string, fromAddress = false): void {
    if (!fromAddress) remember(id);
    stop?.();
    stop = null;
    stopDrawing();
    selected = id;
    entries = [];
    dropped = 0;
    usage = null;
    browsing = "";
    search = "";
    lens = "all";
    viewing = null;
    busy = false;
    ended = false;
    waiting = null;
    drawer = null;

    // A stopped session has nothing to stream until it is sent to, so it is
    // shown from the list alone rather than by opening a stream that is not
    // there.
    live = sessions.find((session) => session.id === id)?.live ?? false;

    // A stopped session has no stream. What it said was written down, so it is
    // read back rather than shown as an empty pane.
    if (!live) {
      void api
        .transcript(id)
        .then((stored) => {
          if (selected !== id) return;
          entries = stored.entries;
          dropped = stored.dropped;
          // What it cost was written down too, so a stopped session still
          // reports its model and how much context it was carrying.
          usage = stored.usage ?? null;
        })
        .catch(() => undefined);
      return;
    }

    stop = streamSession(id, {
      // The daemon replays from the beginning on every connect, so what was
      // shown before is discarded rather than added to.
      reset: () => {
        stopDrawing();
        entries = [];
        busy = false;
        ended = false;
        waiting = null;
        usage = null;
      },
      entry: (entry) => {
        pending.push(entry);
        drawSoon();
      },
      state: (state) => {
        if (state.busy !== undefined) busy = state.busy;
        if (state.waiting !== undefined) waiting = state.waiting;
        if (state.ended !== undefined) ended = state.ended;
        if (state.usage !== undefined) usage = state.usage;
      },
      connected: (up) => {
        connected = up;
      },
    });
  }

  async function send(): Promise<void> {
    const text = draft.trim();
    if (text.length === 0 || selected === null) return;
    const id = selected;
    draft = "";
    failure = null;

    const wasLive = live;
    try {
      await api.prompt(id, text);
    } catch (error) {
      fail(error);
      return;
    }

    // Sending to a stopped session starts it again, so the stream only exists
    // once the message has been accepted.
    if (!wasLive) {
      await refresh();
      if (selected === id) select(id);
    }
  }

  function onKey(event: KeyboardEvent): void {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void send();
    }
  }

  async function start(): Promise<void> {
    const project = newProject.trim();
    const prompt = newPrompt.trim();
    if (project.length === 0 || prompt.length === 0) return;

    failure = null;
    try {
      const started = await api.start(project, prompt);
      newProject = "";
      newPrompt = "";
      starting = false;
      await refresh();
      if (started.id !== undefined) select(started.id);
    } catch (error) {
      fail(error);
    }
  }

  /** Drags the divider, within bounds that keep both sides usable. */
  function grab(event: PointerEvent): void {
    event.preventDefault();
    const handle = event.currentTarget as HTMLElement;
    handle.setPointerCapture(event.pointerId);

    const move = (moved: PointerEvent) => {
      filesWidth = Math.min(760, Math.max(200, window.innerWidth - moved.clientX));
    };
    const drop = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", drop);
      localStorage.setItem("files-width", String(filesWidth));
    };

    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", drop);
  }

  async function open(path: string): Promise<void> {
    if (selected === null) return;
    failure = null;
    try {
      viewing = await api.file(selected, path);
    } catch (error) {
      fail(error);
    }
  }

  $effect(() => {
    // Back and forward move between sessions, because each is its own address.
    const back = () => {
      const asked = inAddress();
      if (asked !== null && asked !== selected) select(asked, true);
    };
    window.addEventListener("popstate", back);
    window.addEventListener("keydown", onKeys);

    void api
      .describe()
      .then((info) => {
        observer = info.observer;
        guildId = info.guildId;
      })
      .catch(() => undefined);
    void refresh();
    const timer = setInterval(() => void refresh(), 4000);
    return () => {
      window.removeEventListener("popstate", back);
      window.removeEventListener("keydown", onKeys);
      stopDrawing();
      clearInterval(timer);
      stop?.();
    };
  });
</script>

<div class="shell" data-drawer={drawer ?? "none"} style="--files: {filesWidth}px">
  <aside class="sidebar">
    <header>
      <h1>errand</h1>
      <ThemeToggle />
      {#if observer}
        <span class="badge">read only</span>
      {:else}
        <button
          class="new"
          onclick={() => (starting = !starting)}
          aria-expanded={starting}
          aria-label="Start a session">+</button
        >
      {/if}
    </header>

    {#if starting && !observer}
      <form
        class="start"
        onsubmit={(event) => {
          event.preventDefault();
          void start();
        }}
      >
        <input bind:value={newProject} placeholder="project" aria-label="Project" />
        <textarea
          bind:value={newPrompt}
          rows="2"
          placeholder="What should it do?"
          aria-label="Opening prompt"
        ></textarea>
        <button
          type="submit"
          disabled={newProject.trim().length === 0 || newPrompt.trim().length === 0}>Start</button
        >
      </form>
    {/if}

    <SessionList {sessions} {selected} onSelect={select} />
  </aside>

  <main>
    {#if selected === null}
      <p class="blank">No session selected.</p>
    {:else}
      <header class="bar">
        <button class="pull" data-opens="sessions" onclick={() => (drawer = "sessions")} aria-label="Sessions"
          >sessions</button
        >
        <span class="named" title={current?.opening ?? current?.project ?? ""}>
          {nameFrom(current?.opening)?.title ?? current?.project ?? selected}
        </span>

        {#if reading !== null}
          <span class="where">turn {reading.at} of {reading.of}</span>
        {:else if usage !== null && usage.turns > 0}
          <span class="where">{usage.turns} turns</span>
        {/if}
        {#if usage !== null && usage.turns > 0}
          <span class="spend">{money(usage.cost)}</span>
        {/if}

        <button class="pull" data-opens="files" onclick={() => (drawer = "files")} aria-label="Files">files</button
        >
        <button
          class="icon"
          aria-label="Search this session"
          aria-expanded={searching}
          title="Search this session"
          onclick={openSearch}>/</button
        >
        <button class="icon" title="Keyboard shortcuts" onclick={() => (keysOpen = true)}>?</button>
      </header>

      <!-- One quiet line for what holds for the whole session, since none of it
           changes while it is read. -->
      <p class="facts">
        {#if usage !== null && usage.model !== undefined}
          <span>{usage.model}</span>
        {/if}
        {#if usage !== null && usage.contextWindow}
          <span title="{tokens(usage.contextTokens)} of {tokens(usage.contextWindow)}">
            {Math.round((usage.contextTokens / usage.contextWindow) * 100)}% of {tokens(
              usage.contextWindow,
            )}
          </span>
        {/if}
        {#if usage !== null && usage.turns > 0}
          <span title="{tokens(usage.totalTokens)} tokens">
            {tokens(usage.totalTokens)} tokens{cacheRate(usage.input, usage.cacheRead) === undefined
              ? ""
              : `, ${cacheRate(usage.input, usage.cacheRead)}% cached`}
          </span>
        {/if}
        {#if current !== undefined}
          <span class="when" title={new Date(current.lastActiveAt).toLocaleString()}>
            {ago(current.lastActiveAt)}
          </span>
        {/if}
        {#if guildId !== null && current?.threadId !== undefined}
          <a
            class="thread"
            href={`https://discord.com/channels/${guildId}/${current.threadId}`}
            target="_blank"
            rel="noreferrer">open the thread</a
          >
        {/if}
      </p>

      {#if !connected}
        <p class="dropped">Reconnecting to the daemon.</p>
      {/if}

      {#if !live && !observer}
        <p class="resumable">
          This session is not running. Send a message to pick it up where it stopped.
        </p>
      {/if}

      <div class="reading">
        {#if searching}
          <div class="finding">
            <input
              class="find"
              bind:this={searchBox}
              bind:value={search}
              type="search"
              placeholder="Search this session"
              aria-label="Search this session"
              onblur={() => {
                if (search.length === 0) searching = false;
              }}
            />
            <!-- Said as it is typed, so an empty result is plainly the search
                 and not the session having nothing in it. -->
            <span class="tally" aria-live="polite">
              {#if search.trim().length === 0}
                type to search
              {:else if found.matches === 0}
                no matches
              {:else}
                {found.matches} in {found.exchanges} exchange{found.exchanges === 1 ? "" : "s"}
              {/if}
            </span>
            <button class="clear" title="Clear the search" aria-label="Clear the search"
              onclick={() => {
                search = "";
                searching = false;
              }}>x</button
            >
          </div>
        {/if}
        <div class="lenses" role="group" aria-label="Narrow this session to">
          {#each LENSES as value}
            {@const count = lensCounts[value]}
            <button
              class:on={lens === value}
              aria-pressed={lens === value}
              disabled={count === 0 && value !== "all"}
              title={LENS_HELP[value]}
              onclick={() => (lens = value)}
            >
              {value}
              <span class="count">{count}</span>
            </button>
          {/each}
        </div>
      </div>

      <!-- Keyed on the session, so switching gives a fresh pane rather than one
           holding the last session's scroll position and follow state. -->
      {#key selected}
        <Transcript
          bind:this={transcript}
          {entries}
          {busy}
          {waiting}
          {ended}
          {dropped}
          {search}
          bind:lens
          onFound={(matches, exchanges) => (found = { matches, exchanges })}
          onOpenFile={open}
          wanted={wantedTurn}
          onReading={(at, of) => (reading = of === 0 ? null : { at, of })}
        />
      {/key}

      {#if failure !== null}
        <p class="failure" role="alert">
          <span>{failure}</span>
          <button onclick={() => (failure = null)} aria-label="Dismiss">dismiss</button>
        </p>
      {/if}

      {#if observer}
        <p class="resumable">This interface is read only. Send messages from the chat thread.</p>
      {:else}
        <form
        class="composer"
        onsubmit={(event) => {
          event.preventDefault();
          void send();
        }}
      >
        <textarea
          bind:value={draft}
          onkeydown={onKey}
          rows="1"
          disabled={ended}
          placeholder={ended
            ? "This session has ended."
            : live
              ? "Send a message, or !help"
              : "Send a message to resume this session"}
          aria-label="Message"
        ></textarea>
        <button type="submit" disabled={ended || draft.trim().length === 0}>Send</button>
        </form>
      {/if}
    {/if}
  </main>

  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    class="grip"
    role="separator"
    aria-orientation="vertical"
    aria-label="Resize the file pane"
    onpointerdown={grab}
    ondblclick={() => {
      filesWidth = 300;
      localStorage.setItem("files-width", "300");
    }}
  ></div>

  <aside class="files">
    {#if selected === null}
      <p class="blank">No session selected.</p>
    {:else if viewing === null}
      <FileTree session={selected} bind:at={browsing} onOpen={open} />
    {:else}
      <FileView
        session={selected}
        file={viewing}
        onClose={() => (viewing = null)}
        onOpenPath={(path) => {
          browsing = path;
          viewing = null;
        }}
      />
    {/if}
  </aside>

  {#if drawer !== null}
    <button class="scrim" aria-label="Close" onclick={() => (drawer = null)}></button>
  {/if}

  {#if keysOpen}
    <div
      class="keys"
      role="dialog"
      aria-label="Keyboard shortcuts"
      aria-modal="true"
      onclick={() => (keysOpen = false)}
      onkeydown={() => undefined}
      tabindex="-1"
    >
      <div class="sheet">
        <h2>Keys</h2>
        <div class="doing">
          <button onclick={() => transcript?.allCalls(true)}>expand calls</button>
          <button onclick={() => transcript?.allCalls(false)}>collapse calls</button>
        </div>
        <dl>
          {#each KEYS as entry}
            <div><dt>{entry.key}</dt><dd>{entry.does}</dd></div>
          {/each}
        </dl>
      </div>
    </div>
  {/if}

</div>

<style>
  .shell {
    display: grid;
    grid-template-columns: 232px minmax(0, 1fr) 6px clamp(200px, var(--files, 300px), 38vw);
    height: 100%;
    min-height: 0;
    overflow: hidden;
  }

  .sidebar,
  .files {
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
    background: var(--bg-inset);
  }

  .sidebar {
    border-right: 1px solid var(--line);
  }

  .files {
    border-left: 1px solid var(--line);
  }

  .grip {
    cursor: col-resize;
    background: var(--line);
    transition: background 120ms ease;
    touch-action: none;
  }

  .grip:hover {
    background: var(--accent);
  }

  header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 14px 14px 10px;
    border-bottom: 1px solid var(--line-soft);
  }

  header h1 {
    margin-right: auto;
  }

  h1 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-dim);
  }

  .new {
    width: 22px;
    height: 22px;
    border-radius: var(--radius-sm);
    color: var(--text-faint);
    font-size: 15px;
    line-height: 1;
  }

  .new:hover {
    background: var(--bg-raised);
    color: var(--text);
  }

  .start {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px;
    border-bottom: 1px solid var(--line-soft);
  }

  main {
    display: flex;
    flex-direction: column;
    min-width: 0;
    /* Without this a flex child cannot shrink below its content, so the
       transcript never scrolls and the whole page grows instead. */
    min-height: 0;
    overflow: hidden;
  }

  .blank {
    margin: auto;
    padding: 24px;
    color: var(--text-faint);
    font-size: 12.5px;
  }

  .dropped,
  .failure {
    margin: 0;
    padding: 8px 22px;
    font-size: 12.5px;
  }

  .dropped {
    color: var(--warn);
    border-bottom: 1px solid var(--line-soft);
  }

  .bar {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 22px;
    border-bottom: 1px solid var(--line-soft);
  }

  .named {
    flex: 1;
    min-width: 0;
    font-size: var(--type-work);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* What changes while reading, which is why it sits beside the name. */
  .where,
  .spend {
    flex-shrink: 0;
    font-family: var(--mono);
    font-size: var(--type-receipt);
    color: var(--text-faint);
  }

  .icon {
    flex-shrink: 0;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-sm);
    font-family: var(--mono);
    font-size: var(--type-receipt);
    color: var(--text-faint);
  }

  .icon:hover,
  .icon[aria-expanded="true"] {
    background: var(--bg-raised);
    color: var(--text);
  }

  .when {
    color: var(--text-faint);
  }

  /*
   * What holds for the whole session, said once and quietly. None of it
   * changes while the session is read, so none of it earns a row of its own.
   */
  .facts {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 2px 14px;
    margin: 0;
    padding: 5px 22px 7px;
    border-bottom: 1px solid var(--line-soft);
    font-size: var(--type-receipt);
    color: var(--text-faint);
  }

  .facts span {
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .thread {
    color: var(--text-faint);
  }

  .thread:hover {
    color: var(--accent);
  }

  .badge {
    font-size: 11px;
    color: var(--text-faint);
    border: 1px solid var(--line);
    border-radius: 999px;
    padding: 2px 8px;
  }

  .reading {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    padding: 8px 18px;
    border-bottom: 1px solid var(--line-soft);
  }

  .finding {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
    min-width: 200px;
  }

  .find {
    flex: 1;
    min-width: 120px;
    padding: 5px 10px;
    font-size: 12.5px;
    background: var(--bg-inset);
  }

  /* What the search found, beside the box rather than somewhere else. */
  .tally {
    font-size: 11.5px;
    color: var(--text-faint);
    white-space: nowrap;
  }

  .clear {
    padding: 2px 7px;
    border-radius: var(--radius-sm);
    font-size: 11.5px;
    color: var(--text-faint);
  }

  .clear:hover {
    color: var(--text);
  }

  .lenses,
  .doing {
    display: flex;
    gap: 2px;
    flex-shrink: 0;
  }

  .doing {
    margin-bottom: 10px;
  }

  .lenses button,
  .doing button {
    padding: 3px 8px;
    border-radius: var(--radius-sm);
    font-size: 11.5px;
    color: var(--text-faint);
  }

  .lenses button:hover,
  .doing button:hover {
    color: var(--text);
  }

  .lenses button.on {
    background: var(--bg-raised);
    color: var(--text);
  }

  /* How much the lens would show, so it says what it does before it is used. */
  .lenses .count {
    margin-left: 5px;
    font-variant-numeric: tabular-nums;
    color: var(--text-faint);
  }

  .lenses button.on .count {
    color: var(--text-dim);
  }

  .lenses button:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .lenses button:disabled:hover {
    color: var(--text-faint);
  }

  .keys {
    position: fixed;
    inset: 0;
    z-index: 10;
    display: grid;
    place-items: center;
    background: var(--overlay);
  }

  .sheet {
    width: min(360px, 90vw);
    max-height: 80vh;
    overflow: auto;
    padding: 18px 20px;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--bg-raised);
  }

  .sheet h2 {
    margin: 0 0 12px;
    font-size: 13px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-dim);
  }

  .sheet dl {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 0;
  }

  .sheet dl div {
    display: flex;
    gap: 12px;
  }

  .sheet dt {
    flex-shrink: 0;
    width: 62px;
    font-family: var(--mono);
    font-size: 12px;
    color: var(--accent);
  }

  .sheet dd {
    margin: 0;
    font-size: 12.5px;
    color: var(--text-dim);
  }

  .resumable {
    margin: 0;
    padding: 10px 22px;
    font-size: 12.5px;
    color: var(--text-faint);
    border-bottom: 1px solid var(--line-soft);
  }

  .failure {
    display: flex;
    align-items: baseline;
    gap: 12px;
    color: var(--bad);
    background: color-mix(in srgb, var(--bad) 10%, transparent);
    border-top: 1px solid color-mix(in srgb, var(--bad) 30%, transparent);
  }

  .failure span {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .failure button {
    flex-shrink: 0;
    font-size: 11.5px;
    color: var(--bad);
    text-decoration: underline;
  }

  .composer {
    display: flex;
    gap: 8px;
    padding: 12px 16px 16px;
    border-top: 1px solid var(--line);
    background: var(--bg);
  }

  input,
  textarea {
    color: var(--text);
    background: var(--bg-raised);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 8px 10px;
    font: inherit;
  }

  input::placeholder,
  textarea::placeholder {
    color: var(--text-faint);
  }

  .composer textarea {
    flex: 1;
    resize: none;
    min-height: 40px;
    max-height: 180px;
    background: var(--bg-inset);
    field-sizing: content;
  }

  .start textarea {
    resize: none;
  }

  .composer button,
  .start button {
    padding: 8px 16px;
    border-radius: var(--radius);
    background: var(--accent);
    color: var(--accent-ink);
    font-weight: 600;
    transition: opacity 120ms ease;
  }

  .composer button:disabled,
  .start button:disabled {
    opacity: 0.35;
    cursor: default;
  }

  /* The handles exist only where a pane is a drawer rather than a column. */
  .pull {
    display: none;
    flex-shrink: 0;
    padding: 3px 8px;
    border: 1px solid var(--line-soft);
    border-radius: var(--radius-sm);
    font-size: 11.5px;
    color: var(--text-faint);
  }

  .pull:hover {
    color: var(--text);
  }

  .scrim {
    position: fixed;
    inset: 0;
    z-index: 8;
    background: var(--overlay);
    border-radius: 0;
    cursor: default;
  }

  /*
   * Below three columns the files become a drawer rather than a narrower
   * column, because past this width taking more from the session costs more
   * than the pane is worth.
   */
  @media (max-width: 1080px) {
    .shell {
      grid-template-columns: 208px minmax(0, 1fr);
    }

    .grip {
      display: none;
    }

    .files {
      position: fixed;
      z-index: 9;
      top: 0;
      right: 0;
      bottom: 0;
      width: min(360px, 86vw);
      border-left: 1px solid var(--line);
      transform: translateX(101%);
      transition: transform 160ms ease;
    }

    .shell[data-drawer="files"] .files {
      transform: none;
    }

    .pull[data-opens="files"] {
      display: inline-block;
    }
  }

  /* Narrower still, and the session is the only column left. */
  @media (max-width: 780px) {
    .shell {
      grid-template-columns: minmax(0, 1fr);
    }

    .sidebar {
      position: fixed;
      z-index: 9;
      top: 0;
      left: 0;
      bottom: 0;
      width: min(320px, 84vw);
      border-right: 1px solid var(--line);
      transform: translateX(-101%);
      transition: transform 160ms ease;
    }

    .shell[data-drawer="sessions"] .sidebar {
      transform: none;
    }

    .pull[data-opens="sessions"] {
      display: inline-block;
    }

    .bar .named {
      min-width: 0;
    }

    .composer {
      padding: 10px 12px;
    }

    .bar,
    .facts,
    .dropped,
    .failure,
    .resumable {
      padding-left: 12px;
      padding-right: 12px;
    }

  }

  /*
   * The time and the cost are both in the session list, so on a screen this
   * narrow they give way rather than wrapping the line they sit on.
   */
  @media (max-width: 560px) {
    .facts {
      gap: 2px 12px;
    }

    .facts .when,
    .spend {
      display: none;
    }
  }
</style>
