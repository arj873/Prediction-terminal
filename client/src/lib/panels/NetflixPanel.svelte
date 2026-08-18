<!--
  NFLX — the Netflix Top 10 for a category and a territory.

  Rank is the price and the week is the period, which is the frame a trader has
  loaded when they are looking at a KXNETFLIX market.
-->
<script lang="ts">
  import type { NetflixEntry, NetflixTop10 } from '$gen';

  import { ent } from '../api/client';
  import { compact, day, truncate } from '../format';
  import TablePanel from './TablePanel.svelte';
  import { createPanelData } from './data.svelte';
  import { countOrDash } from './media';
  import type { Column, Note } from './table';

  interface Props {
    id: string;
    /** `tv` or `films`. */
    category: string;
    /** `global`, or an ISO-3166 alpha-2 country code. */
    scope: string;
  }

  const { id, category, scope }: Props = $props();

  const data = createPanelData<NetflixTop10>({
    load: (signal) => ent.netflix(category, scope, signal),
    // Published weekly, on Tuesdays.
    refreshMs: 60 * 60_000,
  });

  /** Country feeds are rank-only; Netflix publishes views and hours globally. */
  function hasViews(rows: readonly NetflixEntry[]): boolean {
    return rows.some((entry) => entry.views !== null);
  }

  function note(chart: NetflixTop10): Note {
    return [
      { text: `Netflix Top 10 · ${chart.categoryLabel} · ${chart.scopeLabel}` },
      { text: `  week of ${day(chart.week)}`, tone: 'dim' },
      ...(hasViews(chart.entries)
        ? []
        : [{ text: '  · ranks only for this country', tone: 'dim' }]),
    ];
  }

  const columns: Column<NetflixEntry>[] = [
    { header: '#', cell: (entry) => String(entry.rank), class: 'num strong' },
    { header: 'TITLE', cell: (entry) => truncate(entry.title, 42), title: (entry) => entry.title },
    { header: 'SEASON', cell: (entry) => truncate(entry.season, 20), class: 'dim' },
    { header: 'VIEWS', cell: (entry) => compact(entry.views), class: 'num', when: hasViews },
    {
      header: 'HOURS',
      cell: (entry) => compact(entry.hoursViewed),
      class: 'num dim',
      when: hasViews,
    },
    { header: 'WKS', cell: (entry) => countOrDash(entry.weeksInTop10), class: 'num dim' },
  ];
</script>

<TablePanel
  {id}
  kind="NFLX"
  title={`${category.toUpperCase()} ${scope.toUpperCase()}`}
  subtitle={data.data ? `${data.data.scopeLabel} · week of ${day(data.data.week)}` : ''}
  {data}
  rows={(chart) => chart.entries}
  {columns}
  {note}
  tableClass="nflx-table"
/>
