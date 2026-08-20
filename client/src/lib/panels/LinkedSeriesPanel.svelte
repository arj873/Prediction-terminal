<!--
  XV — the board of recurring questions that more than one broker lists.

  Every group carries the confidence behind its pairing, because a quote from
  two brokers is only worth reading if you can see why the terminal believes
  they are quoting the same thing.
-->
<script lang="ts">
  import type { LinkedSeriesResponse, MatchConfidence, SeriesLeg } from '$gen';

  import DataTable from './DataTable.svelte';
  import EmptyState from './EmptyState.svelte';
  import PanelFrame from './PanelFrame.svelte';
  import PanelNote from './PanelNote.svelte';
  import { createPanelData } from './data.svelte';
  import type { Column, NoteSegment } from './table';
  import { xv } from '../api/client';
  import { compact, countdown, group, truncate } from '../format';
  import { formatRef, isVenue, venueInfo } from '../terminal/venue';

  const { id, query }: { id: string; query: string } = $props();

  const data = createPanelData({
    load: (signal) => xv.series(query, 40, signal),
    refreshMs: 120_000,
  });

  const title = $derived(query ? `"${query}"` : 'LINKED SERIES');

  /**
   * The scan line, and the brokers that did not answer.
   *
   * A board covering four of six brokers has to name the two that are missing:
   * without that a "no match" reads as "no such market" rather than as "nobody
   * asked".
   */
  function note(linked: LinkedSeriesResponse): NoteSegment[] {
    const scanned = Object.entries(linked.scanned)
      .map(([venue, count]) => {
        const code = isVenue(venue) ? venueInfo(venue).code : venue;
        return `${code} ${group(count ?? null)}`;
      })
      .join(' · ');

    return [
      { text: `${linked.series.length} linked series` },
      { text: ` · scanned ${scanned}`, tone: 'dim' },
      { text: ` · snapshot ${linked.snapshotAgeSeconds}s old`, tone: 'dim' },
      ...linked.unavailable.map((missing) => ({
        text: ` · ${venueInfo(missing.venue).label} unavailable`,
        tone: 'down',
      })),
    ];
  }

  /** The event that stands for a leg's series — what a click opens. */
  function refOf(leg: SeriesLeg): string {
    return formatRef({ venue: leg.venue, id: leg.sampleEvent });
  }

  const columns: Column<SeriesLeg>[] = [
    {
      header: 'VEN',
      render: venueCell,
      class: 'venue-cell',
      title: (leg) => venueInfo(leg.venue).label,
    },
    {
      header: 'SERIES',
      cell: (leg) => truncate(leg.seriesTicker, 30),
      class: 'mono strong',
      title: (leg) => leg.seriesTicker,
    },
    { header: 'TITLE', cell: (leg) => truncate(leg.title, 40), title: (leg) => leg.title },
    { header: 'EVENTS', cell: (leg) => group(leg.events), class: 'num dim' },
    { header: 'MKTS', cell: (leg) => group(leg.markets), class: 'num dim' },
    { header: 'VOL 24H', cell: (leg) => compact(leg.volume24h), class: 'num' },
    { header: 'NEXT CLOSE', cell: (leg) => countdown(leg.closeTime), class: 'num dim' },
  ];
</script>

{#snippet venueCell(leg: SeriesLeg)}
  <span class="venue-badge venue-{leg.venue}">{venueInfo(leg.venue).code}</span>
{/snippet}

<!--
  `linked` is the only band that is *stated* rather than inferred: it comes from
  the curated table of series identifiers, re-verified against both live
  catalogues on every request. Everything below it is a text match, and says so,
  so a trader never mistakes a guess for a fact.
-->
{#snippet matchChip(confidence: MatchConfidence, reason: string)}
  <span class="match-chip match-{confidence}" title={reason}>{confidence.toUpperCase()}</span>
{/snippet}

<PanelFrame {id} kind="XV" {title} subtitle="series listed by more than one broker" {data}>
  {@const linked = data.data as LinkedSeriesResponse}
  <PanelNote note={note(linked)} />

  {#if linked.series.length === 0}
    <EmptyState
      message={query ? `No linked series match "${query}".` : 'No linked series.'}
      hint="A series appears here only when two or more brokers list it."
    />
  {:else}
    {#each linked.series as series (series.key)}
      <div class="group">
        <div class="group-header" title={series.reason}>
          {@render matchChip(series.confidence, series.reason)}
          <span class="group-title">{truncate(series.title, 62)}</span>
          <span class="group-meta">{series.legs.length} venues</span>
        </div>
        <DataTable
          rows={series.legs}
          {columns}
          rowCommand={(leg: SeriesLeg) => `XV ${refOf(leg)}`}
          rowTitle={(leg: SeriesLeg) => `Compare ${refOf(leg)} across venues`}
        />
      </div>
    {/each}
  {/if}
</PanelFrame>
