<!--
  DGOV — the government's dataset catalogue.

  A finding aid rather than a feed: this is where a series is *named*, and the
  other publishers here are where it is read. So the columns are the ones that
  decide whether a dataset is worth opening — who publishes it, how often it is
  refreshed, when it last was, and in what format.
-->
<script lang="ts">
  import type { DataGovDataset, DataGovSearchResponse } from '$gen';

  import TablePanel from './TablePanel.svelte';
  import type { Column } from './table';
  import { createPanelData } from './data.svelte';
  import { datagov as datagovApi } from '../api/client';
  import { day, truncate } from '../format';

  const { id, query }: { id: string; query: string } = $props();

  const data = createPanelData({
    // A catalogue of 300,000 datasets does not turn over between refreshes.
    load: (signal) => datagovApi.search(query, 40, signal),
    refreshMs: 30 * 60_000,
  });

  const columns: Column<DataGovDataset>[] = [
    { header: 'DATASET', cell: (r) => truncate(r.title, 52), class: 'strong', title: (r) => r.description },
    { header: 'PUBLISHER', cell: (r) => truncate(r.publisher, 34), class: 'dim' },
    { header: 'FREQ', cell: (r) => r.frequency || '—', class: 'dim' },
    { header: 'UPDATED', cell: (r) => (r.modified ? day(r.modified) : '—'), class: 'num dim' },
    { header: 'FORMATS', cell: (r) => r.formats.slice(0, 3).join(' ') || '—', class: 'dim' },
  ];
</script>

<TablePanel
  {id}
  kind="DGOV"
  title={`DATASETS — ${query.toUpperCase()}`}
  {data}
  rows={(d: DataGovSearchResponse) => d.datasets}
  {columns}
  note={(d: DataGovSearchResponse) => `${d.datasets.length} datasets · ${d.source}`}
  empty={() => ({ message: `data.gov lists no dataset matching "${query}".` })}
  rowTitle={(r: DataGovDataset) => r.url || r.title}
/>
