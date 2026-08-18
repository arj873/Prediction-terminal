<!--
  BO — the domestic daily box office, from Box Office Mojo.
-->
<script lang="ts">
  import type { BoxOfficeDay, BoxOfficeEntry } from '$gen';

  import { ent } from '../api/client';
  import { group, money, signedPercent, truncate } from '../format';
  import TablePanel from './TablePanel.svelte';
  import { createPanelData } from './data.svelte';
  import { countOrDash, moveColumn, signedTone } from './media';
  import type { Column, Note } from './table';

  interface Props {
    id: string;
    /** `YYYY-MM-DD`, or nothing for the latest day published. */
    date?: string;
  }

  const { id, date = undefined }: Props = $props();

  const data = createPanelData<BoxOfficeDay>({
    load: (signal) => ent.boxOffice(date, signal),
    refreshMs: 30 * 60_000,
  });

  function note(day: BoxOfficeDay): Note {
    return [
      { text: day.title },
      { text: `  · ${money(day.totalGross)} across ${day.entries.length}`, tone: 'dim' },
    ];
  }

  /** The Rotten Tomatoes score is the natural next question about a release. */
  const rowCommand = (entry: BoxOfficeEntry): string => `RT ${entry.title}`;
  const rowTitle = (entry: BoxOfficeEntry): string =>
    `${entry.title}\nClick for the Rotten Tomatoes score`;

  const columns: Column<BoxOfficeEntry>[] = [
    { header: '#', cell: (entry) => String(entry.rank), class: 'num strong' },
    moveColumn<BoxOfficeEntry>(),
    {
      header: 'RELEASE',
      cell: (entry) => truncate(entry.title, 40),
      title: (entry) => entry.title,
    },
    { header: 'DAILY', cell: (entry) => money(entry.gross), class: 'num' },
    {
      header: '%YD',
      cell: (entry) => signedPercent(entry.changeDay, 1),
      class: (entry) => signedTone(entry.changeDay),
    },
    {
      header: '%LW',
      cell: (entry) => signedPercent(entry.changeWeek, 1),
      class: (entry) => signedTone(entry.changeWeek),
    },
    { header: 'THTRS', cell: (entry) => group(entry.theaters), class: 'num dim' },
    { header: 'TO DATE', cell: (entry) => money(entry.totalGross), class: 'num' },
    { header: 'DAYS', cell: (entry) => countOrDash(entry.daysInRelease), class: 'num dim' },
    { header: 'STUDIO', cell: (entry) => truncate(entry.distributor, 22), class: 'dim' },
  ];
</script>

<TablePanel
  {id}
  kind="BO"
  title={data.data ? data.data.date : (date ?? 'LATEST')}
  subtitle={data.data
    ? `${data.data.entries.length} releases · ${money(data.data.totalGross)} total`
    : ''}
  {data}
  rows={(day) => day.entries}
  {columns}
  {note}
  {rowCommand}
  {rowTitle}
  tableClass="bo-table"
/>
