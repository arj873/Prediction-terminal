<!--
  TOP — the leaderboards: volume, movers, open interest, liquidity.

  The board only asks the venues whose registry entry says they publish the
  figure being ranked. That filter is the whole panel. A `null` volume means
  "this venue does not publish it", and ranking a null as a zero would seat
  every Polymarket US contract at the bottom of a volume board and imply that
  nothing trades there — an outage and a fact about an API rendered identically.

  So the two absences are kept apart and both are named: a venue that cannot be
  ranked is listed with its own explanation of why, and a venue that could have
  been ranked but did not answer is listed as an outage that leaves the board
  incomplete.
-->
<script lang="ts">
  import type { Market, MoverSort, Venue } from '$gen';

  import TablePanel from './TablePanel.svelte';
  import type { Column, NoteSegment } from './table';
  import { createPanelData } from './data.svelte';
  import { venue } from '../api/client';
  import { cents, compact, direction, signedCents, truncate } from '../format';
  import { VENUE_IDS, formatRef, supportsSort, venueInfo } from '../terminal/venue';

  const { id, sort, venues }: { id: string; sort: MoverSort; venues: readonly Venue[] } = $props();

  /** What the sort is called in prose, for the subtitle and the two notes. */
  const SORT_LABEL: Record<MoverSort, string> = {
    volume: '24h volume',
    gainers: 'gainers',
    losers: 'losers',
    open_interest: 'open interest',
    liquidity: 'liquidity',
  };

  interface TopData {
    markets: Market[];
    /** Venues whose API does not publish this figure at all. */
    cannotRank: Venue[];
    /** Venues that do publish it but did not answer. */
    unavailable: Venue[];
  }

  const data = createPanelData({
    load: async (signal): Promise<TopData> => {
      // A venue that does not publish this figure is not asked for it. Inferring
      // that from an empty result made an outage indistinguishable from a fact
      // about the venue's API — and the panel said the wrong one out loud.
      const ranked = venues.filter((v) => supportsSort(v, sort));
      const cannotRank = venues.filter((v) => !ranked.includes(v));

      const settled = await Promise.allSettled(ranked.map((v) => venue.top(v, sort, 30, signal)));

      const markets: Market[] = [];
      const unavailable: Venue[] = [];

      settled.forEach((result, index) => {
        const v = ranked[index]!;
        if (result.status !== 'fulfilled') unavailable.push(v);
        else markets.push(...result.value.markets);
      });

      const field = (market: Market): number =>
        sort === 'volume'
          ? (market.volume24h ?? 0)
          : sort === 'open_interest'
            ? (market.openInterest ?? 0)
            : sort === 'liquidity'
              ? (market.liquidity ?? 0)
              : (market.change ?? 0);

      markets.sort((a, b) => (sort === 'losers' ? field(a) - field(b) : field(b) - field(a)));

      return { markets: markets.slice(0, 30), cannotRank, unavailable };
    },
    refreshMs: 30_000,
  });

  const scope = $derived(
    venues.length === VENUE_IDS.length
      ? 'all venues'
      : venues.map((v) => venueInfo(v).label).join(' · '),
  );

  const label = $derived(SORT_LABEL[sort]);

  /**
   * "Kalshi", "Kalshi and Gemini", "Kalshi, Gemini and ForecastEx".
   *
   * A plain comma join read as one name with a subject-verb disagreement behind
   * it the day a second venue joined the list — "Polymarket US, Gemini publishes
   * no 24h volume" — and a third would only have made it worse. The server
   * words its own refusals the same way, in `sources/venues.rs`.
   */
  const names = (list: readonly Venue[]): string => {
    const labels = list.map((v) => venueInfo(v).label);
    if (labels.length <= 1) return labels[0] ?? '';
    return `${labels.slice(0, -1).join(', ')} and ${labels[labels.length - 1]}`;
  };

  /**
   * Each excluded venue's own account of what it withholds, deduplicated.
   *
   * The registry carries the reason next to the capability precisely so the
   * panel can say it: "not ranked here" alone invites the reader to assume an
   * outage, which is the confusion this panel exists to avoid.
   */
  const reasons = (list: readonly Venue[]): string[] => [
    ...new Set(
      list.map((v) => venueInfo(v).capabilities.note).filter((note): note is string => !!note),
    ),
  ];

  function note(loaded: TopData): NoteSegment[] {
    // Nothing to caveat when there is no board; the empty state says it instead.
    if (loaded.markets.length === 0) return [];

    const segments: NoteSegment[] = [];

    if (loaded.cannotRank.length > 0) {
      segments.push({
        text:
          loaded.cannotRank.length === 1
            ? `${names(loaded.cannotRank)} publishes no ${label} and is not ranked here.`
            : `${names(loaded.cannotRank)} publish no ${label} and are not ranked here.`,
        tone: 'dim',
      });
      for (const reason of reasons(loaded.cannotRank)) {
        segments.push({ text: ` ${reason}`, tone: 'dim' });
      }
    }

    // A different absence, and a trader needs to tell them apart: a venue that
    // publishes no volume cannot appear on a volume board however healthy it is,
    // whereas one that does publish it and did not answer is an outage.
    if (loaded.unavailable.length > 0) {
      segments.push({
        text: `${loaded.cannotRank.length > 0 ? '  ' : ''}${names(loaded.unavailable)} did not answer — these rankings are incomplete.`,
        tone: 'down',
      });
    }

    return segments;
  }

  function empty(loaded: TopData): { message: string; hint?: string } {
    const everyVenueBlind = loaded.cannotRank.length === venues.length;

    // The note is not rendered without a board, so both caveats move down here:
    // an empty board that is empty because every venue is down must not read as
    // an empty board that is empty because nothing is trading.
    const hint = [
      ...reasons(loaded.cannotRank),
      ...(loaded.unavailable.length > 0 ? [`${names(loaded.unavailable)} did not answer.`] : []),
    ].join(' ');

    return {
      message: everyVenueBlind
        ? `No venue in this scope publishes ${label}.`
        : 'No markets to rank yet.',
      ...(hint ? { hint } : {}),
    };
  }

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
      cell: (market) => truncate(market.ticker, 26),
      class: 'mono strong',
      title: (market) => market.ticker,
    },
    {
      header: 'MARKET',
      cell: (market) => truncate(market.title, 46),
      title: (market) => market.title,
    },
    { header: 'BID', cell: (market) => cents(market.yesBid), class: 'num price-bid' },
    { header: 'ASK', cell: (market) => cents(market.yesAsk), class: 'num price-ask' },
    {
      header: 'CHG',
      cell: (market) => signedCents(market.change),
      class: (market) => `num ${direction(market.change)}`,
    },
    { header: 'VOL 24H', cell: (market) => compact(market.volume24h), class: 'num' },
    { header: 'OI', cell: (market) => compact(market.openInterest), class: 'num dim' },
  ];
</script>

{#snippet venueBadge(market: Market)}
  <span class="venue-badge venue-{market.venue}">{venueInfo(market.venue).code}</span>
{/snippet}

<TablePanel
  {id}
  kind="TOP"
  title={sort.toUpperCase()}
  subtitle={`by ${label} · ${scope}`}
  {data}
  rows={(loaded: TopData) => loaded.markets}
  {columns}
  {note}
  {empty}
  rowCommand={(market: Market) => `GP ${refOf(market)}`}
  rowTitle={(market: Market) => `Click to chart ${refOf(market)}`}
/>
