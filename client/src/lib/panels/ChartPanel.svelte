<!--
  GP — the contract price chart, at any of the three venues.

  Candlesticks (or an area line) of the YES contract price, volume underneath,
  and a crosshair legend that reads out the bar under the cursor. Prices are
  drawn in dollars and labelled in cents, which is how a prediction market is
  quoted.

  Not every venue publishes bars. Polymarket International publishes a price
  sample series, which the server buckets and labels as such — the label lands
  in the subtitle and in the empty state. Polymarket US publishes nothing public
  at all: its 501 carries an explanation, and `PanelFrame` prints that (message
  and hint) rather than an empty pane.
-->
<script lang="ts">
  import { untrack } from 'svelte';
  import type { Candle, CandlesResponse, Market } from '$gen';
  import type { MouseEventParams, Time } from 'lightweight-charts';

  import EmptyState from './EmptyState.svelte';
  import PanelFrame from './PanelFrame.svelte';
  import { createPanelData } from './data.svelte';
  import { venue, type CandleInterval } from '../api/client';
  import Chart, { type SeriesSpec } from '../chart/Chart.svelte';
  import { toCandleData, toLineData, toVolumeData } from '../chart/data';
  import { getTerminalContext } from '../context';
  import { cents, compact, countdown, direction, signedCents, truncate } from '../format';
  import type { ChartProps } from '../terminal/registry';
  import { formatRef, venueInfo } from '../terminal/venue';

  const { id, ref, interval, lookbackSeconds, style }: { id: string } & ChartProps = $props();

  interface ChartData {
    market: Market | null;
    candles: CandlesResponse;
  }

  const INTERVAL_LABEL: Record<CandleInterval, string> = { 1: '1m', 60: '1h', 1440: '1d' };

  const { workspace } = getTerminalContext();

  const data = createPanelData<ChartData>({
    load: async (signal) => {
      const end = Math.floor(Date.now() / 1000);
      const start = end - lookbackSeconds;

      // The quote header is a nicety; a market that 404s should not kill the chart.
      const [candles, market] = await Promise.all([
        venue.candles(ref, interval, start, end, signal),
        venue.market(ref, signal).catch(() => null),
      ]);

      return { candles, market };
    },
    // 1-minute bars move constantly; daily bars do not. Poll accordingly.
    refreshMs: () => (interval === 1 ? 15_000 : 60_000),
  });

  const loaded = $derived(data.data);
  const bars = $derived<readonly Candle[]>(loaded?.candles.candles ?? []);

  const subtitle = $derived.by(() => {
    const label = `${venueInfo(ref.venue).label} · ${INTERVAL_LABEL[interval]} · ${style}`;
    const market = loaded?.market;
    if (market) return `${label} · ${truncate(market.title, 52)}`;
    const note = loaded?.candles.note;
    return note ? `${label} · ${truncate(note, 52)}` : label;
  });

  /* --------------------------------------------------------------- series */

  const series = $derived.by((): SeriesSpec[] => {
    // Volume bars carry their up/down tint in the data, so a `THEME` change has
    // to re-shape them; the chart's own restyle can only reach series options.
    void workspace.theme;

    return [
      // Stable ids: the price series survives a poll, and with it the reader's
      // zoom. A style switch changes the series *kind* under the same id, which
      // the chart reads as a different series wearing the same name.
      {
        id: 'price',
        kind: style === 'candle' ? 'candles' : 'probability-area',
        data: style === 'candle' ? toCandleData(bars) : toLineData(bars),
      },
      { id: 'volume', kind: 'volume', data: toVolumeData(bars) },
    ];
  });

  /**
   * What is being charted, as opposed to what the data currently says.
   *
   * Read imperatively and applied only once a load has landed: bumping this the
   * moment the props change would refit the chart to the bars still on screen
   * and leave the newly requested window off to one side.
   */
  function identity(): string {
    return `${ref.venue}:${ref.id}|${interval}|${lookbackSeconds}|${style}`;
  }

  let fitKey = $state(identity());

  $effect(() => {
    void data.data;
    fitKey = untrack(identity);
  });

  /* --------------------------------------------------------------- legend */

  /**
   * The bar under the crosshair, or nothing when the cursor is off the pane.
   *
   * Only moved to a time the price series actually has a bar for — over a gap
   * the previous reading stands rather than blanking.
   */
  let cursorTime = $state<number | undefined>(undefined);

  function onCrosshair(param: MouseEventParams<Time>): void {
    if (!param.time || param.point === undefined) {
      cursorTime = undefined;
      return;
    }
    const time = Number(param.time);
    if (bars.some((bar) => bar.time === time)) cursorTime = time;
  }

  const bar = $derived(
    (cursorTime === undefined ? undefined : bars.find((b) => b.time === cursorTime)) ?? bars.at(-1),
  );

  /** `2026-08-16 14:25` on intraday bars, `2026-08-16` on daily ones. */
  function legendTime(time: number): string {
    const at = new Date(time * 1000).toISOString();
    return interval === 1440 ? at.slice(0, 10) : at.slice(0, 16).replace('T', ' ');
  }

  /* ---------------------------------------------------------------- stats */

  const stats = $derived.by(() => {
    const market = loaded?.market;
    const first = bars[0];
    const last = bars.at(-1);

    const windowChange = first && last ? last.close - first.open : null;
    const traded = bars.filter((candle) => candle.traded).length;

    return [
      { label: 'BID', value: cents(market?.yesBid ?? null), tone: '' },
      { label: 'ASK', value: cents(market?.yesAsk ?? null), tone: '' },
      { label: 'LAST', value: cents(market?.lastPrice ?? last?.close ?? null), tone: '' },
      {
        label: 'CHG',
        value: signedCents(market?.change ?? null),
        tone: direction(market?.change ?? null),
      },
      { label: 'WIN', value: signedCents(windowChange), tone: direction(windowChange) },
      { label: 'VOL', value: compact(market?.volume ?? null), tone: '' },
      { label: '24H', value: compact(market?.volume24h ?? null), tone: '' },
      { label: 'OI', value: compact(market?.openInterest ?? null), tone: '' },
      { label: 'BARS', value: `${bars.length} (${traded} traded)`, tone: '' },
      { label: 'CLOSE', value: countdown(market?.closeTime ?? null), tone: '' },
    ];
  });
