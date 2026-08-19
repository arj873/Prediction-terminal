<!--
  The contract table `SRCH` and `EVT` both print.

  One spec rather than two, because it is one list read two ways: every contract
  in an event for `EVT`, the liquid few of a matched event for `SRCH`. The old
  client shared `marketRow` between exactly these two panels for exactly this
  reason — the columns, the truncations and the tooltips are the same decision.

  Every row charts its market, and carries the venue as well as the ticker: the
  first question about a contract is usually "who else lists this, and at what?".
-->
<script lang="ts">
  import type { Market } from '$gen';

  import DataTable from './DataTable.svelte';
  import type { Column } from './table';
  import { cents, compact, countdown, direction, signedCents, truncate } from '../format';
  import { formatRef, venueInfo } from '../terminal/venue';

  const { markets }: { markets: readonly Market[] } = $props();

  const refOf = (market: Market): string => formatRef({ venue: market.venue, id: market.ticker });

  const columns: Column<Market>[] = [
    {
      header: 'VEN',
      render: venueBadge,
      class: 'venue-cell',
      title: (market) => venueInfo(market.venue).label,
    },
    {
      header: 'TICKER',
      cell: (market) => truncate(market.ticker, 28),
      class: 'mono strong',
      title: (market) => market.ticker,
    },
    {
      header: 'CONTRACT',
      cell: (market) => truncate(market.yesSubTitle || market.title, 42),
      title: (market) => market.title,
    },
    { header: 'BID', cell: (market) => cents(market.yesBid), class: 'num price-bid' },
    { header: 'ASK', cell: (market) => cents(market.yesAsk), class: 'num price-ask' },
    { header: 'LAST', cell: (market) => cents(market.lastPrice), class: 'num' },
    {
      header: 'CHG',
      cell: (market) => signedCents(market.change),
      class: (market) => `num ${direction(market.change)}`,
    },
    { header: 'VOL 24H', cell: (market) => compact(market.volume24h), class: 'num dim' },
    { header: 'OI', cell: (market) => compact(market.openInterest), class: 'num dim' },
    { header: 'CLOSES', cell: (market) => countdown(market.closeTime), class: 'num dim' },
  ];
</script>

<!-- A broker is a series, not a direction, which is why it is a chip and not a colour. -->
{#snippet venueBadge(market: Market)}
  <span class="venue-badge venue-{market.venue}">{venueInfo(market.venue).code}</span>
{/snippet}

<DataTable
  rows={markets}
  {columns}
  rowCommand={(market: Market) => `GP ${refOf(market)}`}
  rowTitle={(market: Market) => `${market.title}\nClick to chart ${refOf(market)}`}
/>
