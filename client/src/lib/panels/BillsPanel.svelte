<!--
  CONG — bills, most recently acted on first.

  The panel states what the search actually looked at, because it is not what a
  reader assumes: Congress.gov's bill collection has no text parameter, so this
  matches titles across a window of recently-updated bills. A miss means "not
  among the recently active bills", not "no such bill" — and the note says so
  rather than letting an empty table imply the stronger claim.
-->
<script lang="ts">
  import type { Bill, BillSearchResponse } from '$gen';

  import TablePanel from './TablePanel.svelte';
  import type { Column } from './table';
  import { createPanelData } from './data.svelte';
  import { congress as congressApi } from '../api/client';
  import { day, truncate } from '../format';

  const {
    id,
    query = '',
    congress = undefined,
  }: { id: string; query?: string; congress?: number } = $props();

  const data = createPanelData({
    load: (signal) => congressApi.bills(query, congress, 60, signal),
    refreshMs: 5 * 60_000,
  });

  const columns: Column<Bill>[] = [
    { header: 'BILL', cell: (r) => `${r.type.toUpperCase()} ${r.number}`, class: 'strong' },
    {
      header: '',
      // A word, not a tick: this is the only column that says how the story
      // ended, and a blank beside a live bill must not read as "failed".
      cell: (r) => (r.becameLaw ? 'LAW' : ''),
      class: (r) => (r.becameLaw ? 'up strong' : 'dim'),
    },
    { header: 'TITLE', cell: (r) => truncate(r.title, 62) },
    { header: 'SPONSOR', cell: (r) => truncate(r.sponsor, 26), class: 'dim' },
    {
      header: 'LATEST ACTION',
      cell: (r) => (r.latestAction ? truncate(r.latestAction.text, 48) : '—'),
      title: (r) => r.latestAction?.text ?? '',
      class: 'dim',
    },
    {
      header: 'ON',
      cell: (r) => (r.latestAction?.date ? day(r.latestAction.date) : '—'),
      class: 'num dim',
    },
  ];

  function note(loaded: BillSearchResponse): string {
    return `${loaded.congress}th Congress · ${loaded.source} · ${loaded.note}`;
  }
</script>

<TablePanel
  {id}
  kind="CONG"
  title={query ? `BILLS — ${query.toUpperCase()}` : 'BILLS'}
  subtitle={data.data ? `${(data.data as BillSearchResponse).bills.length} matched` : ''}
  {data}
  rows={(d: BillSearchResponse) => d.bills}
  {columns}
  {note}
  empty={(d: BillSearchResponse) => ({
    message: query
      ? `No recently-active bill matches "${query}".`
      : 'Congress.gov returned no bills.',
    hint: d.note,
  })}
  rowTitle={(r: Bill) => `${r.label} — ${r.url}`}
/>