</script>

<PanelFrame {id} kind="GP" title={formatRef(ref)} {subtitle} subject={formatRef(ref)} {data}>
  <div class="chart-wrap">
    {#if bar}
      <!--
        Whitespace between these spans is not rendered: `.chart-legend` and
        `.legend-pair` are flex containers, so whitespace-only text between
        items produces no box.
      -->
      <div class="chart-legend">
        <span class="legend-time">{legendTime(bar.time)}Z</span>
        <span class="legend-pair">
          <span class="legend-key">O</span>
          <span>{cents(bar.open)}</span>
        </span>
        <span class="legend-pair">
          <span class="legend-key">H</span>
          <span>{cents(bar.high)}</span>
        </span>
        <span class="legend-pair">
          <span class="legend-key">L</span>
          <span>{cents(bar.low)}</span>
        </span>
        <span class="legend-pair">
          <span class="legend-key">C</span>
          <span class={direction(bar.close - bar.open)}>{cents(bar.close)}</span>
        </span>
        <span class="legend-pair">
          <span class="legend-key">V</span>
          <span>{compact(bar.volume)}</span>
        </span>
        {#if !bar.traded}
          <!-- A synthesised bar is not a real print; say so rather than implying a trade. -->
          <span class="legend-flag">NO TRADE</span>
        {/if}
      </div>
    {/if}

    {#if bars.length === 0}
      <div class="chart-host">
        <EmptyState
          message="No candles in this window."
          hint={loaded?.candles.note ??
            'This market may not have traded yet. Try a wider range, e.g. `GP <ticker> 1d 1y`.'}
        />
      </div>
    {:else}
      <Chart {series} {fitKey} {onCrosshair} volumePaneHeight={70} />
    {/if}
  </div>

  <div class="chart-stats">
    {#each stats as stat (stat.label)}
      <span class="stat">
        <span class="stat-label">{stat.label}</span>
        <span class="stat-value {stat.tone}">{stat.value}</span>
      </span>
    {/each}
  </div>
</PanelFrame>
