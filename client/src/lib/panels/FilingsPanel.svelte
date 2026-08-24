<!--
  SEC — what a filer has filed, newest first.

  The wire, not the archive: an 8-K appears on EDGAR within seconds of
  acceptance, which is often before the press release, and is what a market on a
  merger, a bankruptcy or an earnings date settles on. The form column is what
  a reader scans, so it leads.
-->
<script lang="ts">
  import type { SecFiling, SecFilingsResponse } from '$gen';

  import TablePanel from './TablePanel.svelte';
  import type { Column } from './table';
  import { createPanelData } from './data.svelte';
  import { sec as secApi } from '../api/client';
  import { day, truncate } from '../format';

  const {
    id,
    company,
    form = undefined,
  }: { id: string; company: string; form?: string } = $props();

  const data = createPanelData({
    // EDGAR accepts a filing continuously through the trading day.
    load: (signal) => secApi.filings(company, form, 60, signal),
    refreshMs: 60_000,
  });

  const columns: Column<SecFiling>[] = [
    { header: 'FORM', cell: (r) => r.form, class: 'strong' },
    { header: 'FILED', cell: (r) => day(r.filed), class: 'num' },
    { header: 'PERIOD', cell: (r) => (r.reportDate ? day(r.reportDate) : '—'), class: 'num dim' },
    // Only an 8-K cites items, and they are the reason to read one.
    { header: 'ITEMS', cell: (r) => r.items || '—', class: 'dim' },
    { header: 'DESCRIPTION', cell: (r) => truncate(r.description || r.primaryDocument, 60) },
  ];

  function note(loaded: SecFilingsResponse): string {
    const c = loaded.company;
    const bits = [
      `CIK ${c.cik}`,
      c.tickers.length ? c.tickers.join(', ') : '',
      c.sicDescription,
      c.exchanges.length ? c.exchanges.join(', ') : '',
    ].filter(Boolean);
    return `${bits.join(' · ')} · ${loaded.forms.length} form types on file`;
  }
</script>

<TablePanel
  {id}
  kind="SEC"
  title={data.data ? (data.data as SecFilingsResponse).company.name : company.toUpperCase()}
  subtitle={form ? `${form.toUpperCase()} only` : 'recent filings'}
  {data}
  rows={(d: SecFilingsResponse) => d.filings}
  {columns}
  {note}
  empty={() => ({ message: 'EDGAR lists no recent filings for this filer.' })}
  rowTitle={(r: SecFiling) => `${r.form} ${r.accession} — ${r.url}`}
/>
