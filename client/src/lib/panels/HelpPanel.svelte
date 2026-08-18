<!--
  HELP — generated from the command table, so it cannot drift from what the
  terminal actually does.

  It reads `terminal/commands.ts` rather than `panels/registry.ts`: the table
  names panel *kinds* as strings and imports no component, which is what keeps
  the help panel out of the import cycle the old registry sat in.
-->
<script lang="ts">
  import PanelFrame from './PanelFrame.svelte';
  import DataTable from './DataTable.svelte';
  import { createPanelData } from './data.svelte';
  import type { Column } from './table';
  import { navigable } from '../actions/navigable';
  import { getRowCursor, getTerminalContext } from '../context';
  import type { Command } from '../terminal/command';
  import { COMMANDS, COMMAND_INDEX } from '../terminal/commands';

  const { id, topic = undefined }: { id: string; topic?: string } = $props();

  const { run } = getTerminalContext();

  /**
   * Nothing is fetched — the command table is already in memory. Going through
   * `createPanelData` anyway is what keeps the `PanelFrame` contract honest:
   * the frame renders a body only once `data.data` is defined, and a panel that
   * faked that field would be lying to the one thing that reads it.
   */
  const data = createPanelData({ load: async () => COMMANDS });

  const GROUP_TITLES: Record<Command['group'], string> = {
    markets: 'PREDICTION MARKETS',
    data: 'DATA SOURCES',
    workspace: 'WORKSPACE',
  };

  const GROUPS = ['markets', 'data', 'workspace'] as const;

  const command = $derived(topic ? COMMAND_INDEX.get(topic) : undefined);

  const columns: Column<Command>[] = [
    { header: 'CMD', cell: (c) => c.verb, class: 'mono strong' },
    { header: 'USAGE', cell: (c) => c.usage, class: 'mono dim' },
    { header: 'DESCRIPTION', cell: (c) => c.summary },
    { header: 'ALIASES', cell: (c) => (c.aliases ?? []).join(' '), class: 'mono dim' },
  ];

  /**
   * The key map is generated from the bindings themselves, in its own panel —
   * duplicating it here is how the two would come to disagree. What is left is
   * the handful worth knowing before you have read it.
   */
  const KEY_HINTS: readonly (readonly [string, string])[] = [
    ['Enter', 'Run the command'],
    ['↑ / ↓', 'Previous / next command in history'],
    ['Tab', 'Complete the verb'],
    ['Esc', 'Clear the line — again on an empty line hands the keyboard to the panels'],
    ['Alt+1…9', 'Focus that panel'],
    ['Alt+↑/↓', 'Move the row cursor · Alt+Enter opens the row'],
    ['Ctrl+←/→', 'Move focus between panels'],
    ['Alt+F', 'Maximise the focused panel'],
    ['Alt+Q', 'Close the focused panel · Ctrl+L clears the log'],
    ['Alt+K', 'The key map, where all of this can be rebound'],
  ];

  const SOURCES: readonly { text: string; tone?: string }[] = [
    { text: 'Kalshi — public trade-api v2. Prices in cents; a contract settles at $1.' },
    {
      text: 'Polymarket International — gamma-api for the catalogue, the CLOB for books and price history. Prefix a slug with pm:.',
    },
    {
      text: 'Polymarket US — the public gateway. Quote and book only: its trade tape and candles need an API key, and the terminal holds none. Prefix a slug with pmus:.',
    },
    {
      text: 'FRED — scraped from fred.stlouisfed.org. Set FRED_API_KEY for an API fallback if the scrape is blocked.',
    },
    { text: 'Billboard — scraped from billboard.com/charts.' },
    { text: 'Rotten Tomatoes — scraped from rottentomatoes.com. Settles Kalshi KXRT.' },
    { text: 'Netflix — the official Top 10 dataset published at netflix.com/tudum/top10.' },
    {
      text: 'Spotify / YouTube — mirrored by kworb.net; both platforms’ own charts need a login or render client-side.',
    },
    { text: 'Box office — scraped from boxofficemojo.com daily charts.' },
    { text: 'Steam — Valve’s public API for live players, steamcharts.com for the leaderboard.' },
    { text: 'TV schedules — the TVmaze public API.' },
    {
      text: 'News — Alpaca’s wire (Benzinga). The one keyed feed: set ALPACA_API_KEY_ID and ALPACA_API_SECRET_KEY.',
    },
    {
      tone: 'dim',
      text: 'Read-only market data. Nothing here places an order; the Alpaca key is used to read news and nothing else.',
    },
  ];
</script>

<PanelFrame {id} kind="HELP" title={topic ?? 'COMMANDS'} {data}>
  {@const cursor = getRowCursor()}

  {#if topic}
    {#if !command}
      <div class="panel-empty">
        <div>No command called {topic}.</div>
        <div class="panel-empty-hint">Run HELP for the full list.</div>
      </div>
    {:else}
      <div class="help-detail">
        <h3 class="help-group-title">{command.verb}</h3>
        <p class="help-summary">{command.summary}</p>
        <div class="help-usage">
          <span class="help-label">USAGE</span>
          <code>{command.usage}</code>
        </div>
        {#if (command.aliases ?? []).length > 0}
          <div class="help-usage">
            <span class="help-label">ALIASES</span>
            <code>{(command.aliases ?? []).join('  ')}</code>
          </div>
        {/if}
      </div>

      {#if command.examples?.length}
        <div class="help-detail">
          <span class="help-label">EXAMPLES (click to run)</span>
          <div class="help-examples">
            {#each command.examples as example (example)}
              <!--
                Every example is a working command, so it is a click target —
                and therefore a place the row cursor has to be able to land.
              -->
              <button
                class="example"
                type="button"
                onclick={() => run(example)}
                use:navigable={{ cursor, command: example }}>{example}</button
              >
            {/each}
          </div>
        </div>
      {/if}
    {/if}
  {:else}
    {@const commands = data.data as Command[]}
    <div class="help-intro">
      <p>
        Type a command and press Enter. ↑/↓ walks history, Tab completes, Esc clears the line. Click
        any table row to drill in.
      </p>
    </div>

    {#each GROUPS as group (group)}
      <div class="help-group">
        <h3 class="help-group-title">{GROUP_TITLES[group]}</h3>
        <DataTable
          rows={commands.filter((c) => c.group === group)}
          {columns}
          rowCommand={(c: Command) => `HELP ${c.verb}`}
        />
      </div>
    {/each}

    <div class="help-group">
      <h3 class="help-group-title">KEYS — run KEYS for the whole map</h3>
      <DataTable
        rows={KEY_HINTS}
        columns={[
          { header: 'KEY', cell: (hint: readonly string[]) => hint[0] ?? '', class: 'mono strong' },
          { header: 'ACTION', cell: (hint: readonly string[]) => hint[1] ?? '' },
        ]}
      />
    </div>

    <div class="help-group">
      <h3 class="help-group-title">DATA SOURCES</h3>
      <ul class="help-list">
        {#each SOURCES as source (source.text)}
          <li class={source.tone}>{source.text}</li>
        {/each}
      </ul>
    </div>
  {/if}
</PanelFrame>
