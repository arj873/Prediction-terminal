<!--
  SPOT / YT — the Spotify and YouTube streaming charts.

  One panel for both, because they are the same table: a rank, a weekly move,
  the period figure and the running total. Only the labels differ.
-->
<script lang="ts">
  import type { StreamChart, StreamEntry } from '$gen';

  import { ent } from '../api/client';
  import { compact, group, truncate } from '../format';
  import TablePanel from './TablePanel.svelte';
  import { createPanelData } from './data.svelte';
  import { countOrDash, moveColumn } from './media';
  import { movementNote, type Column, type Note } from './table';

  interface Props {
    id: string;
    source: 'spotify' | 'youtube';
    /** Spotify: country code. YouTube: the view name. */
    scope: string;
    /** Spotify only: `daily` or `weekly`. */
    period: string;
  }

  const { id, source, scope, period }: Props = $props();

  const data = createPanelData<StreamChart>({
    load: (signal) =>
      source === 'spotify'
        ? ent.spotify(scope, period, 200, signal)
        : ent.youtube(scope, 200, signal),
    refreshMs: 30 * 60_000,
  });

  const title = $derived(
    source === 'spotify' ? `${scope.toUpperCase()} ${period.toUpperCase()}` : scope.toUpperCase(),
  );

  function note(chart: StreamChart): Note {
    return [
      ...movementNote(chart.entries),
      // Say where the numbers came from: this is a mirror of Spotify and
      // YouTube, not the platforms themselves.
      { text: '  · via kworb.net', tone: 'dim' },
    ];
  }

  const columns: Column<StreamEntry>[] = $derived([
    { header: '#', cell: (entry) => String(entry.rank), class: 'num strong' },
    moveColumn<StreamEntry>(),
    {
      header: 'TITLE / ARTIST',
      render: titleArtist,
      title: (entry) => (entry.artist ? `${entry.artist} — ${entry.title}` : entry.title),
    },
    {
      // The period figure. On the all-time table that is the daily number and
      // `TOTAL` carries the cumulative one, which is why they are two columns.
      header: source === 'youtube' ? 'VIEWS' : 'STREAMS',
      class: 'num',
      render: streamsCell,
    },
    {
      header: 'PK',
      cell: (entry) => countOrDash(entry.peak),
      class: 'num dim',
      when: (rows) => rows.some((entry) => entry.peak !== null),
    },
    {
      header: 'TOTAL',
      cell: (entry) => compact(entry.total),
      class: 'num dim',
      when: (rows) => rows.some((entry) => entry.total !== null),
    },
  ]);
</script>

{#snippet titleArtist(entry: StreamEntry)}
  <div class="stacked-cell">
    <div class="bb-title">{truncate(entry.title, 44)}</div>
    {#if entry.artist}
      <div class="bb-artist">{truncate(entry.artist, 44)}</div>
    {/if}
  </div>
{/snippet}

<!-- The delta belongs beside the figure it moved, not in its own column. -->
{#snippet streamsCell(entry: StreamEntry)}
  <div>
    <div class="num">{group(entry.streams)}</div>
    {#if entry.streamsChange !== null}
      <div
        class="num tiny {entry.streamsChange > 0 ? 'up' : entry.streamsChange < 0 ? 'down' : 'dim'}"
      >
        {entry.streamsChange > 0 ? '+' : ''}{compact(entry.streamsChange)}
      </div>
    {/if}
  </div>
{/snippet}

<TablePanel
  {id}
  kind={source === 'spotify' ? 'SPOT' : 'YT'}
  {title}
  subtitle={data.data ? truncate(data.data.title, 52) : ''}
  {data}
  rows={(chart) => chart.entries}
  {columns}
  {note}
  tableClass="stream-table"
/>
