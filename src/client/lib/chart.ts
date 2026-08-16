/**
 * lightweight-charts wiring.
 *
 * One place that knows about the charting library, so panels deal in data and
 * the terminal's look stays consistent. The theme is read from the live CSS
 * custom properties rather than hard-coded, which means `THEME amber|green|ice`
 * restyles every open chart with no per-panel bookkeeping.
 */

import {
  AreaSeries,
  CandlestickSeries,
  ColorType,
  CrosshairMode,
  HistogramSeries,
  LineSeries,
  LineStyle,
  createChart,
  type CandlestickData,
  type DeepPartial,
  type HistogramData,
  type IChartApi,
  type ISeriesApi,
  type LineData,
  type MouseEventParams,
  type SeriesOptionsCommon,
  type Time,
  type TimeChartOptions,
  type UTCTimestamp,
  type WhitespaceData,
} from 'lightweight-charts';

export type { IChartApi, ISeriesApi, MouseEventParams, Time, UTCTimestamp };

function cssVar(name: string, fallback: string): string {
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return value || fallback;
}

export interface TerminalChartTheme {
  background: string;
  text: string;
  dim: string;
  grid: string;
  border: string;
  up: string;
  down: string;
  accent: string;
  accentSoft: string;
}

export function chartTheme(): TerminalChartTheme {
  return {
    background: cssVar('--chart-bg', '#07070a'),
    text: cssVar('--fg', '#e8b23a'),
    dim: cssVar('--fg-dim', '#8a7440'),
    grid: cssVar('--chart-grid', '#1a1a22'),
    border: cssVar('--border', '#2a2a35'),
    up: cssVar('--up', '#33d17a'),
    down: cssVar('--down', '#f05a5a'),
    accent: cssVar('--accent', '#4aa3ff'),
    accentSoft: cssVar('--accent-soft', 'rgba(74,163,255,0.18)'),
  };
}

/**
 * Colours for implied-price overlays, in assignment order.
 *
 * Read from CSS like the rest of the theme, so `THEME` restyles overlays too.
 * They are deliberately distinct from `--up`/`--down`: an overlay is a separate
 * *series*, not a direction, and colouring it green would read as "rising".
 */
export function overlayPalette(): string[] {
  return [
    cssVar('--overlay-1', '#4aa3ff'),
    cssVar('--overlay-2', '#c678dd'),
    cssVar('--overlay-3', '#e5c07b'),
    cssVar('--overlay-4', '#56b6c2'),
    cssVar('--overlay-5', '#e06c75'),
  ];
}

/**
 * Decimal places appropriate to a price's magnitude.
 *
 * The same axis has to render BTC at 63,000 and EUR/USD at 1.0842. Two decimals
 * on the second one throws away the entire day's range; six on the first is
 * noise. Picking from the magnitude gets both right without a per-instrument
 * table.
 */
export function precisionFor(magnitude: number): number {
  const abs = Math.abs(magnitude);
  if (!Number.isFinite(abs) || abs === 0) return 2;
  if (abs >= 1000) return 2;
  if (abs >= 10) return 2;
  if (abs >= 1) return 4;
  return 6;
}

function priceFormat(precision: number) {
  return { type: 'price' as const, precision, minMove: 10 ** -precision };
}

function baseOptions(theme: TerminalChartTheme): DeepPartial<TimeChartOptions> {
  return {
    layout: {
      background: { type: ColorType.Solid, color: theme.background },
      textColor: theme.dim,
      fontSize: 11,
      fontFamily: `'JetBrains Mono', 'IBM Plex Mono', 'SF Mono', Menlo, Consolas, monospace`,
      attributionLogo: false,
      panes: {
        separatorColor: theme.border,
        separatorHoverColor: theme.accent,
        enableResize: true,
      },
    },
    grid: {
      vertLines: { color: theme.grid, style: LineStyle.Solid },
      horzLines: { color: theme.grid, style: LineStyle.Solid },
    },
    rightPriceScale: {
      borderColor: theme.border,
      // Small, symmetric margins. Volume lives in its own pane rather than
      // squatting on the bottom of this one, so nothing has to be reserved —
      // a large bottom margin here is what makes a 0..1 contract's axis
      // extend into negative cents.
      scaleMargins: { top: 0.1, bottom: 0.08 },
    },
    timeScale: {
      borderColor: theme.border,
      rightOffset: 2,
      barSpacing: 6,
      minBarSpacing: 0.5,
      fixLeftEdge: true,
      lockVisibleTimeRangeOnResize: true,
    },
    crosshair: {
      mode: CrosshairMode.Normal,
      vertLine: {
        color: theme.accent,
        width: 1,
        style: LineStyle.Dashed,
        labelBackgroundColor: theme.accent,
      },
      horzLine: {
        color: theme.accent,
        width: 1,
        style: LineStyle.Dashed,
        labelBackgroundColor: theme.accent,
      },
    },
    handleScale: { axisPressedMouseMove: { time: true, price: false } },
    localization: { locale: 'en-US' },
    autoSize: false,
  };
}

