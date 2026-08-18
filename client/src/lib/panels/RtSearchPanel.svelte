<!--
  RT SEARCH — pick between same-named titles.

  A score of `null` is a title Rotten Tomatoes has not rated yet, and prints as
  `--` rather than 0: on the KXRT ladder a zero would read as a catastrophic
  review instead of an unwritten one.
-->
<script lang="ts">
  import type { RtSearchResponse, RtSearchResult } from '$gen';

  import TablePanel from './TablePanel.svelte';
  import type { Column } from './table';
  import { ent } from '../api/client';
  import { createPanelData } from './data.svelte';
  import { truncate } from '../format';

  const { id, query }: { id: string; query: string } = $props();

  const data = createPanelData({
    load: (signal) => ent.rtSearch(query, 20, signal),
  });

  /** Fresh above 60%, rotten below; unrated is toneless, not bad. */
  function tone(score: number | null): string {
    if (score === null) return 'dim';
    return score >= 60 ? 'up' : 'down';
  }

  const columns: Column<RtSearchResult>[] = [
    {
      header: 'SCORE',
      cell: (r) => (r.criticsScore === null ? '--' : `${r.criticsScore}%`),
      class: (r) => `num ${tone(r.criticsScore)}`,
    },
    { header: 'TITLE', cell: (r) => truncate(r.title, 46), class: 'strong', title: (r) => r.title },
    { header: 'YEAR', cell: (r) => r.year || '—', class: 'num dim' },
    { header: 'TYPE', cell: (r) => r.mediaType, class: 'dim' },
    { header: 'SLUG', cell: (r) => r.slug, class: 'mono dim' },
  ];
</script>

<TablePanel
  {id}
  kind="RT SEARCH"
  title={`"${query}"`}
  {data}
  rows={(d: RtSearchResponse) => d.results}
  {columns}
  empty={(d: RtSearchResponse) => ({
    message: `Rotten Tomatoes has nothing for "${d.query}".`,
  })}
  rowCommand={(r: RtSearchResult) => `RT ${r.slug}`}
  rowTitle={(r: RtSearchResult) => `Open ${r.slug}`}
/>
