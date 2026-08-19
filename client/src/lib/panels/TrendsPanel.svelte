<!--
  TRND — what is being searched right now, from Google Trends.

  The panel says plainly that this is the *daily* list. The markets that name
  trends.google.com settle on the annual Year in Search, published once in
  December; what this shows is the evidence a trader has in August for a market
  that resolves then — the same relationship BO has to a total-gross market.
-->
<script lang="ts">
  import type { TrendEntry, TrendList } from '$gen';

  import { ent } from '../api/client';
  import { compact, truncate } from '../format';
  import TablePanel from './TablePanel.svelte';
  import { createPanelData } from './data.svelte';
  import { countOrDash } from './media';
  import type { Column, Note } from './table';

  interface Props {
    id: string;
    /** Two-letter ISO country code. */
    geo: string;
  }

  const { id, geo }: Props = $props();

  const data = createPanelData<TrendList>({
    load: (signal) => ent.trends(geo || undefined, 25, signal),
    // Google rebuilds the list through the day.
    refreshMs: 300_000,
  });

  const subtitle = $derived(data.data ? `${data.data.entries.length} trending now` : '');

  const note = (): Note => [
    { text: 'Google’s daily trending list' },
    {
      text: '— not the annual Year in Search those markets settle on.',
      tone: 'warn',
    },
  ];

  const rowCommand = (entry: TrendEntry): string | null =>
    entry.query ? `SRCH ${entry.query}` : null;
  const rowTitle = (entry: TrendEntry): string =>
    entry.headline ? `${entry.headline} — ${entry.headlineSource}` : entry.query;

  const columns: Column<TrendEntry>[] = [
    { header: '#', cell: (entry) => countOrDash(entry.rank), class: 'num strong' },
    {
      header: 'SEARCH',
      cell: (entry) => truncate(entry.query, 28),
      title: (entry) => entry.query,
    },
    {
      // Google states these as lower bounds and never as exact counts, so the
      // column says so rather than presenting a figure it does not mean.
      header: 'SEARCHES',
      cell: (entry) => (entry.trafficFloor === null ? '—' : `${compact(entry.trafficFloor)}+`),
      class: 'num',
    },
    {
      header: 'WHY',
      cell: (entry) => truncate(entry.headline, 46) || '—',
      title: (entry) => entry.headline,
      class: 'dim',
    },
    {
      header: 'SOURCE',
      cell: (entry) => truncate(entry.headlineSource, 16) || '—',
      class: 'dim',
    },
  ];
</script>

<TablePanel
  {id}
  kind="TRND"
  title={(data.data?.geoLabel ?? geo).toUpperCase()}
  {subtitle}
  {data}
  rows={(list: TrendList) => list.entries}
  {columns}
  {note}
  {rowCommand}
  {rowTitle}
/>
