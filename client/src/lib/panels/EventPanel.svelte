<!--
  EVT — every contract that resolves one question, as a strike ladder.

  Sorted by mid, so the ladder reads as a distribution. On a mutually exclusive
  event the YES mids should sum to about 1; when they do not, the gap is the
  arbitrage (or the width of the spreads), which is why it is printed rather
  than left for the reader to add up.
-->
<script lang="ts">
  import type { VenueEvent } from '$gen';

  import MarketTable from './MarketTable.svelte';
  import PanelFrame from './PanelFrame.svelte';
  import { createPanelData } from './data.svelte';
  import { navigable } from '../actions/navigable';
  import { venue } from '../api/client';
  import { getRowCursor, getTerminalContext } from '../context';
  import { compact, percent, truncate } from '../format';
  import { formatRef, venueInfo, type VenueRef } from '../terminal/venue';

  const { id, ref }: { id: string; ref: VenueRef } = $props();

  const { run } = getTerminalContext();

  const data = createPanelData({
    load: (signal) => venue.event(ref, signal),
    refreshMs: 10_000,
  });

  const subject = $derived(formatRef(ref));

  const subtitle = $derived.by(() => {
    const label = venueInfo(ref.venue).label;
    const event = data.data;
    return event ? `${label} · ${truncate(event.title, 52)}` : label;
  });

  /**
   * 24h volume across the event, or `--` when its venue publishes none.
   *
   * Summing the unpublished figures as zeroes would print `24h 0` for every
   * Polymarket US event, which reads as "nothing traded here" rather than "this
   * venue does not say".
   */
  function volume24h(event: VenueEvent): number | null {
    const published = event.markets
      .map((market) => market.volume24h)
      .filter((value): value is number => value !== null);
    return published.length === 0 ? null : published.reduce((sum, value) => sum + value, 0);
  }

  /** Σ of the YES mids, or `null` when the venue quotes none of the legs. */
  function midSum(event: VenueEvent): number | null {
    const mids = event.markets
      .map((market) => market.mid)
      .filter((value): value is number => value !== null);
    return mids.length === 0 ? null : mids.reduce((sum, value) => sum + value, 0);
  }
</script>

<PanelFrame {id} kind="EVT" title={subject} {subtitle} {subject} {data}>
  {@const cursor = getRowCursor()}
  {@const event = data.data as VenueEvent}
  {@const sum = midSum(event)}

  <div class="result-note">
    <span>{`${event.markets.length} contracts`}</span>
    <span class="dim">{` · 24h ${compact(volume24h(event))}`}</span>
    <span class="dim">{` · ${event.category || 'uncategorised'}`}</span>
    {#if event.mutuallyExclusive}
      <!-- Flagged only when the sum is knowably off: an unquoted ladder is not an arbitrage. -->
      <span class={sum !== null && Math.abs(sum - 1) > 0.05 ? 'down' : 'dim'}
        >{` · exclusive, Σmid ${percent(sum, 1)}`}</span
      >
    {/if}
  </div>

  <MarketTable markets={[...event.markets].sort((a, b) => (b.mid ?? 0) - (a.mid ?? 0))} />

  <div class="panel-actions">
    <button
      class="action"
      type="button"
      onclick={() => run(`XV ${subject}`)}
      use:navigable={{ cursor }}>XV COMPARE</button
    >
  </div>
</PanelFrame>
