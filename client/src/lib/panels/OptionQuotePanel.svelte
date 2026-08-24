<!--
  OPD — one contract, its pair leg, and everything the model can say about it.

  The Greeks are stated in the units a trader reads them in, and the panel says
  so in the header rather than leaving the reader to guess whether vega is per
  point or per unit. It also says where the volatility came from: a venue's own
  mark and one solved from the book mid are different claims, and a panel that
  printed them identically would be hiding which.

  Only Deribit publishes free option price history, so an equity contract gets a
  sentence saying that rather than an empty chart that reads as a broken load.
-->
<script lang="ts">
  import type { OptionContract, OptionQuoteResponse } from '$gen';

  import PanelFrame from './PanelFrame.svelte';
  import { createPanelData } from './data.svelte';
  import { options as optionsApi } from '../api/client';
  import Chart from '../chart/Chart.svelte';
  import { toCandleData } from '../chart/data';
  import { day, price } from '../format';
  import { greek, rate, vol } from './options';

  const { id, contract }: { id: string; contract: string } = $props();

  const data = createPanelData({
    load: (signal) => optionsApi.contract(contract, 60, signal),
    refreshMs: 10_000,
  });

  const loaded = $derived(data.data as OptionQuoteResponse | undefined);

  const subtitle = $derived(
    loaded
      ? `${loaded.contract.type.toUpperCase()} ${price(loaded.contract.strike)} · ${day(
          new Date(loaded.contract.expiry * 1000).toISOString().slice(0, 10),
        )}`
      : '',
  );

  /**
   * `venue` and `solved` are different claims about the same number. A venue's
   * mark IV is what its own risk system uses; a solved one is this terminal's
   * reading of the book against a forward it fitted.
   */
  function ivWord(source: string | null): string {
    if (source === 'venue') return 'venue mark';
    if (source === 'solved') return 'solved from mid';
    return '--';
  }
</script>

{#snippet field(label: string, value: string, tone = '')}
  <div class="field">
    <span class="field-label">{label}</span>
    <span class="field-value {tone}">{value}</span>
  </div>
{/snippet}

{#snippet leg(title: string, c: OptionContract)}
  <div class="leg">
    <div class="leg-head">{title}</div>
    <div class="leg-grid">
      {@render field('BID', price(c.bid))}
      {@render field('ASK', price(c.ask))}
      {@render field('MID', price(c.mid), 'strong')}
      {@render field('LAST', price(c.last))}
      {@render field('IV', vol(c.iv))}
      {@render field('THEO', price(c.theo))}
      {@render field('INTRINSIC', price(c.intrinsic))}
      {@render field('EXTRINSIC', price(c.extrinsic))}
      {@render field('BREAKEVEN', price(c.breakeven))}
      {@render field('OPEN INT', c.openInterest === null ? '--' : String(c.openInterest))}
      {@render field('VOLUME', c.volume === null ? '--' : String(c.volume))}
      {@render field('MONEY', c.inTheMoney ? 'in' : 'out')}
    </div>
    <div class="greeks">
      {@render field('DELTA /unit', greek(c.greeks.delta, 4))}
      {@render field('GAMMA /unit', greek(c.greeks.gamma, 6))}
      {@render field('VEGA /pt', greek(c.greeks.vega, 4))}
      {@render field('THETA /day', greek(c.greeks.theta, 4))}
      {@render field('RHO /pt', greek(c.greeks.rho, 4))}
    </div>
  </div>
{/snippet}

<PanelFrame {id} kind="OPD" title={contract} {subtitle} subject={contract} {data}>
  {@const quote = data.data as OptionQuoteResponse}

  <div class="meta-strip">
    {@render field('SPOT', price(quote.spot), 'strong')}
    {@render field('FWD', price(quote.forward))}
    {@render field('FWD SRC', quote.forwardSource, quote.forwardSource === 'assumed' ? 'warn' : '')}
    {@render field('IV FROM', ivWord(quote.contract.ivSource))}
    {@render field('RATE', rate(quote.rate))}
    {@render field('CARRY', rate(quote.carry))}
    {@render field('SIZE', `${quote.contractSize}×`)}
    {@render field('VENUE', quote.venue)}
  </div>

  {#if quote.forwardSource === 'assumed'}
    <div class="panel-note warn">
      Nothing in this board would state a forward, so the Greeks rest on an assumed carry.
    </div>
  {/if}

  <div class="legs">
    {@render leg(quote.contract.type.toUpperCase(), quote.contract)}
    {#if quote.pair}
      {@render leg(`${quote.pair.type.toUpperCase()} (pair)`, quote.pair)}
    {/if}
  </div>

  {#if quote.history.length > 0}
    <div class="chart-wrap">
      <Chart
        series={[{ id: 'opd', kind: 'candles', data: toCandleData(quote.history) }]}
        fitKey={contract}
      />
    </div>
  {:else if quote.historyNote}
    <div class="panel-note">{quote.historyNote}</div>
  {/if}

  {#if quote.note}
    <div class="panel-note">{quote.note}</div>
  {/if}
</PanelFrame>

<style>
  .legs {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    padding: 4px 6px;
  }

  .leg {
    flex: 1 1 260px;
    min-width: 0;
    border: 1px solid var(--rule);
  }

  .leg-head {
    padding: 1px 6px;
    color: var(--dim);
    letter-spacing: 0.08em;
    border-bottom: 1px solid var(--rule);
  }

  .leg-grid,
  .greeks {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(120px, 1fr));
    gap: 0 10px;
    padding: 3px 6px;
  }

  /* The Greeks carry their units in the label, because a vega per unit and a
     vega per point differ by exactly 100 and nothing else would notice. */
  .greeks {
    border-top: 1px solid var(--rule);
  }

  .chart-wrap {
    min-height: 180px;
    padding: 4px 6px;
  }
</style>