/**
 * Create a chart bound to `container`.
 *
 * `autoSize` is deliberately off: it uses a ResizeObserver that fires during
 * grid re-layout and fights the workspace's own observer. Panels call
 * {@link TerminalChart.resize} from `onResize()` instead, which is
 * deterministic.
 */
export class TerminalChart {
  readonly chart: IChartApi;
  readonly #container: HTMLElement;

  constructor(container: HTMLElement) {
    this.#container = container;
    const theme = chartTheme();
    this.chart = createChart(container, {
      ...baseOptions(theme),
      width: Math.max(container.clientWidth, 120),
      height: Math.max(container.clientHeight, 80),
    });
  }

  /** Price axis formatted in cents — how a prediction market is actually read. */
  addCandles(): ISeriesApi<'Candlestick'> {
    const theme = chartTheme();
    return this.chart.addSeries(CandlestickSeries, {
      upColor: theme.up,
      downColor: theme.down,
      wickUpColor: theme.up,
      wickDownColor: theme.down,
      borderUpColor: theme.up,
      borderDownColor: theme.down,
      priceFormat: centsFormat(),
    });
  }

  addProbabilityArea(): ISeriesApi<'Area'> {
    const theme = chartTheme();
    return this.chart.addSeries(AreaSeries, {
      lineColor: theme.accent,
      topColor: theme.accentSoft,
      bottomColor: 'rgba(0,0,0,0)',
      lineWidth: 2,
      priceFormat: centsFormat(),
    });
  }

  /**
   * Volume, in a dedicated pane below the price.
   *
   * The obvious alternative — an overlay price scale with a large bottom
   * margin on the main scale — makes the price axis extend past its data, which
   * on a 0..1 contract prints negative cents. A separate pane keeps the price
   * axis honest and gives volume a readable scale of its own.
   */
  addVolume(heightPx = 70): ISeriesApi<'Histogram'> {
    const series = this.chart.addSeries(
      HistogramSeries,
      { priceFormat: { type: 'volume' }, color: chartTheme().dim, priceLineVisible: false },
      1,
    );

    const panes = this.chart.panes();
    // Only size the pane if it is small enough to matter; on a short tile,
    // forcing 70px of volume would leave nothing for price.
    const total = this.#container.clientHeight;
    if (panes.length > 1 && total > heightPx * 3) {
      panes[1]?.setHeight(heightPx);
    }
    return series;
  }

  addLine(options: DeepPartial<SeriesOptionsCommon> & { color?: string } = {}): ISeriesApi<'Line'> {
    const theme = chartTheme();
    return this.chart.addSeries(LineSeries, {
      color: theme.accent,
      lineWidth: 2,
      ...options,
    });
  }

  /* ------------------------------------------------------- spot + overlay */

  /** Candles for an underlying, priced in its own currency rather than cents. */
  addSpotCandles(precision: number): ISeriesApi<'Candlestick'> {
    const theme = chartTheme();
    return this.chart.addSeries(CandlestickSeries, {
      upColor: theme.up,
      downColor: theme.down,
      wickUpColor: theme.up,
      wickDownColor: theme.down,
      borderUpColor: theme.up,
      borderDownColor: theme.down,
      priceFormat: priceFormat(precision),
    });
  }

  addSpotLine(precision: number): ISeriesApi<'Line'> {
    return this.chart.addSeries(LineSeries, {
      color: chartTheme().text,
      lineWidth: 2,
      priceFormat: priceFormat(precision),
    });
  }

