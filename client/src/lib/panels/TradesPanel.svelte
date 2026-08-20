<!--
  TAS — the print tape.

  Venue-agnostic like the rest of the market panels, but only two of the three
  brokers publish a public tape; `DES` disables its TAS button for the one that
  does not rather than letting the click land on a 501.
-->
<script lang="ts">
  import type { Trade, TradesResponse } from '$gen';

  import TablePanel from './TablePanel.svelte';
  import type { Column } from './table';
  import { createPanelData } from './data.svelte';
  import { venue } from '../api/client';
  import { cents, compact, group, timeOfDay } from '../format';
  import { formatRef, venueInfo, type VenueRef } from '../terminal/venue';

  const { id, ref }: { id: string; ref: VenueRef } = $props();

  const data = createPanelData({
    load: (signal) => venue.trades(ref, 100, signal),
    refreshMs: 5_000,
  });

  const subject = $derived(formatRef(ref));
  const subtitle = $derived(venueInfo(ref.venue).label);

  /**
   * A YES taker lifted the offer — an aggressive buy of YES, which prints green
   * by the usual convention. A NO taker hit the bid: that is a sale of YES, and
   * prints red.
   *
   * Not every exchange says which side was the aggressor. ForecastEx matches by
   * pairing a YES buyer with a NO buyer, so structurally neither lifted the
   * other. Both it and predict.fun send an empty `takerSide`, and an
   * unattributed print is drawn in neither colour rather than being guessed into
   * red — which is what a two-way test does to every print that states nothing.
   */
  function aggression(trade: Trade): 'buy' | 'sell' | 'unattributed' {
    if (trade.takerSide === 'yes') return 'buy';
    if (trade.takerSide === 'no') return 'sell';
    return 'unattributed';
  }

  const columns: Column<Trade>[] = [
    { header: 'TIME (UTC)', cell: (trade) => timeOfDay(Number(trade.ts)), class: 'mono dim' },
    {
      header: 'PRICE',
      cell: (trade) => cents(trade.yesPrice),
      class: (trade) => `num trade-${aggression(trade)}`,
    },
    { header: 'SIZE', cell: (trade) => compact(trade.count), class: 'num' },
    {
      header: 'TAKER',
      cell: (trade) => trade.takerSide.toUpperCase() || '--',
      class: (trade) => `tag-cell trade-${aggression(trade)}`,
    },
    { header: '', cell: (trade) => (trade.isBlockTrade ? 'BLOCK' : ''), class: 'dim' },
  ];

  function note(loaded: TradesResponse): string {
    const volume = loaded.trades.reduce((sum, trade) => sum + trade.count, 0);
    const notional = loaded.trades.reduce((sum, trade) => sum + trade.count * trade.yesPrice, 0);
    return `${loaded.trades.length} prints · ${group(volume)} contracts · $${group(notional, 2)} notional`;
  }
</script>

<TablePanel
  {id}
  kind="TAS"
  title={subject}
  {subtitle}
  {subject}
  {data}
  rows={(loaded: TradesResponse) => loaded.trades}
  {columns}
  note={(loaded: TradesResponse) => (loaded.trades.length === 0 ? null : note(loaded))}
  empty={() => ({ message: 'No prints yet.' })}
  rowClass={(trade: Trade) => `trade-row-${aggression(trade)}`}
  tableClass="tas-table"
/>
