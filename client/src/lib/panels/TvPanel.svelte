<!--
  TV — a day's broadcast schedule, from TVmaze.
-->
<script lang="ts">
  import type { TvEpisode, TvSchedule } from '$gen';

  import { ent } from '../api/client';
  import { day, truncate } from '../format';
  import TablePanel from './TablePanel.svelte';
  import { createPanelData } from './data.svelte';
  import type { Column, EmptyState, Note } from './table';

  interface Props {
    id: string;
    /** `YYYY-MM-DD`, or nothing for today. */
    date?: string;
    /** ISO-3166 alpha-2; the server defaults to US. */
    country?: string;
  }

  const { id, date = undefined, country = undefined }: Props = $props();

  const data = createPanelData<TvSchedule>({
    load: (signal) => ent.tv(date, country, signal),
    refreshMs: 30 * 60_000,
  });

  function note(schedule: TvSchedule): Note {
    return schedule.episodes.length === 0
      ? null
      : `${schedule.episodes.length} airings · ${schedule.country} · ${day(schedule.date)} · via TVmaze`;
  }

  function empty(schedule: TvSchedule): EmptyState {
    return {
      message: `Nothing scheduled for ${schedule.country} on ${day(schedule.date)}.`,
      hint: 'Try another date: `TV 2026-08-20`.',
    };
  }

  /** A show is the natural search term for the market that trades on it. */
  const rowCommand = (episode: TvEpisode): string => `SRCH ${episode.show}`;
  const rowTitle = (episode: TvEpisode): string => `Search Kalshi for "${episode.show}"`;

  const columns: Column<TvEpisode>[] = [
    { header: 'TIME', cell: (episode) => episode.airtime || '—', class: 'num' },
    {
      header: 'SHOW',
      cell: (episode) => truncate(episode.show, 34),
      class: 'strong',
      title: (episode) => episode.show,
    },
    { header: 'NETWORK', cell: (episode) => truncate(episode.network, 18), class: 'dim' },
    {
      header: 'EP',
      class: 'mono dim',
      cell: (episode) =>
        episode.season !== null && episode.episode !== null
          ? `S${String(episode.season).padStart(2, '0')}E${String(episode.episode).padStart(2, '0')}`
          : '—',
    },
    {
      header: 'EPISODE',
      cell: (episode) => truncate(episode.name, 34),
      title: (episode) => episode.name,
    },
    {
      header: 'RUN',
      cell: (episode) => (episode.runtime === null ? '—' : `${episode.runtime}m`),
      class: 'num dim',
    },
  ];
</script>

<TablePanel
  {id}
  kind="TV"
  title={data.data ? `${data.data.country} ${data.data.date}` : (date ?? 'TODAY')}
  subtitle={data.data ? `${data.data.episodes.length} airings` : ''}
  {data}
  rows={(schedule) => schedule.episodes}
  {columns}
  {note}
  {empty}
  {rowCommand}
  {rowTitle}
  tableClass="tv-table"
/>
