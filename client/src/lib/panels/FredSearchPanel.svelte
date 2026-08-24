<!--
  FSRCH — find a FRED series id by words.

  The note names which arm of the provider chain answered, because a scraped
  result set and an API one are not the same claim.
-->
<script lang="ts">
  import type { DataSearchResponse, DataSearchResult } from '$gen';

  import TablePanel from './TablePanel.svelte';
  import type { Column } from './table';
  import { createPanelData } from './data.svelte';
  import { fred } from '../api/client';
  import { truncate } from '../format';

  const { id, query }: { id: string; query: string } = $props();

  const data = createPanelData({
    load: (signal) => fred.search(query, 40, signal),
  });

  const columns: Column<DataSearchResult>[] = [
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
  rows={(d: DataSearchResponse) => d.results}
  {columns}
  note={(d: DataSearchResponse) => (d.results.length === 0 ? null : `${d.results.length} series`)}
  empty={(d: DataSearchResponse) => ({ message: `No FRED series match "${d.query}".` })}
  rowCommand={(r: DataSearchResult) => `FRED ${r.id}`}
  rowTitle={(r: DataSearchResult) => `Open ${r.id}`}
/>
