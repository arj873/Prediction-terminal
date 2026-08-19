<!--
  POD — Apple's podcast charts.

  Two views, because they answer different questions. `top` is the ranking of
  shows overall, which is what a "most popular podcast" market is about.
  `episodes` is what is charting right now, which is where a guest booking shows
  up — and a guest booking is what most of these markets actually trade.
-->
<script lang="ts">
  import type { PodcastChart, PodcastEntry } from '$gen';

  import { ent } from '../api/client';
  import { truncate } from '../format';
  import TablePanel from './TablePanel.svelte';
  import { createPanelData } from './data.svelte';
  import { countOrDash } from './media';
  import type { Column, Note } from './table';

  interface Props {
    id: string;
    /** `top` or `episodes`. */
    view: string;
    /** Two-letter ISO country code. */
    country: string;
  }

  const { id, view, country }: Props = $props();

  const data = createPanelData<PodcastChart>({
    load: (signal) => ent.podcasts(view || undefined, country || undefined, 50, signal),
  });

  const subtitle = $derived(data.data ? `${data.data.entries.length} rows` : '');

  function note(chart: PodcastChart): Note {
    const explicit = chart.entries.filter((entry) => entry.explicit).length;
    const base =
      chart.view === 'episodes'
        ? 'Episodes charting right now — where a guest booking shows up.'
        : 'Apple’s ranking of shows overall.';
    return explicit > 0 ? [{ text: base }, { text: `${explicit}E`, tone: 'dim' }] : base;
  }

  const rowCommand = (entry: PodcastEntry): string | null =>
    entry.name ? `SRCH ${entry.name}` : null;
  const rowTitle = (entry: PodcastEntry): string =>
    entry.publisher ? `${entry.name} — ${entry.publisher}` : entry.name;

  const columns: Column<PodcastEntry>[] = [
    { header: '#', cell: (entry) => countOrDash(entry.rank), class: 'num strong' },
    {
      header: 'SHOW',
      cell: (entry) => truncate(entry.name, 42),
      title: (entry) => entry.name,
    },
    {
      // The publisher on the show chart, the show on the episode chart — same
      // field, and both are what you want beside the title.
      header: 'BY',
      cell: (entry) => truncate(entry.publisher, 26) || '—',
      class: 'dim',
    },
    { header: 'GENRE', cell: (entry) => truncate(entry.genre, 16) || '—', class: 'dim' },
    { header: '', cell: (entry) => (entry.explicit ? 'E' : ''), class: 'dim' },
  ];
</script>

<TablePanel
  {id}
  kind="POD"
  title={(data.data?.viewLabel ?? view).toUpperCase()}
  {subtitle}
  {data}
  rows={(chart: PodcastChart) => chart.entries}
  {columns}
  {note}
  {rowCommand}
  {rowTitle}
/>
