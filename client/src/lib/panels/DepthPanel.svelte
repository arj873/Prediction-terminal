<!--
  OB — the resting book, as a conventional ladder.

  YES asks descending above the spread, YES bids descending below. What actually
  rests on the ask side is the NO book; the server has already inverted it into
  YES terms (`1 - noPrice`, ascending) and hands it over as `yesAsks`, so nothing
  here re-derives it — one inversion, server-side, is the whole point.

  Not a `TablePanel`, and deliberately so: the row between the two sides is
  synthetic — it is the spread, not a level — and the depth column is a bar
  rather than a formatted figure.
-->
<script lang="ts">
  import type { OrderBook } from '$gen';

  import PanelFrame from './PanelFrame.svelte';
  import { createPanelData } from './data.svelte';
  import { venue } from '../api/client';
  import { cents, compact } from '../format';
  import { formatRef, venueInfo, type VenueRef } from '../terminal/venue';

  const { id, ref }: { id: string; ref: VenueRef } = $props();

  const data = createPanelData({
    load: (signal) => venue.orderBook(ref, 15, signal),
    refreshMs: 3_000,
  });

  const subject = $derived(formatRef(ref));
  const subtitle = $derived(venueInfo(ref.venue).label);

  /**
   * Bar width as a share of the largest resting size on *either* side, so the
   * two halves of the ladder are drawn to one scale and a thick bid reads as a
   * thick bid. The 2% floor keeps a one-lot level visible instead of invisible.
   */
  function depthWidth(size: number, maxSize: number): string {
    return `${Math.max(2, (size / maxSize) * 100)}%`;
  }
</script>

{#snippet ladderRow(price: number, size: number, maxSize: number, side: 'bid' | 'ask')}
  <tr class="ladder-{side}">
    <td class="num">{compact(size)}</td>
    <td class="num price-{side}">{cents(price)}</td>
    <td class="depth-cell">
      <span class="depth-bar depth-{side}" style:width={depthWidth(size, maxSize)}></span>
    </td>
  </tr>
{/snippet}

<PanelFrame {id} kind="OB" title={subject} {subtitle} {subject} {data}>
  {@const book = data.data as OrderBook}
  {@const maxSize = Math.max(
    1,
    ...book.yes.map((level) => level.size),
    ...book.yesAsks.map((level) => level.size),
  )}

  <div class="result-note">
    <span>{`BID ${cents(book.bestYesBid)}`}</span>
    <span class="dim">{' × '}</span>
    <span>{`ASK ${cents(book.bestYesAsk)}`}</span>
    <span class="dim">{`   ${book.yes.length + book.yesAsks.length} levels`}</span>
  </div>

  <table class="data-table ladder">
    <thead>
      <tr>
        <th>SIZE</th>
        <th>PRICE (YES)</th>
        <th>DEPTH</th>
      </tr>
    </thead>
    <tbody>
      <!-- Best ask nearest the spread, so the ladder reads outwards from the touch. -->
      {#each [...book.yesAsks].reverse() as level, index (index)}
        {@render ladderRow(level.price, level.size, maxSize, 'ask')}
      {/each}

      <!-- Synthetic: the gap between the two sides, not a level anyone can trade. -->
      <tr class="spread-row">
        <td class="num">{cents(book.spread)}</td>
        <td class="spread-label">SPREAD</td>
        <td class="num">{cents(book.mid)}</td>
      </tr>

      {#each book.yes as level, index (index)}
        {@render ladderRow(level.price, level.size, maxSize, 'bid')}
      {/each}
    </tbody>
  </table>
</PanelFrame>
