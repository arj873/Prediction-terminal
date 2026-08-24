<!--
  SRC — what this deployment can actually serve.

  Not what the registry lists: a reader picking a prefix needs to know which
  publishers will answer *here* before typing one, and what a missing key would
  add. Twelve rows, each carrying the reference shape to type and the sentence
  that says what its credential does or does not buy.
-->
<script lang="ts">
  import type { DataSourceStatus, DataSourcesResponse } from '$gen';

  import TablePanel from './TablePanel.svelte';
  import type { Column } from './table';
  import { createPanelData } from './data.svelte';
  import { data as dataApi } from '../api/client';
  import { truncate } from '../format';

  const { id }: { id: string } = $props();

  const data = createPanelData({
    // A credential does not appear mid-session, so this is read once and left.
    load: (signal) => dataApi.sources(signal),
  });

  const columns: Column<DataSourceStatus>[] = [
    {
      header: 'SRC',
      cell: (r) => r.code,
      class: (r) => `tag-cell source-${r.id}`,
      title: (r) => r.label,
    },
    { header: 'PUBLISHER', cell: (r) => r.label, class: 'strong' },
    { header: 'KIND', cell: (r) => r.kind.toUpperCase(), class: 'dim' },
    { header: 'REFERENCE', cell: (r) => r.idExample, class: 'mono dim' },
    {
      header: '',
      // A word rather than a tick: the column is scanned, and "OK" beside a
      // publisher that needs a key would read as "no key needed".
      cell: (r) => (r.available ? 'READY' : 'NO KEY'),
      class: (r) => (r.available ? 'up strong' : 'warn strong'),
    },
    {
      header: 'COVERS',
      cell: (r) => truncate(r.note || r.covers, 78),
      title: (r) => (r.note ? `${r.covers}\n\n${r.note}` : r.covers),
      class: 'dim',
    },
  ];

  function note(loaded: DataSourcesResponse): string {
    const ready = loaded.sources.filter((s) => s.available).length;
    return `${ready} of ${loaded.sources.length} publishers ready · ECO charts a series, ECOS searches every one at once`;
  }
</script>

<TablePanel
  {id}
  kind="SRC"
  title="DATA SOURCES"
  {data}
  rows={(d: DataSourcesResponse) => d.sources}
  {columns}
  {note}
  empty={() => ({ message: 'No data sources are registered.' })}
  rowCommand={(r: DataSourceStatus) =>
    r.kind === 'series' ? `ECO ${r.idExample}` : `ECOS ${r.label}`}
  rowTitle={(r: DataSourceStatus) => `Open ${r.idExample}`}
/>