  /**
   * An implied-price line drawn over the true price.
   *
   * Dashed and thinner than the price on purpose: the two series are in the
   * same units on the same axis, and the reader has to be able to tell at a
   * glance which one is the market's forecast and which one actually happened.
   */
  addImpliedLine(color: string, precision: number): ISeriesApi<'Line'> {
    return this.chart.addSeries(LineSeries, {
      color,
      lineWidth: 2,
      lineStyle: LineStyle.Dashed,
      priceFormat: priceFormat(precision),
      priceLineVisible: false,
      lastValueVisible: true,
      crosshairMarkerVisible: true,
    });
  }

  /** Drop a series without rebuilding the chart — used when an overlay is unticked. */
  removeSeries(series: ISeriesApi<'Line'>): void {
    this.chart.removeSeries(series);
  }

  addFredArea(): ISeriesApi<'Area'> {
    const theme = chartTheme();
    return this.chart.addSeries(AreaSeries, {
      lineColor: theme.accent,
      topColor: theme.accentSoft,
      bottomColor: 'rgba(0,0,0,0)',
      lineWidth: 2,
      priceLineVisible: false,
    });
  }

  /** Re-read the CSS variables and restyle. Called on theme change. */
  restyle(): void {
    this.chart.applyOptions(baseOptions(chartTheme()));
  }

  resize(): void {
    const width = this.#container.clientWidth;
    const height = this.#container.clientHeight;
    if (width > 0 && height > 0) this.chart.resize(width, height);
  }

  fit(): void {
    this.chart.timeScale().fitContent();
  }

  destroy(): void {
    this.chart.remove();
  }
}

function centsFormat() {
  return {
    type: 'custom' as const,
    minMove: 0.0001,
    formatter: (price: number): string => {
      const c = price * 100;
      return Number.isInteger(Math.round(c * 100) / 100) ? `${Math.round(c)}¢` : `${c.toFixed(1)}¢`;
    },
  };
}

/* ------------------------------------------------------------ data shaping */

/**
 * lightweight-charts requires strictly ascending, unique timestamps and throws
 * on anything else. Upstream data is *usually* clean; this makes it always
 * clean, keeping last-wins on a duplicate bucket.
 */
export function dedupeAscending<T extends { time: Time }>(points: T[]): T[] {
  const byTime = new Map<unknown, T>();
  for (const point of points) byTime.set(point.time, point);
  return [...byTime.values()].sort((a, b) => Number(a.time) - Number(b.time));
}

export function toCandleData(
  candles: { time: number; open: number; high: number; low: number; close: number }[],
): CandlestickData<Time>[] {
  return dedupeAscending(
    candles.map((c) => ({
      time: c.time as UTCTimestamp,
      open: c.open,
      high: c.high,
      low: c.low,
      close: c.close,
    })),
  );
}

export function toVolumeData(
  candles: { time: number; volume: number; close: number; open: number }[],
  theme = chartTheme(),
): HistogramData<Time>[] {
  return dedupeAscending(
    candles.map((c) => ({
      time: c.time as UTCTimestamp,
      value: c.volume,
      color: c.close >= c.open ? `${theme.up}55` : `${theme.down}55`,
    })),
  );
}

export function toLineData(candles: { time: number; close: number }[]): LineData<Time>[] {
  return dedupeAscending(
    candles.map((c) => ({ time: c.time as UTCTimestamp, value: c.close })),
  );
}

/**
 * Implied-price points → chart points.
 *
 * A `null` value becomes a whitespace point rather than being dropped. That is
 * the whole reason this is not `toLineData`: a bucket where the ladder went
 * unquoted is a genuine hole in the forecast, and joining across it would draw
 * a straight line the market never implied.
 */
export function toImpliedData(
  points: { time: number; value: number | null }[],
): (LineData<Time> | WhitespaceData<Time>)[] {
  return dedupeAscending(
    points.map((p) =>
      p.value === null
        ? { time: p.time as UTCTimestamp }
        : { time: p.time as UTCTimestamp, value: p.value },
    ),
  );
}

/**
 * FRED observations → chart points.
 *
 * A `null` value becomes a whitespace point (`{ time }` with no `value`), which
 * is how lightweight-charts draws a genuine gap. Dropping the row instead would
 * silently connect across a data outage and misrepresent the series.
 */
export function toFredData(
  observations: { date: string; value: number | null }[],
): (LineData<Time> | WhitespaceData<Time>)[] {
  const points = observations.map((o) =>
    o.value === null ? { time: o.date as Time } : { time: o.date as Time, value: o.value },
  );
  return dedupeAscending(points);
}
