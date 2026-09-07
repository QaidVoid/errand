<script lang="ts">
  /**
   * A question the session's model put to a cheaper one.
   *
   * Folded away by default, and marked as another model's words wherever it
   * appears. A reader scanning a turn should be able to tell at a glance that
   * this part was not the session's own model, and should be able to open it
   * and check what was actually said.
   */
  import { humanSize, tokens } from "../lib/format.ts";
  import type { Delegated } from "../lib/types.ts";

  let { delegated }: { delegated: Delegated } = $props();

  const refused = $derived(delegated.refused !== undefined);
</script>

<details class="delegation" class:refused>
  <summary>
    <span class="mark" aria-hidden="true">to</span>
    {#if refused}
      <span class="what">not delegated</span>
      <span class="why">{delegated.refused}</span>
    {:else}
      <span class="who">{delegated.model}</span>
      <span class="what">about {delegated.describes}</span>
      {#if delegated.keptOut !== undefined && delegated.keptOut > 0}
        <span class="saved" title="Kept out of this conversation">
          {humanSize(delegated.keptOut)} kept out
        </span>
      {/if}
      {#if delegated.tokens !== undefined && delegated.tokens > 0}
        <span class="cost">{tokens(delegated.tokens)}</span>
      {/if}
    {/if}
  </summary>

  <p class="asked">{delegated.question}</p>

  {#if delegated.answer !== undefined}
    <p class="said">{delegated.answer}</p>
    <p class="caveat">
      A description by {delegated.model}, not something this session read itself.
    </p>
  {/if}
</details>

<style>
  .delegation {
    border: 1px solid var(--line-soft);
    border-radius: var(--radius-sm);
    background: var(--bg-inset);
  }

  summary {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 5px 10px;
    cursor: pointer;
    font-size: var(--type-work);
    color: var(--text-dim);
  }

  summary::-webkit-details-marker {
    display: none;
  }

  /* Says what this row is before any of it is read. */
  .mark {
    font-size: 10px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-faint);
  }

  .who {
    color: var(--text);
    font-family: var(--mono);
  }

  .what {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .saved,
  .cost {
    font-size: var(--type-receipt);
    color: var(--text-faint);
    font-variant-numeric: tabular-nums;
  }

  .refused .what {
    color: var(--text-faint);
  }

  .why {
    font-size: var(--type-receipt);
    color: var(--text-faint);
  }

  .asked,
  .said,
  .caveat {
    margin: 0;
    padding: 0 10px 8px 10px;
    font-size: var(--type-work);
  }

  .asked {
    color: var(--text-dim);
    padding-top: 4px;
  }

  .asked::before {
    content: "asked: ";
    color: var(--text-faint);
  }

  .said {
    white-space: pre-wrap;
    color: var(--text);
  }

  .caveat {
    font-size: var(--type-receipt);
    color: var(--text-faint);
  }
</style>
