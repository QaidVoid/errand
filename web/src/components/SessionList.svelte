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
  }: {
    sessions: SessionSummary[];
    selected: string | null;
    onSelect: (id: string) => void;
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
  {#if sessions.length === 0}
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
    gap: 2px;
    padding: 8px;
    overflow-y: auto;
    min-height: 0;
  }

  .empty {
    margin: 8px;
    color: var(--text-faint);
    font-size: var(--type-work);
  }

  /* Where one day ends and another begins, which is how a reader looks for a
     session they had open before. */
  .day {
    margin: 10px 0 2px;
    padding: 0 10px;
    font-size: var(--type-receipt);
    color: var(--text-faint);
  }

  .day:first-child {
    margin-top: 2px;
  }

  .row {
    display: flex;
    align-items: flex-start;
    gap: 9px;
    padding: 9px 10px;
    border-radius: var(--radius-sm);
    text-align: left;
    width: 100%;
    min-width: 0;
    transition: background 120ms ease;
  }

  .row:hover {
    background: var(--bg-raised);
  }

  .row.active {
    background: var(--bg-raised);
    box-shadow: inset 2px 0 0 var(--accent);
  }

  .dot {
    width: 7px;
    height: 7px;
    margin-top: 6px;
    border-radius: 50%;
    background: var(--text-faint);
    flex-shrink: 0;
  }

  .dot.busy {
    background: var(--ok);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--ok) 18%, transparent);
  }

  .dot.ended {
    background: transparent;
    border: 1px solid var(--text-faint);
  }

  .body {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 1px;
  }

  .named {
    font-size: var(--type-work);
    line-height: 1.35;
    /* Two lines is enough to recognise a task by, and bounds the row height. */
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  /* What was asked for, under the thing it was asked about. */
  .detail {
    font-size: var(--type-receipt);
    color: var(--text-dim);
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
    color: var(--text-faint);
    min-width: 0;
  }

  .project {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .state,
  .when {
    flex-shrink: 0;
  }
</style>
