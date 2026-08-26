<!--
  BB — a Billboard chart as a ranked table.

  Reads like a market monitor on purpose: rank is the price, the weekly move is
  the change, and peak/weeks-on-chart are the fundamentals. Movement is coloured
  so a week's action is legible at a glance.
-->
<script lang="ts">
  import { SvelteSet } from 'svelte/reactivity';

  import type { BillboardChart, BillboardEntry } from '$gen';

  import { billboard } from '../api/client';
  import { asset as snapshotAsset, snapshotAvailable } from '../api/snapshot';
  import { day, rankMove, truncate } from '../format';
  import TablePanel from './TablePanel.svelte';
  import { createPanelData } from './data.svelte';
  import { movementNote, type Column, type Note } from './table';

  interface Props {
    id: string;
    /** Chart slug, e.g. `hot-100`. */
    chart: string;
    /** Chart week, `YYYY-MM-DD`. Nothing means the latest. */
    date?: string;
  }

  const { id, chart, date = undefined }: Props = $props();

  const data = createPanelData<BillboardChart>({
    load: (signal) => billboard.chart(chart, date, signal),
    // Charts refresh weekly; an hourly poll is already generous.
    refreshMs: 60 * 60_000,
  });

  /**
   * Artwork is fetched through the server rather than straight from Billboard's
   * CDN — same origin, and it works on networks that cannot reach the CDN.
   */
  function artUrl(url: string): string {
    const path = `/billboard/art?u=${encodeURIComponent(url)}`;
    // A snapshot build has no server to proxy through, so the recorded image
    // stands in. An unrecorded one resolves to the empty string, which fails
    // the load and collapses to the placeholder below like any other miss.
    if (snapshotAvailable()) return snapshotAsset(path) ?? '';
    return `/api${path}`;
  }

  /**
   * A failure collapses to the empty placeholder instead of leaving a broken
   * image icon in every row.
   */
  const broken = new SvelteSet<string>();

  function note(loaded: BillboardChart): Note {
    return [
      { text: `${loaded.entries.length} entries · week of ${day(loaded.date)}` },
      ...movementNote(loaded.entries).slice(1),
    ];
  }

  const columns: Column<BillboardEntry>[] = [
    { header: '#', cell: (entry) => String(entry.rank), class: 'num strong' },
    { header: '', render: artwork, class: 'bb-art-cell' },
    {
      header: 'TITLE / ARTIST',
      render: titleArtist,
      title: (entry) => (entry.artist ? `${entry.title} — ${entry.artist}` : entry.title),
    },
    {
      header: 'MOVE',
      cell: (entry) => rankMove(entry.move, entry.isNew).text,
      class: (entry) => `num ${rankMove(entry.move, entry.isNew).tone}`,
    },
    {
      header: 'LW',
      cell: (entry) => (entry.lastWeek === null ? '—' : String(entry.lastWeek)),
      class: 'num dim',
    },
    {
      header: 'PK',
      cell: (entry) => (entry.peak === null ? '—' : String(entry.peak)),
      class: 'num',
    },
    {
      header: 'WKS',
      cell: (entry) => (entry.weeksOnChart === null ? '—' : String(entry.weeksOnChart)),
      class: 'num dim',
    },
  ];
</script>

{#snippet artwork(entry: BillboardEntry)}
  {#if entry.imageUrl && !broken.has(entry.imageUrl)}
    {@const url = entry.imageUrl}
    <img
      class="bb-art"
      src={artUrl(url)}
      alt=""
      loading="lazy"
      decoding="async"
      onerror={() => broken.add(url)}
    />
  {:else}
    <span class="bb-art bb-art-empty"></span>
  {/if}
{/snippet}

{#snippet titleArtist(entry: BillboardEntry)}
  <div class="stacked-cell">
    <div class="bb-title">{truncate(entry.title, 44)}</div>
    {#if entry.artist}
      <div class="bb-artist">{truncate(entry.artist, 44)}</div>
    {/if}
  </div>
{/snippet}

<TablePanel
  {id}
  kind="BB"
  title={chart.toUpperCase()}
  subtitle={data.data ? `${data.data.title} · week of ${day(data.data.date)}` : ''}
  {data}
  rows={(loaded) => loaded.entries}
  {columns}
  {note}
  tableClass="bb-table"
/>
