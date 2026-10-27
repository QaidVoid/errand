<script lang="ts">
  /**
   * The sessions, named by what each was asked to do.
   *
   * A working directory is not a name a reader recognises, so the opening
   * prompt leads and the project is secondary. Running sessions come first,
   * because a stopped one is rarely what somebody opened the page for.
   */
  import { ago } from "../lib/format.ts";
  import { nameFrom } from "../lib/naming.ts";
  import type { SessionSummary } from "../lib/types.ts";

  let {
    sessions,
    selected,
    onSelect,
    filtered = false,
  }: {
    sessions: SessionSummary[];
    selected: string | null;
    onSelect: (id: string) => void;
    /** Whether a filter is hiding some, which changes what an empty list means. */
    filtered?: boolean;
  } = $props();

  const ordered = $derived(
    [...sessions].sort((a, b) => {
      if (a.live !== b.live) return a.live ? -1 : 1;
      return b.lastActiveAt - a.lastActiveAt;
    }),
  );

  /**
   * The list, cut into days.
   *
   * A reader looking for the session they had open yesterday is looking in
   * time. Running sessions keep their place at the top, since a heading is
   * about when something was last touched and a running one is being touched
   * now.
   */
  const days = $derived.by(() => {
    const groups: { day: string; sessions: SessionSummary[] }[] = [];
    for (const session of ordered) {
      const day = session.live ? "running" : dayOf(session.lastActiveAt);
      const last = groups[groups.length - 1];
      if (last !== undefined && last.day === day) last.sessions.push(session);
      else groups.push({ day, sessions: [session] });
    }
    return groups;
  });

  function dayOf(at: number, now: number = Date.now()): string {
    const midnight = new Date(now);
    midnight.setHours(0, 0, 0, 0);
    const start = midnight.getTime();
    if (at >= start) return "today";
    if (at >= start - 86_400_000) return "yesterday";
    return new Date(at).toLocaleDateString([], { month: "short", day: "numeric" });
  }

  /** What to call a session that was never asked anything. */
  function named(session: SessionSummary): { title: string; detail?: string } {
    return nameFrom(session.opening) ?? { title: `${session.project} ${session.id}` };
  }

  /**
   * What a row says about its state, which is nothing when every row would say
   * the same. Stopped is what a session usually is, and the dot carries it.
   */
  function state(session: SessionSummary): string | undefined {
    if (!session.live) return undefined;
    return session.busy ? "working" : "idle";
  }

  /** The project, unless it only repeats the identifier beside it. */
  function project(session: SessionSummary): string | undefined {
    return session.project === session.id ? undefined : session.project;
  }
</script>

<nav aria-label="Sessions">
  {#if sessions.length === 0 && filtered}
    <p class="empty">No sessions match the filters.</p>
  {:else if sessions.length === 0}
    <p class="empty">
      No sessions yet. Start one here, or send a message in the served channel.
    </p>
  {/if}

  {#each days as group (group.day)}
    <p class="day">{group.day}</p>
    {#each group.sessions as session (session.id)}
      <button
        class="row"
        class:active={session.id === selected}
        aria-current={session.id === selected ? "true" : undefined}
        onclick={() => onSelect(session.id)}
        title={session.opening ?? `${session.project} ${session.id}`}
      >
        <span class="dot" class:busy={session.busy} class:ended={!session.live}></span>
        <span class="body">
          <span class="named">{named(session).title}</span>
          {#if named(session).detail !== undefined}
            <span class="detail">{named(session).detail}</span>
          {/if}
          <span class="under">
            {#if project(session) !== undefined}
              <span class="project">{project(session)}</span>
            {/if}
            {#if state(session) !== undefined}
              <span class="state">{state(session)}</span>
            {/if}
            <span class="when">{ago(session.lastActiveAt)}</span>
          </span>
        </span>
      </button>
    {/each}
  {/each}
</nav>

<style>
  nav {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 4px 8px 12px;
    overflow-y: auto;
    min-height: 0;
  }

  .empty {
    margin: 8px;
    color: var(--faint);
    font-size: var(--type-work);
  }

  /* Where one day ends and another begins, which is how a reader looks for a
     session they had open before. */
  .day {
    margin: 14px 0 4px;
    padding: 0 10px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ghost);
  }

  .day:first-child {
    margin-top: 4px;
  }

  .row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 8px 10px;
    border-radius: 8px;
    text-align: left;
    width: 100%;
    min-width: 0;
    transition: background 120ms ease;
  }

  .row:hover {
    background: var(--hover);
  }

  .row.active {
    background: var(--selected);
  }

  .row.active:hover {
    background: var(--selected-hover);
  }

  .dot {
    width: 7px;
    height: 7px;
    margin-top: 6px;
    border-radius: 50%;
    background: var(--faint);
    flex-shrink: 0;
  }

  .dot.busy {
    background: var(--ok);
    box-shadow: 0 0 0 3px var(--ok-soft);
    animation: pulse 1.6s ease-in-out infinite;
  }

  .dot.ended {
    background: transparent;
    border: 1px solid var(--ghost);
  }

  @keyframes pulse {
    50% {
      box-shadow: 0 0 0 5px var(--ok-soft);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .dot.busy {
      animation: none;
    }
  }

  .body {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 2px;
  }

  .named {
    font-size: var(--type-work);
    line-height: 1.35;
    color: var(--ink);
    /* Two lines is enough to recognise a task by, and bounds the row height. */
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .row.active .named {
    color: var(--ink-strong);
  }

  /* What was asked for, under the thing it was asked about. */
  .detail {
    font-size: var(--type-receipt);
    color: var(--muted);
    line-height: 1.35;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .under {
    display: flex;
    gap: 8px;
    font-size: var(--type-receipt);
    color: var(--faint);
    min-width: 0;
  }

  .project {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .state {
    color: var(--ok);
  }

  .state,
  .when {
    flex-shrink: 0;
  }
</style>
