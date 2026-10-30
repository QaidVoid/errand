<script lang="ts">
  import Appearance from "./components/Appearance.svelte";
  import Composer from "./components/Composer.svelte";
  import FileTree from "./components/FileTree.svelte";
  import Icon from "./components/Icon.svelte";
  import LensMenu from "./components/LensMenu.svelte";
  import FileView from "./components/FileView.svelte";
  import SessionList from "./components/SessionList.svelte";
  import Transcript from "./components/Transcript.svelte";
  import { api, streamSession } from "./lib/api.ts";
  import { ago, cacheRate, money, tokens } from "./lib/format.ts";
  import { counts, type Lens, LENS_HELP, LENSES } from "./lib/lens.ts";
  import { read } from "./lib/turns.ts";
  import { nameFrom } from "./lib/naming.ts";
  import type {
    CommandInfo,
    Entry,
    FileContents,
    ModelCatalog,
    SessionSummary,
    Usage,
    Visitor,
  } from "./lib/types.ts";

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
  /** Whether changing anything takes a Discord login. */
  let login = $state(false);
  /** Who this browser is logged in as. */
  let visitor = $state<Visitor | null>(null);
  /** Whether this browser may start and prompt sessions. */
  let mayAct = $state(false);
  let appearanceOpen = $state(false);
  let catalog = $state<ModelCatalog | null>(null);
  let commandList = $state<CommandInfo[]>([]);
  /** What the home page will start, as it is being written. */
  let homeDraft = $state("");
  /** What a command run from the home page answered, once it has. */
  let homeAnswer = $state<string | null>(null);
  /** True while a command run from the home page is still working. */
  let homeWorking = $state(false);
  let newModel = $state<string | null>(null);

  /** Whether this browser may start and prompt sessions, whatever made it so. */
  const canAct = $derived(!observer && (!login || mayAct));

  /** Switches the open session's model the way the thread would, with `!model`. */
  async function switchModel(name: string | null): Promise<void> {
    if (name === null) return;
    draft = `!model ${name}`;
    await send();
  }

  /** Back to the home page, where what is written starts a session. */
  function goHome(): void {
    stop?.();
    stop = null;
    stopDrawing();
    selected = null;
    entries = [];
    usage = null;
    viewing = null;
    drawer = null;
    const url = new URL(location.href);
    url.searchParams.delete("session");
    url.hash = "";
    history.pushState({}, "", url);
  }
  /** Whether the list shows only this visitor's sessions, and only running ones. */
  let onlyMine = $state(localStorage.getItem("only-mine") === "true");
  let onlyRunning = $state(localStorage.getItem("only-running") === "true");

  function setFilter(key: "only-mine" | "only-running", on: boolean): void {
    if (key === "only-mine") onlyMine = on;
    else onlyRunning = on;
    localStorage.setItem(key, String(on));
  }

  /**
   * The sessions the list shows. Mine means owned by the logged-in account,
   * so without a login there is nobody for it to mean and it is not applied.
   */
  const listed = $derived(
    sessions.filter(
      (session) =>
        (!onlyMine || visitor === null || session.owner === visitor.id) &&
        (!onlyRunning || session.live),
    ),
  );
  /** Whether the file pane is showing, kept so the choice survives a reload. */
  let filesOpen = $state(localStorage.getItem("files-open") === "true");

  /** What came back from Discord, said once and then taken out of the address. */
  const LOGIN_RESULTS: Record<string, string> = {
    denied: "Login was cancelled on Discord.",
    expired: "That login took too long or was started in another tab. Try again.",
    failed: "Discord could not confirm who you are. Try again in a moment.",
  };

  function takeLoginResult(): void {
    const url = new URL(location.href);
    const result = url.searchParams.get("login");
    if (result === null) return;
    failure = LOGIN_RESULTS[result] ?? null;
    url.searchParams.delete("login");
    history.replaceState(history.state, "", url);
  }

  function setFilesOpen(open: boolean): void {
    filesOpen = open;
    localStorage.setItem("files-open", String(open));
  }

  async function describe(): Promise<void> {
    try {
      const info = await api.describe();
      observer = info.observer;
      guildId = info.guildId;
      login = info.login;
      visitor = info.visitor ?? null;
      mayAct = info.mayAct;
    } catch {
      // The list refreshing says plainly enough when the daemon is away.
    }
  }

  async function logout(): Promise<void> {
    await api.logout();
    await describe();
  }

  /** What the session is doing, said in one word beside its name. */
  const status = $derived(
    ended ? "ended" : !live ? "stopped" : waiting !== null ? "waiting" : busy ? "working" : "idle",
  );
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
    { key: "f", does: "show or hide the files" },
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
    if (listed.length === 0) return;
    const at = listed.findIndex((session) => session.id === selected);
    const next = listed[Math.max(0, Math.min(listed.length - 1, at + by))];
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
      f: () => setFilesOpen(!filesOpen),
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
    if (selected !== null) return;

    // Only a session the address names is opened. Anything else lands on the
    // home page, which is where a new session is started from.
    const asked = inAddress();
    if (asked !== null && sessions.some((session) => session.id === asked)) select(asked, true);
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

  async function start(): Promise<void> {
    const written = homeDraft.trim();
    if (written.length === 0 || homeWorking) return;

    // A command is run as the channel runs it rather than starting a session
    // to answer it, so asking for a picture here does not open a thread.
    if (written.startsWith("!")) {
      failure = null;
      homeAnswer = null;
      homeWorking = true;
      try {
        homeAnswer = (await api.command(written)).answer;
        homeDraft = "";
      } catch (error) {
        fail(error);
      } finally {
        homeWorking = false;
      }
      return;
    }

    // The same flag a message in the channel would carry, so the daemon reads
    // the model exactly as it would there.
    const prompt = newModel === null ? written : `--model ${newModel} ${written}`;

    failure = null;
    try {
      const started = await api.start(prompt);
      homeDraft = "";
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

    takeLoginResult();
    void describe();
    void api
      .models()
      .then((found) => (catalog = found))
      .catch(() => undefined);
    void api
      .commands()
      .then((found) => (commandList = found))
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

{#snippet gate(what: string)}
  {#if observer}
    <p class="gate">This interface is read only. Send messages from the chat thread.</p>
  {:else if login && visitor === null}
    <p class="gate">
      <span>Log in to {what}.</span>
      <a class="discord" href={api.loginUrl}>Log in with Discord</a>
    </p>
  {:else}
    <p class="gate">Your account can read here, but cannot drive sessions.</p>
  {/if}
{/snippet}

<div
  class="shell"
  class:files-open={filesOpen && selected !== null}
  data-drawer={drawer ?? "none"}
  style="--files: {filesWidth}px"
>
  <aside class="sidebar">
    <header class="brand">
      <span class="logo" aria-hidden="true">e</span>
      <h1>errand</h1>
      {#if observer}
        <span class="badge">read only</span>
      {/if}
    </header>

    {#if !observer && (mayAct || !login)}
      <button class="new" onclick={goHome}>
        <Icon name="plus" /> New session
      </button>
    {/if}

    {#if sessions.length > 0}
      <div class="filters" role="group" aria-label="Show only">
        {#if visitor !== null}
          <button
            class:on={onlyMine}
            aria-pressed={onlyMine}
            title="Only sessions you started"
            onclick={() => setFilter("only-mine", !onlyMine)}>mine</button
          >
        {/if}
        <button
          class:on={onlyRunning}
          aria-pressed={onlyRunning}
          title="Only sessions with a sandbox running"
          onclick={() => setFilter("only-running", !onlyRunning)}>running</button
        >
      </div>
    {/if}

    <SessionList
      sessions={listed}
      {selected}
      onSelect={select}
      filtered={listed.length < sessions.length}
    />

    <footer class="account">
      {#if visitor !== null}
        {#if visitor.avatar !== null}
          <img class="avatar" src={visitor.avatar} alt="" />
        {:else}
          <span class="avatar initial" aria-hidden="true">{visitor.name.slice(0, 1)}</span>
        {/if}
        <span class="who">
          <span class="name">{visitor.name}</span>
          <span class="role">{mayAct ? "can chat" : "read only"}</span>
        </span>
        <button class="quiet" onclick={() => void logout()}>log out</button>
      {:else if login}
        <a class="discord" href={api.loginUrl}>Log in with Discord</a>
      {/if}
      <button
        class="icon"
        title="Appearance"
        aria-label="Appearance"
        onclick={() => (appearanceOpen = true)}><Icon name="palette" /></button
      >
    </footer>
  </aside>

  <main>
    {#if selected === null}
      <div class="home">
        <button class="pull floating" onclick={() => (drawer = "sessions")} aria-label="Sessions"
          ><Icon name="menu" /></button
        >
        <div class="hero">
          <span class="logo big" aria-hidden="true">e</span>
          <h2>
            {visitor === null ? "What should it work on?" : `What should it work on, ${visitor.name}?`}
          </h2>
          <p>
            A session gets a sandbox of its own and a thread in the channel, and keeps going
            while you are away.
          </p>
        </div>

        {#if failure !== null}
          <p class="banner bad" role="alert">
            <span>{failure}</span>
            <button class="quiet" onclick={() => (failure = null)} aria-label="Dismiss">dismiss</button>
          </p>
        {/if}

        {#if canAct}
          <Composer
            bind:draft={homeDraft}
            roomy
            disabled={homeWorking}
            placeholder="Describe the task. Enter starts it. A ! command runs as it would in the channel."
            {catalog}
            model={newModel}
            allowDefault
            onModel={(name) => (newModel = name)}
            onSend={() => void start()}
          />
          {#if homeWorking}
            <p class="answered working" role="status">Working on it...</p>
          {:else if homeAnswer !== null}
            <p class="answered" role="status">{homeAnswer}</p>
          {/if}
        {:else}
          {@render gate("start a session")}
        {/if}

        {#if listed.some((session) => session.live)}
          <div class="recent">
            <p>Running now</p>
            {#each listed.filter((session) => session.live).slice(0, 4) as session (session.id)}
              <button onclick={() => select(session.id)}>
                <span class="dot" class:busy={session.busy}></span>
                <span class="what">{nameFrom(session.opening)?.title ?? session.project}</span>
                <span class="when">{ago(session.lastActiveAt)}</span>
              </button>
            {/each}
          </div>
        {/if}
      </div>
    {:else}
      <header class="bar">
        <button class="pull" onclick={() => (drawer = "sessions")} aria-label="Sessions"
          ><Icon name="menu" /></button
        >
        {#if searching}
          <div class="finding">
            <Icon name="search" />
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
                {""}
              {:else if found.matches === 0}
                no matches
              {:else}
                {found.matches} in {found.exchanges}
              {/if}
            </span>
            <button
              class="quiet"
              aria-label="Close the search"
              onclick={() => {
                search = "";
                searching = false;
              }}>done</button
            >
          </div>
        {:else}
          <div class="title">
            <span class="named" title={current?.opening ?? current?.project ?? ""}>
              {nameFrom(current?.opening)?.title ?? current?.project ?? selected}
            </span>
            <span class="status {status}">{status}</span>
          </div>
          {#if usage !== null && usage.cost > 0}
            <span class="spend">{money(usage.cost)}</span>
          {/if}
          <button
            class="icon"
            aria-label="Search this session"
            title="Search this session (/)"
            onclick={openSearch}><Icon name="search" /></button
          >
        {/if}

        <LensMenu bind:lens counts={lensCounts} />
        {#if guildId !== null && current?.threadId !== undefined}
          <a
            class="icon thread"
            href={`https://discord.com/channels/${guildId}/${current.threadId}`}
            target="_blank"
            rel="noreferrer"
            title="Open the thread in Discord"
            aria-label="Open the thread in Discord"><Icon name="hash" /></a
          >
        {/if}
        <button
          class="icon"
          aria-pressed={filesOpen}
          title="Files (f)"
          aria-label="Files"
          onclick={() => setFilesOpen(!filesOpen)}><Icon name="folder" /></button
        >
        <button class="icon" title="Keyboard shortcuts (?)" onclick={() => (keysOpen = true)}
          ><Icon name="help" /></button
        >
      </header>

      <!-- One quiet line for what holds for the whole session, since none of it
           changes while it is read. -->
      <p class="facts">
        {#if usage !== null && usage.contextWindow}
          {@const used = Math.round((usage.contextTokens / usage.contextWindow) * 100)}
          <span
            class="meter"
            title="{tokens(usage.contextTokens)} of {tokens(usage.contextWindow)} context"
          >
            <span class="track"><span class="fill" style="width: {Math.min(used, 100)}%"></span></span>
            {used}% of {tokens(usage.contextWindow)}
          </span>
        {/if}
        {#if usage !== null && usage.totalTokens > 0}
          <span title="{tokens(usage.totalTokens)} tokens">
            {tokens(usage.totalTokens)} tokens{cacheRate(usage.input, usage.cacheRead, usage.cacheWrite) === undefined
              ? ""
              : `, ${cacheRate(usage.input, usage.cacheRead, usage.cacheWrite)}% cached`}
          </span>
        {/if}
        {#if reading !== null}
          <span>exchange {reading.at} of {reading.of}</span>
        {/if}
        {#if current !== undefined}
          <span class="when" title={new Date(current.lastActiveAt).toLocaleString()}>
            {ago(current.lastActiveAt)}
          </span>
        {/if}
      </p>

      {#if !connected}
        <p class="banner warn">Reconnecting to the daemon.</p>
      {/if}

      <!-- Keyed on the session, so switching gives a fresh pane rather than one
           holding the last session's scroll position and follow state. -->
      {#key selected}
        <Transcript
          bind:this={transcript}
          session={selected}
          onAnswer={canAct && live && !ended
            ? (choice) => {
                draft = choice;
                void send();
              }
            : undefined}
          {entries}
          {busy}
          {waiting}
          {ended}
          {dropped}
          {search}
          bind:lens
          onFound={(matches, exchanges) => (found = { matches, exchanges })}
          onOpenFile={(path) => {
            setFilesOpen(true);
            void open(path);
          }}
          wanted={wantedTurn}
          onReading={(at, of) => (reading = of === 0 ? null : { at, of })}
        />
      {/key}

      <div class="dock">
        {#if failure !== null}
          <p class="banner bad" role="alert">
            <span>{failure}</span>
            <button class="quiet" onclick={() => (failure = null)} aria-label="Dismiss">dismiss</button>
          </p>
        {/if}

        {#if canAct}
          <Composer
            bind:draft
            disabled={ended}
            placeholder={ended
              ? "This session has ended."
              : live
                ? "Message the agent"
                : "Send a message to pick this session up again"}
            commands={commandList}
            {catalog}
            model={usage?.model ?? null}
            onModel={(name) => void switchModel(name)}
            onSend={() => void send()}
          />
          <p class="note">
            {#if visitor !== null}Sent as {visitor.name}, and posted in the thread.{/if}
            Shift+Enter adds a line. Type <code>!</code> for commands.
          </p>
        {:else}
          {@render gate("chat in this session")}
        {/if}
      </div>
    {/if}
  </main>

  {#if selected !== null}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="grip"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize the file pane"
      onpointerdown={grab}
      ondblclick={() => {
        filesWidth = 360;
        localStorage.setItem("files-width", "360");
      }}
    ></div>

    <aside class="files" aria-label="Files">
      <header class="pane-head">
        <span>files</span>
        <button class="quiet" onclick={() => setFilesOpen(false)} aria-label="Close the files"
          >close</button
        >
      </header>
      {#if viewing === null}
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
  {/if}

  {#if drawer !== null}
    <button class="scrim" aria-label="Close" onclick={() => (drawer = null)}></button>
  {/if}

  {#if keysOpen}
    <div
      class="modal"
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

  {#if appearanceOpen}
    <Appearance onClose={() => (appearanceOpen = false)} />
  {/if}

</div>

<style>
  .shell {
    display: grid;
    grid-template-columns: 264px minmax(0, 1fr);
    height: 100%;
    min-height: 0;
    overflow: hidden;
    background: var(--bg);
  }

  .shell.files-open {
    grid-template-columns: 264px minmax(0, 1fr) 6px clamp(260px, var(--files, 360px), 42vw);
  }

  .sidebar {
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
    background: var(--sidebar);
    border-right: 1px solid var(--line);
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 16px 16px 12px;
  }

  .logo {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 8px;
    background: var(--accent);
    color: var(--send-icon);
    font-family: var(--mono);
    font-weight: 700;
    font-size: 14px;
  }

  .logo.big {
    width: 48px;
    height: 48px;
    border-radius: 14px;
    font-size: 24px;
  }

  h1 {
    margin: 0 auto 0 0;
    font-size: 15px;
    font-weight: 650;
    letter-spacing: -0.02em;
    color: var(--ink-strong);
  }

  .badge {
    white-space: nowrap;
    font-size: 11px;
    color: var(--faint);
    border: 1px solid var(--line);
    border-radius: 999px;
    padding: 2px 8px;
  }

  .new {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 12px 8px;
    padding: 8px 12px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--fill);
    color: var(--ink);
    font-size: 13px;
    font-weight: 550;
    transition: background 120ms ease, border-color 120ms ease;
  }

  .new:hover {
    background: var(--fill-hover);
    border-color: var(--line-strong);
  }

  .new :global(svg) {
    color: var(--accent);
  }

  .filters {
    display: flex;
    gap: 4px;
    padding: 0 16px 2px;
  }

  .filters button {
    padding: 2px 10px;
    border: 1px solid var(--line);
    border-radius: 999px;
    font-size: 11.5px;
    color: var(--faint);
  }

  .filters button:hover {
    color: var(--ink);
    border-color: var(--line-strong);
  }

  .filters button.on {
    color: var(--accent);
    border-color: var(--accent-bd);
    background: var(--accent-soft);
  }

  .account {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: auto;
    padding: 12px;
    border-top: 1px solid var(--line);
  }

  .avatar {
    width: 28px;
    height: 28px;
    flex-shrink: 0;
    border-radius: 50%;
  }

  .avatar.initial {
    display: grid;
    place-items: center;
    background: var(--accent-soft);
    color: var(--accent);
    font-weight: 600;
    text-transform: uppercase;
  }

  .who {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
    line-height: 1.25;
  }

  .who .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
    color: var(--ink-strong);
  }

  .who .role {
    font-size: 11.5px;
    color: var(--faint);
  }

  /* Discord's own button colours, which are the same in every theme. */
  .discord {
    flex: 1;
    padding: 7px 12px;
    border-radius: 8px;
    background: #5865f2;
    color: #ffffff;
    font-size: 13px;
    font-weight: 600;
    text-align: center;
    text-decoration: none;
  }

  .discord:hover {
    background: #4752c4;
  }

  main {
    position: relative;
    display: flex;
    flex-direction: column;
    min-width: 0;
    /* Without this a flex child cannot shrink below its content, so the
       transcript never scrolls and the whole page grows instead. */
    min-height: 0;
    overflow: hidden;
  }

  /*
   * Where a session is started: the composer in the middle of the page with
   * nothing else competing for it, as the first thing anyone sees.
   */
  .home {
    position: relative;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 22px;
    height: 100%;
    overflow-y: auto;
    padding: 32px 24px 12vh;
  }

  .home > :global(:not(.floating)) {
    width: 100%;
    max-width: 720px;
    margin-left: auto;
    margin-right: auto;
  }

  .hero {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    text-align: center;
  }

  .hero h2 {
    margin: 6px 0 0;
    font-size: 24px;
    font-weight: 650;
    letter-spacing: -0.02em;
    color: var(--ink-strong);
  }

  .hero p {
    max-width: 46ch;
    margin: 0;
    font-size: 13.5px;
    color: var(--faint);
  }

  .answered {
    margin: 10px 0 0;
    font-size: 13px;
    color: var(--text-dim);
    white-space: pre-wrap;
  }

  .answered.working {
    color: var(--text-faint);
  }

  .recent {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .recent p {
    margin: 0 0 4px 10px;
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ghost);
  }

  .recent button {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border-radius: 10px;
    text-align: left;
  }

  .recent button:hover {
    background: var(--hover);
  }

  .recent .dot {
    width: 7px;
    height: 7px;
    flex-shrink: 0;
    border-radius: 50%;
    background: var(--faint);
  }

  .recent .dot.busy {
    background: var(--ok);
    box-shadow: 0 0 0 3px var(--ok-soft);
  }

  .recent .what {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 13px;
    color: var(--ink);
  }

  .recent .when {
    flex-shrink: 0;
    font-size: 11.5px;
    color: var(--faint);
  }

  .pull.floating {
    position: absolute;
    top: 12px;
    left: 12px;
  }

  .bar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 12px 20px 6px;
  }

  .title {
    display: flex;
    align-items: center;
    gap: 10px;
    flex: 1;
    min-width: 0;
  }

  .named {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 15px;
    font-weight: 600;
    color: var(--ink-strong);
  }

  .status {
    flex-shrink: 0;
    padding: 1px 8px;
    border-radius: 999px;
    font-size: 11px;
    border: 1px solid var(--line);
    color: var(--faint);
  }

  .status.working {
    color: var(--ok);
    border-color: var(--ok-bd, var(--line));
    background: var(--ok-soft);
  }

  .status.waiting {
    color: var(--warn);
    background: var(--warn-soft);
  }

  .status.ended {
    color: var(--ghost);
  }

  .where,
  .spend {
    flex-shrink: 0;
    padding: 0 6px;
    font-family: var(--mono);
    font-size: var(--type-receipt);
    color: var(--faint);
  }

  .icon {
    display: grid;
    place-items: center;
    flex-shrink: 0;
    width: 30px;
    height: 30px;
    border-radius: 8px;
    font-size: 14px;
    color: var(--muted);
    text-decoration: none;
  }

  .icon:hover,
  .icon[aria-pressed="true"] {
    background: var(--hover);
    color: var(--ink);
  }

  .facts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 14px;
    margin: 0;
    padding: 0 20px 10px;
    font-size: var(--type-receipt);
    color: var(--faint);
    border-bottom: 1px solid var(--subtle);
  }

  .facts:empty {
    display: none;
  }


  .note code {
    padding: 0 4px;
    border-radius: 4px;
    background: var(--code-inline);
  }

  .meter {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .track {
    width: 48px;
    height: 4px;
    overflow: hidden;
    border-radius: 2px;
    background: var(--fill-hover);
  }

  .fill {
    display: block;
    height: 100%;
    background: var(--accent);
  }

  .banner {
    display: flex;
    align-items: baseline;
    gap: 12px;
    margin: 0;
    padding: 8px 20px;
    font-size: 12.5px;
  }

  .banner span {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .banner.warn {
    color: var(--warn);
    background: var(--warn-soft);
  }

  .banner.bad {
    margin: 0 0 8px;
    border-radius: 10px;
    color: var(--danger);
    background: var(--danger-soft);
  }

  .finding {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1;
    min-width: 0;
    padding: 0 4px 0 10px;
    border: 1px solid var(--composer-focus-line);
    border-radius: 10px;
    background: var(--composer-bg);
    color: var(--faint);
  }

  .find {
    flex: 1;
    min-width: 0;
    padding: 6px 0;
    border: none;
    background: transparent;
    font-size: 13px;
  }

  .find:focus {
    outline: none;
  }

  .tally {
    font-size: 11.5px;
    color: var(--faint);
    white-space: nowrap;
  }


  .doing {
    display: flex;
    gap: 2px;
    flex-shrink: 0;
    margin-bottom: 10px;
  }

  .doing button {
    padding: 3px 9px;
    border-radius: 999px;
    font-size: 11.5px;
    color: var(--faint);
  }


  .doing button:hover {
    color: var(--ink);
  }

  .dock {
    padding: 8px 20px 16px;
    background: linear-gradient(to bottom, transparent, var(--bg) 18px);
  }

  .note {
    max-width: var(--measure);
    margin: 6px auto 0;
    font-size: 11.5px;
    color: var(--ghost);
    text-align: center;
  }

  .gate {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-wrap: wrap;
    gap: 12px;
    max-width: var(--measure);
    margin: 0 auto;
    padding: 12px 16px;
    border: 1px dashed var(--line-strong);
    border-radius: 14px;
    font-size: 13px;
    color: var(--muted);
  }

  .gate .discord {
    flex: 0 0 auto;
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

  .shell:not(.files-open) .grip,
  .shell:not(.files-open) .files {
    display: none;
  }

  .files {
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
    background: var(--sidebar);
  }

  .pane-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 14px 8px;
    font-size: 11.5px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--faint);
    border-bottom: 1px solid var(--subtle);
  }

  .quiet {
    padding: 4px 9px;
    border-radius: 8px;
    font-size: 12px;
    font-weight: 400;
    letter-spacing: normal;
    text-transform: none;
    color: var(--muted);
  }

  .quiet:hover {
    background: var(--hover);
    color: var(--ink);
  }

  input {
    color: var(--ink);
    background: var(--composer-bg);
    border: 1px solid var(--line);
    border-radius: 9px;
    padding: 8px 10px;
    font: inherit;
  }

  input::placeholder {
    color: var(--faint);
  }

  .pull {
    display: none;
    flex-shrink: 0;
    width: 30px;
    height: 30px;
    border-radius: 8px;
    color: var(--muted);
  }

  .pull:hover {
    background: var(--hover);
  }

  .scrim {
    position: fixed;
    inset: 0;
    z-index: 8;
    background: var(--overlay);
    border-radius: 0;
    cursor: default;
  }

  .modal {
    position: fixed;
    inset: 0;
    z-index: 10;
    display: grid;
    place-items: center;
    background: var(--overlay);
    backdrop-filter: blur(2px);
  }

  .sheet {
    width: min(380px, 92vw);
    max-height: 82vh;
    overflow: auto;
    padding: 20px;
    border: 1px solid var(--line);
    border-radius: 14px;
    background: var(--menu);
    box-shadow: 0 24px 64px rgb(0 0 0 / 40%);
  }

  .sheet h2 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
    color: var(--ink-strong);
  }

  .sheet dl {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 12px 0 0;
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
    color: var(--muted);
  }

  /*
   * Below this the files become a drawer over the session rather than a
   * column, because taking more width from the conversation costs more than
   * the pane is worth.
   */
  @media (max-width: 1080px) {
    .shell.files-open {
      grid-template-columns: 240px minmax(0, 1fr);
    }

    .shell {
      grid-template-columns: 240px minmax(0, 1fr);
    }

    .grip {
      display: none;
    }

    .shell.files-open .files {
      position: fixed;
      z-index: 9;
      top: 0;
      right: 0;
      bottom: 0;
      width: min(420px, 88vw);
      border-left: 1px solid var(--line);
      box-shadow: -16px 0 40px rgb(0 0 0 / 30%);
    }
  }

  /* Narrower still, and the conversation is the only column left. */
  @media (max-width: 780px) {
    .shell,
    .shell.files-open {
      grid-template-columns: minmax(0, 1fr);
    }

    .sidebar {
      position: fixed;
      z-index: 9;
      top: 0;
      left: 0;
      bottom: 0;
      width: min(300px, 84vw);
      transform: translateX(-101%);
      transition: transform 160ms ease;
    }

    .shell[data-drawer="sessions"] .sidebar {
      transform: none;
    }

    .pull {
      display: grid;
      place-items: center;
    }

    .bar .where {
      display: none;
    }

    .bar,
    .facts,
    .dock {
      padding-left: 12px;
      padding-right: 12px;
    }
  }

  /* The name is what the bar is for, so on a phone it gets a line to itself
     rather than being squeezed to a few letters by the buttons beside it. */
  @media (max-width: 560px) {
    .bar {
      flex-wrap: wrap;
      row-gap: 2px;
    }

    .title {
      flex-basis: 100%;
      order: 1;
      padding-bottom: 4px;
    }

    .pull {
      margin-right: auto;
    }

    .facts .when,
    .spend {
      display: none;
    }
  }
</style>
