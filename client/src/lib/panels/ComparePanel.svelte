<!--
  XV <event> — one question, quoted side by side at every broker that lists it.

  The table grows two columns per venue the terminal found, so its column set is
  a function of the payload rather than a constant. Two derived figures sit at
  the end of every row:

    DIVERGE  the richest mid minus the cheapest — how far apart the books are
    EDGE     the cheapest ask anywhere minus the richest bid anywhere

  A positive edge means the books are crossed *between brokers*, before fees,
  latency and the fact that both legs have to fill. Anything else is the spread,
  and colouring it would invite reading a cost as a profit.
-->
<script lang="ts">
  import type { CompareResponse, CompareRow, MatchConfidence } from '$gen';

  import DataTable from './DataTable.svelte';
  import EmptyState from './EmptyState.svelte';
  import PanelFrame from './PanelFrame.svelte';
  import { createPanelData } from './data.svelte';
  import type { Column } from './table';
  import { navigable } from '../actions/navigable';
  import { xv } from '../api/client';
  import { getRowCursor, getTerminalContext } from '../context';
  import { cents, countdown, signedCents, truncate } from '../format';
  import { formatRef, venueInfo, type VenueRef } from '../terminal/venue';

  const { id, ref }: { id: string; ref: VenueRef } = $props();

  const { run } = getTerminalContext();

  const data = createPanelData({
    load: (signal) => xv.compare(ref.id, ref.venue, signal),
    refreshMs: 10_000,
  });

  const title = $derived(formatRef(ref));
  const subtitle = $derived(data.data?.title ? truncate(data.data.title, 64) : 'cross-venue quote');

  /** A crossed book between brokers, rather than the spread within one. */
  function crossed(row: CompareRow): boolean {
    return row.edge !== null && row.edge > 0;
  }

  /**
   * `edgeVenues` is ordered cheap side first, so it reads as an instruction —
   * with every caveat that instruction needs attached to it.
   */
  function edgeTitle(row: CompareRow): string | undefined {
    if (!crossed(row) || row.edgeVenues.length !== 2) return undefined;
    return (
      `Buy at ${venueInfo(row.edgeVenues[0]!).label}, sell at ${venueInfo(row.edgeVenues[1]!).label}. ` +
      `Before fees, and both legs have to fill.`
    );
  }

  /**
   * The price columns, in the order the server paired the events.
   *
   * Two per venue, except where the venue publishes no book: ForecastEx matches
   * by pairing a YES buyer with a NO buyer, so it has a print and never a quote,
   * and it gets one LAST column instead of two columns of `--`. Its print still
   * moves DIVERGE, which is computed from mids, and two empty columns beside a
   * divergence that plainly used this venue reads as a bug rather than as a fact
   * about the exchange.
   */
  function columnsFor(compare: CompareResponse): Column<CompareRow>[] {
    const columns: Column<CompareRow>[] = [
      {
        header: 'CONTRACT',
        cell: (row) => truncate(row.label, 30),
        class: 'strong',
        title: (row) => row.label,
      },
    ];

    for (const event of compare.events) {
      const info = venueInfo(event.venue);
      const legOf = (row: CompareRow) => row.legs.find((leg) => leg.venue === event.venue);

      if (!info.capabilities.book) {
        columns.push({
          header: `${info.code} LAST`,
          cell: (row) => cents(legOf(row)?.mid ?? null),
          class: 'num',
        });
        continue;
      }

      columns.push({
        header: `${info.code} BID`,
        cell: (row) => cents(legOf(row)?.yesBid ?? null),
        class: 'num price-bid',
      });
      columns.push({
        header: `${info.code} ASK`,
        cell: (row) => cents(legOf(row)?.yesAsk ?? null),
        class: 'num price-ask',
      });
    }

    columns.push({ header: 'DIVERGE', cell: (row) => cents(row.divergence), class: 'num' });
    columns.push({
      header: 'EDGE',
      cell: (row) => signedCents(row.edge),
      class: (row) => `num ${crossed(row) ? 'up strong' : 'dim'}`,
      title: edgeTitle,
    });

    return columns;
  }
</script>

<!--
  `linked` is the only band that is *stated* rather than inferred: it comes from
  the curated table of series identifiers, re-verified against both live
  catalogues. Everything below it is a text match, and says so.
-->
{#snippet matchChip(confidence: MatchConfidence, reason: string)}
  <span class="match-chip match-{confidence}" title={reason}>{confidence.toUpperCase()}</span>
{/snippet}

<PanelFrame {id} kind="XV" {title} {subtitle} subject={formatRef(ref)} {data}>
  {@const cursor = getRowCursor()}
  {@const compare = data.data as CompareResponse}

  <!-- Which events are being compared, and how sure the terminal is. -->
  <div class="compare-legs">
    {#each compare.events as event (`${event.venue}:${event.eventTicker}`)}
      {@const command = `EVT ${formatRef({ venue: event.venue, id: event.eventTicker })}`}
      <!-- A button rather than the old div: the caret reaches it either way,
           but the mouse target should be one the browser knows about too.
           `text-align` undoes the only thing a button brings that the class
           does not want. -->
      <button
        type="button"
        class="compare-leg"
        style="text-align: left"
        title={`${event.title}\n${event.reason}`}
        onclick={() => run(command)}
        use:navigable={{ cursor, command }}
      >
        <span class="venue-badge venue-{event.venue}">{venueInfo(event.venue).code}</span>
        <span class="compare-leg-ticker">{truncate(event.eventTicker, 40)}</span>
        {@render matchChip(event.confidence, event.reason)}
        <span class="dim">{countdown(event.closeTime)}</span>
      </button>
    {/each}
  </div>

  {#if compare.events.length < 2}
    <EmptyState
      message="No other broker appears to list this question."
      hint="Run `XV` on its own for the board of series more than one venue carries."
    />
  {:else}
    {#if compare.rows.length === 0}
      <EmptyState
        message="The events match, but none of their contracts could be paired."
        hint="The brokers word this ladder too differently to line up rung by rung."
      />
    {:else}
      <DataTable rows={compare.rows} columns={columnsFor(compare)} />
    {/if}

    {#if compare.unmatched.length > 0}
      <!--
        Contracts only one venue lists are named rather than dropped: a ladder
        that is finer at one broker is information, not noise.
      -->
      <details class="notes">
        <!-- Navigable so the keyboard can open it: the mouse can, so the caret must. -->
        <summary use:navigable={{ cursor }}
          >{compare.unmatched.length} CONTRACTS WITHOUT A COUNTERPART</summary
        >
        <p>
          {compare.unmatched
            .map((contract) => `${venueInfo(contract.venue).code} ${contract.label}`)
            .join(' · ')}
        </p>
      </details>
    {/if}
  {/if}
</PanelFrame>
