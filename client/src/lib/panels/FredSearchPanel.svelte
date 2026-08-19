<!--
  FSRCH — find a FRED series id by words.

  The note names which arm of the provider chain answered, because a scraped
  result set and an API one are not the same claim.
-->
<script lang="ts">
  import type { FredSearchResponse, FredSearchResult } from '$gen';

  import TablePanel from './TablePanel.svelte';
  import type { Column } from './table';
  import { createPanelData } from './data.svelte';
  import { fred } from '../api/client';
  import { truncate } from '../format';

  const { id, query }: { id: string; query: string } = $props();

  const data = createPanelData({
    load: (signal) => fred.search(query, 40, signal),
  });

  const columns: Column<FredSearchResult>[] = [
    { header: 'SERIES', cell: (r) => r.id, class: 'mono strong' },
    { header: 'TITLE', cell: (r) => truncate(r.title, 72), title: (r) => r.title },
    { header: 'UNITS', cell: (r) => r.units ?? '', class: 'dim' },
    { header: 'FREQ', cell: (r) => r.frequency ?? '', class: 'dim' },
  ];
</script>

<TablePanel
  {id}
  kind="FSRCH"
  title={`"${query}"`}
  {data}
  rows={(d: FredSearchResponse) => d.results}
  {columns}
  note={(d: FredSearchResponse) =>
    d.results.length === 0 ? null : `${d.results.length} series · via ${d.source}`}
  empty={(d: FredSearchResponse) => ({ message: `No FRED series match "${d.query}".` })}
  rowCommand={(r: FredSearchResult) => `FRED ${r.id}`}
  rowTitle={(r: FredSearchResult) => `Open ${r.id}`}
/>
