<!-- `BB CHARTS` — the catalogue of slugs this terminal knows. -->
<script lang="ts">
  import type { BillboardChartListItem, BillboardChartsResponse } from '$gen';

  import { billboard } from '../api/client';
  import TablePanel from './TablePanel.svelte';
  import { createPanelData } from './data.svelte';
  import type { Column } from './table';

  const { id }: { id: string } = $props();

  // A fixed list; there is nothing here that changes under a poll.
  const data = createPanelData<BillboardChartsResponse>({
    load: (signal) => billboard.charts(signal),
  });

  const columns: Column<BillboardChartListItem>[] = [
    { header: 'SLUG', cell: (item) => item.slug, class: 'mono strong' },
    { header: 'CHART', cell: (item) => item.name },
  ];
</script>

<TablePanel
  {id}
  kind="BB"
  title="CHARTS"
  {data}
  rows={(loaded) => loaded.charts}
  {columns}
  note={() => 'Any billboard.com chart slug works — these are the shortcuts.'}
  rowCommand={(item) => `BB ${item.slug}`}
  rowTitle={(item) => `Open ${item.slug}`}
/>
