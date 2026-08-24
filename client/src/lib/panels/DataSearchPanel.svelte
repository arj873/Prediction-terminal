<!--
  ECOS — find a series id by words, across every publisher at once.

  Three things below the table rather than in it, because each is a different
  kind of absence and a reader acts on them differently: how many publishers
  answered, which were asked and did not, and which were never asked for want
  of a key. The last of those is the only one a reader can fix, so it says how.
-->
<script lang="ts">
  import type { DataSearchResponse, DataSearchResult } from '$gen';

  import TablePanel from './TablePanel.svelte';
  import type { Column, NoteSegment } from './table';
  import { createPanelData } from './data.svelte';
  import { data as dataApi } from '../api/client';
  import { truncate } from '../format';
  import { dataSourceInfo, formatDataRef, type DataSource } from '../terminal/dataset';

  const {
    id,
    query,
    sources = [],
  }: { id: string; query: string; sources?: readonly DataSource[] } = $props();

  const data = createPanelData({
    load: (signal) => dataApi.search(query, sources, 40, signal),
  });

  const columns: Column<DataSearchResult>[] = [
    {
      header: 'SRC',
      cell: (r) => dataSourceInfo(r.provider).code,
      class: (r) => `tag-cell source-${r.provider}`,
      title: (r) => dataSourceInfo(r.provider).label,
    },
    { header: 'SERIES', cell: (r) => r.id, class: 'mono strong', title: (r) => r.id },
    { header: 'TITLE', cell: (r) => truncate(r.title, 64), title: (r) => r.title },
    { header: 'UNITS', cell: (r) => r.units ?? '', class: 'dim' },
    { header: 'FREQ', cell: (r) => r.frequency ?? '', class: 'dim' },
  ];

  const named = (list: readonly { provider: DataSource }[]): string =>
    list.map((row) => dataSourceInfo(row.provider).label).join(', ');

  function note(loaded: DataSearchResponse): NoteSegment[] | null {
    const segments: NoteSegment[] = [];

    if (loaded.results.length > 0) {
      const publishers = new Set(loaded.results.map((r) => r.provider));
      segments.push({
        text: `${loaded.results.length} series · ${publishers.size} publisher${publishers.size === 1 ? '' : 's'}`,
        tone: 'dim',
      });
    }

    // Asked and did not answer: an outage, and nothing a reader can do about it
    // except know the board is short.
    if (loaded.unavailable.length > 0) {
      segments.push({
        text: `${segments.length > 0 ? '  ' : ''}${named(loaded.unavailable)} did not answer — these results are incomplete.`,
        tone: 'down',
      });
    }

    // Never asked: a key this deployment does not hold, which is fixable, so
    // the publisher's own sentence about it goes here rather than a shrug.
    for (const row of loaded.skipped) {
      segments.push({
        text: `  ${dataSourceInfo(row.provider).label} was not asked. ${row.hint}`,
        tone: 'dim',
      });
    }

    return segments.length > 0 ? segments : null;
  }
</script>

<TablePanel
  {id}
  kind="ECOS"
  title={`"${query}"`}
  subtitle={sources.length > 0 ? sources.map((s) => dataSourceInfo(s).label).join(' · ') : ''}
  {data}
  rows={(d: DataSearchResponse) => d.results}
  {columns}
  {note}
  empty={(d: DataSearchResponse) => ({
    message: `No series match "${d.query}".`,
    hint: 'Try fewer words, or name a publisher: `ECOS oil eia`, `ECOS inflation ecb oecd`.',
  })}
  rowCommand={(r: DataSearchResult) => `ECO ${formatDataRef({ source: r.provider, id: r.id })}`}
  rowTitle={(r: DataSearchResult) => `Open ${r.id} at ${dataSourceInfo(r.provider).label}`}
/>
