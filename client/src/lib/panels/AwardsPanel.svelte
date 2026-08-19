<!--
  AWRD — nominees and winners, from Wikidata.

  Two things the panel has to be honest about, because both are properties of
  the source rather than of the award: a nominee slate fills in over the days
  after a ceremony, so the count shown is the count on record and not the size
  of the field; and a ceremony that has not happened yet is empty, which is the
  answer an open market is asking for rather than a failure to find anything.
-->
<script lang="ts">
  import type { AwardEntry, AwardResult } from '$gen';

  import { ent } from '../api/client';
  import { truncate } from '../format';
  import TablePanel from './TablePanel.svelte';
  import { createPanelData } from './data.svelte';
  import type { Column, Note } from './table';

  interface Props {
    id: string;
    /** The award, in full or by one of the short forms. */
    query: string;
    /** One ceremony, or every one on record. */
    year?: number;
  }

  const { id, query, year }: Props = $props();

  const data = createPanelData<AwardResult>({
    load: (signal) => ent.awards(query, year, 300, signal),
  });

  const subtitle = $derived.by(() => {
    const result = data.data;
    if (!result) return '';
    const ceremonies = result.years.length;
    return result.year !== null
      ? `${result.year} ceremony`
      : `${ceremonies} ${ceremonies === 1 ? 'ceremony' : 'ceremonies'} on record`;
  });

  function note(result: AwardResult): Note {
    // The source's own caveat wins over anything generic: an empty ceremony and
    // a half-filled slate are the two readings that would otherwise mislead.
    if (result.note) return [{ text: result.note, tone: 'warn' }];
    return 'Nominees and winners as recorded on Wikidata.';
  }

  /** Nothing to drill into: a recipient is not a market. */
  const rowCommand = (): string | null => null;
  const rowTitle = (entry: AwardEntry): string =>
    entry.work ? `${entry.name} — ${entry.work}` : entry.name;

  const columns: Column<AwardEntry>[] = [
    {
      header: 'YEAR',
      cell: (entry) => (entry.year === null ? '—' : String(entry.year)),
      class: 'num dim',
    },
    {
      header: '',
      cell: (entry) => (entry.won ? 'WON' : ''),
      class: 'strong up',
    },
    {
      header: 'RECIPIENT',
      cell: (entry) => truncate(entry.name, 40),
      title: (entry) => entry.name,
    },
    {
      header: 'FOR',
      cell: (entry) => truncate(entry.work, 34) || '—',
      title: (entry) => entry.work,
      class: 'dim',
    },
  ];
</script>

<TablePanel
  {id}
  kind="AWRD"
  title={truncate(data.data?.award ?? query, 34).toUpperCase()}
  {subtitle}
  {data}
  rows={(result: AwardResult) => result.entries}
  {columns}
  {note}
  {rowCommand}
  {rowTitle}
/>
