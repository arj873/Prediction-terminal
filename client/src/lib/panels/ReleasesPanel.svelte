<!--
  REL — an artist's release calendar, from Apple Music.

  The column that matters is the date, and the rows that matter are the ones
  still ahead of it: a record listed for a date three weeks out is a pre-order,
  and it is the most direct evidence a "will they release by X" market has.
-->
<script lang="ts">
  import type { Release, ReleaseList } from '$gen';

  import { ent } from '../api/client';
  import { truncate } from '../format';
  import TablePanel from './TablePanel.svelte';
  import { createPanelData } from './data.svelte';
  import { countOrDash } from './media';
  import type { Column, Note } from './table';

  interface Props {
    id: string;
    /** The artist. */
    query: string;
    /** `album` or `song`. */
    kind: string;
  }

  const { id, query, kind }: Props = $props();

  const data = createPanelData<ReleaseList>({
    load: (signal) => ent.releases(query, kind || undefined, 25, signal),
  });

  const upcoming = $derived(data.data?.releases.filter((r) => r.upcoming).length ?? 0);

  const subtitle = $derived.by(() => {
    const list = data.data;
    if (!list) return '';
    return upcoming > 0
      ? `${list.releases.length} releases · ${upcoming} announced`
      : `${list.releases.length} releases`;
  });

  function note(list: ReleaseList): Note {
    if (upcoming > 0) {
      return [
        { text: `${upcoming} not out yet` },
        { text: '— an announced date is a pre-order, not a guarantee.', tone: 'dim' },
      ];
    }
    return `${list.kindLabel} credited to ${list.query}, newest first.`;
  }

  const rowCommand = (): string | null => null;
  const rowTitle = (release: Release): string => `${release.title} — ${release.artist}`;

  const columns: Column<Release>[] = [
    {
      header: 'DATE',
      cell: (release) => release.date,
      class: (release) => (release.upcoming ? 'mono up strong' : 'mono dim'),
    },
    {
      header: 'TITLE',
      cell: (release) => truncate(release.title, 42),
      title: (release) => release.title,
    },
    {
      header: 'ARTIST',
      cell: (release) => truncate(release.artist, 26),
      class: 'dim',
    },
    { header: 'TRACKS', cell: (release) => countOrDash(release.trackCount), class: 'num dim' },
    { header: 'GENRE', cell: (release) => truncate(release.genre, 16) || '—', class: 'dim' },
  ];
</script>

<TablePanel
  {id}
  kind="REL"
  title={truncate(query.toUpperCase(), 30)}
  {subtitle}
  {data}
  rows={(list: ReleaseList) => list.releases}
  {columns}
  {note}
  {rowCommand}
  {rowTitle}
/>
