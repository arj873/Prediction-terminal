<!--
  STEAM — concurrent players, either the most-played leaderboard or one game.
-->
<script lang="ts">
  import type { SteamChart, SteamGame } from '$gen';

  import { ent } from '../api/client';
  import { compact, group, truncate } from '../format';
  import TablePanel from './TablePanel.svelte';
  import { createPanelData } from './data.svelte';
  import { countOrDash } from './media';
  import type { Column, Note } from './table';

  interface Props {
    id: string;
    /** A game name or app id; empty means the most-played leaderboard. */
    query: string;
  }

  const { id, query }: Props = $props();

  const data = createPanelData<SteamChart>({
    load: (signal) => ent.steam(query || undefined, 25, signal),
    // Concurrent players are a live figure — this is the one feed worth polling.
    refreshMs: 120_000,
  });

  /**
   * Valve's counts are `u64` on the server, so the generated type says `bigint`
   * while the JSON delivers a number. The `null` has to survive the conversion:
   * an absent count means no live figure right now, which is `--` and not `0`.
   */
  function players(value: bigint | null): number | null {
    return value === null ? null : Number(value);
  }

  const subtitle = $derived.by(() => {
    const chart = data.data;
    if (!chart) return '';
    if (chart.view === 'game') return chart.games[0]?.name ?? '';
    const total = chart.games.reduce((sum, game) => sum + (players(game.currentPlayers) ?? 0), 0);
    return `${compact(total)} players in the top ${chart.games.length}`;
  });

  function note(chart: SteamChart): Note {
    return chart.view === 'game'
      ? 'Live concurrent players, from Valve’s own API.'
      : 'Most-played on Steam right now.';
  }

  /** Only a game the chart named an app id for can be drilled into. */
  const rowCommand = (game: SteamGame): string | null =>
    game.appId ? `STEAM ${game.appId}` : null;
  const rowTitle = (game: SteamGame): string => `Live player count for ${game.name}`;

  const columns: Column<SteamGame>[] = [
    { header: '#', cell: (game) => countOrDash(game.rank), class: 'num strong' },
    { header: 'GAME', cell: (game) => truncate(game.name, 44), title: (game) => game.name },
    { header: 'PLAYERS', cell: (game) => group(players(game.currentPlayers)), class: 'num' },
    { header: 'PEAK 24H', cell: (game) => group(players(game.peakPlayers)), class: 'num dim' },
    { header: 'APPID', cell: (game) => (game.appId ? String(game.appId) : '—'), class: 'mono dim' },
  ];
</script>

<TablePanel
  {id}
  kind="STEAM"
  title={query ? truncate(query.toUpperCase(), 30) : 'TOP'}
  {subtitle}
  {data}
  rows={(chart) => chart.games}
  {columns}
  {note}
  {rowCommand}
  {rowTitle}
  tableClass="steam-table"
/>
