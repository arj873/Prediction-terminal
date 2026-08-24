<!--
  OI — open interest and volume by strike, and the pain curve behind them.

  Max pain is the settlement price at which the least option value pays out. It
  is a positioning read and not a forecast, and the panel says so: the *curve* is
  published rather than only its minimum, because the shape is what shows whether
  the low is a sharp pin or a flat basin the underlying could sit anywhere in.

  The payout is scaled by the contract size — 100 shares to a US listed contract,
  1 coin on Deribit — so the number is the money actually at stake rather than a
  per-share figure that reads a hundred times too small.
-->
<script lang="ts">
  import type { OptionPositioning, OptionStrikeStat } from '$gen';

  import DataTable from './DataTable.svelte';
  import PanelFrame from './PanelFrame.svelte';
  import type { Column } from './table';
  import { createPanelData } from './data.svelte';
  import { options as optionsApi } from '../api/client';
  import { compact, money, price } from '../format';

  const {
    id,
    symbol,
    expiry = undefined,
  }: { id: string; symbol: string; expiry?: string } = $props();

  const data = createPanelData({
    load: (signal) => optionsApi.positioning(symbol, expiry, signal),
    refreshMs: 30_000,
  });

  const loaded = $derived(data.data as OptionPositioning | undefined);

  const subtitle = $derived(
    loaded ? `${loaded.expiry.date} · ${Math.round(loaded.expiry.daysToExpiry)}d` : '',
  );

  const columns: Column<OptionStrikeStat>[] = [
    { header: 'STRIKE', cell: (r) => price(r.strike), class: 'num strong' },
    { header: 'CALL OI', cell: (r) => compact(r.callOpenInterest), class: 'num' },
    { header: 'PUT OI', cell: (r) => compact(r.putOpenInterest), class: 'num' },
    {
      header: 'P/C',
      cell: (r) =>
        r.callOpenInterest > 0 ? (r.putOpenInterest / r.callOpenInterest).toFixed(2) : '--',
      class: 'num dim',
    },
    { header: 'CALL VOL', cell: (r) => compact(r.callVolume), class: 'num dim' },
    { header: 'PUT VOL', cell: (r) => compact(r.putVolume), class: 'num dim' },
    {
      header: 'PAIN PAYOUT',
      cell: (r) => (r.painPayout === null ? '--' : money(r.painPayout)),
      class: 'num dim',
    },
  ];

  function ratio(value: number | null): string {
    return value === null ? '--' : value.toFixed(2);
  }
</script>

{#snippet field(label: string, value: string, tone = '')}
  <div class="field">
    <span class="field-label">{label}</span>
    <span class="field-value {tone}">{value}</span>
  </div>
{/snippet}

<PanelFrame
  {id}
  kind="OI"
  title={loaded ? `${loaded.symbol} POSITIONING` : `${symbol} POSITIONING`}
  {subtitle}
  subject={symbol}
  {data}
>
  {@const board = data.data as OptionPositioning}

  <div class="meta-strip">
    {@render field('SPOT', price(board.spot), 'strong')}
    {@render field('MAX PAIN', price(board.maxPain), 'strong')}
    {@render field('CALL OI', compact(board.totalCallOpenInterest))}
    {@render field('PUT OI', compact(board.totalPutOpenInterest))}
    {@render field('P/C OI', ratio(board.putCallOpenInterest))}
    {@render field('P/C VOL', ratio(board.putCallVolume))}
    {@render field('VENUE', board.venue)}
  </div>

  <!-- Said plainly, and every time: a pin is where writers lose least, not
       where the underlying is going. -->
  <div class="panel-note">
    Max pain is the settlement price at which the least option value pays out — a read on how the
    board is positioned, not a forecast of where it settles.
  </div>

  {#if board.strikes.length === 0}
    <div class="panel-empty">No open interest is listed for this expiry.</div>
  {:else}
    <DataTable
      rows={board.strikes}
      {columns}
      rowClass={(r: OptionStrikeStat) => (r.strike === board.maxPain ? 'max-pain' : undefined)}
      rowTitle={(r: OptionStrikeStat) =>
        r.strike === board.maxPain ? 'The strike where writers pay out least' : undefined}
    />
  {/if}

  {#if board.note}
    <div class="panel-note">{board.note}</div>
  {/if}
</PanelFrame>

<style>
  :global(tr.max-pain td) {
    background: color-mix(in srgb, var(--accent) 14%, transparent);
  }
</style>
