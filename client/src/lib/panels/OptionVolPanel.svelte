<!--
  VOL — the volatility smile for one expiry, and the at-the-money term
  structure behind it.

  Each rung of the smile is read from its *out-of-the-money* leg. In theory the
  two legs at a strike carry the same volatility — same strike, same expiry, one
  distribution — and in practice the in-the-money leg is where the spread is
  widest, the volume thinnest and, on American equity options, where the
  early-exercise premium sits. Both legs are shown so the reader can see the
  disagreement, but the headline number takes the traded side.

  The skew is quoted on delta rather than on strike, so it means the same thing
  on a $600 index and a $77,000 coin.
-->
<script lang="ts">
  import type { OptionSmilePoint, OptionSurface, OptionTermPoint } from '$gen';

  import DataTable from './DataTable.svelte';
  import PanelFrame from './PanelFrame.svelte';
  import type { Column } from './table';
  import { createPanelData } from './data.svelte';
  import { options as optionsApi } from '../api/client';
  import { compact, price } from '../format';
  import { skew, vol } from './options';

  const {
    id,
    symbol,
    expiry = undefined,
  }: { id: string; symbol: string; expiry?: string } = $props();

  const data = createPanelData({
    load: (signal) => optionsApi.surface(symbol, expiry, signal),
    refreshMs: 20_000,
  });

  const loaded = $derived(data.data as OptionSurface | undefined);

  const subtitle = $derived(
    loaded ? `${loaded.expiry.date} · ${Math.round(loaded.expiry.daysToExpiry)}d` : '',
  );

  const smileColumns: Column<OptionSmilePoint>[] = [
    { header: 'STRIKE', cell: (r) => price(r.strike), class: 'num strong' },
    {
      header: 'K/S',
      cell: (r) => (r.moneyness === null ? '--' : r.moneyness.toFixed(3)),
      class: 'num dim',
    },
    { header: 'IV', cell: (r) => vol(r.iv), class: 'num strong' },
    { header: 'CALL IV', cell: (r) => vol(r.callIv), class: 'num dim' },
    { header: 'PUT IV', cell: (r) => vol(r.putIv), class: 'num dim' },
    { header: 'VOL', cell: (r) => compact(r.volume), class: 'num dim' },
    { header: 'OPEN INT', cell: (r) => compact(r.openInterest), class: 'num dim' },
  ];

  const termColumns: Column<OptionTermPoint>[] = [
    { header: 'EXPIRY', cell: (r) => r.date, class: 'strong' },
    { header: 'DAYS', cell: (r) => String(Math.round(r.daysToExpiry)), class: 'num dim' },
    { header: 'ATM IV', cell: (r) => vol(r.atmIv), class: 'num strong' },
    { header: 'FORWARD', cell: (r) => price(r.forward), class: 'num dim' },
    { header: 'OPEN INT', cell: (r) => compact(r.openInterest), class: 'num dim' },
    { header: 'VOL', cell: (r) => compact(r.volume), class: 'num dim' },
  ];
</script>

{#snippet field(label: string, value: string, tone = '')}
  <div class="field">
    <span class="field-label">{label}</span>
    <span class="field-value {tone}">{value}</span>
  </div>
{/snippet}

<PanelFrame
  {id}
  kind="VOL"
  title={loaded ? `${loaded.symbol} VOLATILITY` : `${symbol} VOLATILITY`}
  {subtitle}
  subject={symbol}
  {data}
>
  {@const surface = data.data as OptionSurface}

  <div class="meta-strip">
    {@render field('SPOT', price(surface.spot), 'strong')}
    {@render field('FWD', price(surface.forward))}
    {@render field('ATM IV', vol(surface.atmIv), 'strong')}
    <!-- Positive means downside is bid: the shape of an index board almost
         always, and of a crypto board only when the market is frightened. -->
    {@render field('25Δ SKEW', skew(surface.skew), surface.skew === null ? 'dim' : '')}
    {@render field('VENUE', surface.venue)}
  </div>

  <div class="split">
    <section>
      <h3 class="section-head">SMILE</h3>
      {#if surface.smile.length === 0}
        <div class="panel-empty">No rungs carry a volatility for this expiry.</div>
      {:else}
        <DataTable rows={surface.smile} columns={smileColumns} />
      {/if}
    </section>

    <section>
      <h3 class="section-head">TERM STRUCTURE</h3>
      {#if surface.term.length === 0}
        <div class="panel-empty">No other expiries are listed.</div>
      {:else}
        <DataTable
          rows={surface.term}
          columns={termColumns}
          rowCommand={(r: OptionTermPoint) => `VOL ${surface.symbol} ${r.date}`}
          rowTitle={(r: OptionTermPoint) => `Read the smile at ${r.date}`}
        />
      {/if}
    </section>
  </div>

  {#if surface.note}
    <div class="panel-note">{surface.note}</div>
  {/if}
</PanelFrame>

<style>
  .split {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    align-items: flex-start;
  }

  section {
    flex: 1 1 320px;
    min-width: 0;
  }

  .section-head {
    margin: 0;
    padding: 2px 6px;
    font: inherit;
    color: var(--dim);
    letter-spacing: 0.08em;
    border-bottom: 1px solid var(--rule);
  }
</style>
