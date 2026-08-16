/**
 * GP — the Kalshi price chart.
 *
 * Candlesticks (or an area line) of the YES contract price, volume underneath,
 * and a crosshair legend that reads out the bar under the cursor. Prices are
 * drawn in dollars and labelled in cents, which is how a prediction market is
 * quoted.
 */

import type { CandleInterval, CandlesResponse, Market } from '../../shared/types.js';
import { kalshi } from '../lib/api.js';
import {
  TerminalChart,
  chartTheme,
  toCandleData,
  toLineData,
  toVolumeData,
  type ISeriesApi,
  type MouseEventParams,
} from '../lib/chart.js';
import { cents, compact, countdown, direction, signedCents, truncate } from '../lib/format.js';
import { append, el } from '../lib/dom.js';
import { Panel, type PanelContext } from './panel.js';

export type ChartStyle = 'candle' | 'line';

export interface ChartPanelOptions {
  ticker: string;
  interval: CandleInterval;
  /** Look-back window in seconds. */
  lookbackSeconds: number;
  style: ChartStyle;
}

interface ChartData {
  market: Market | null;
  candles: CandlesResponse;
}

const INTERVAL_LABEL: Record<CandleInterval, string> = { 1: '1m', 60: '1h', 1440: '1d' };

export class ChartPanel extends Panel<ChartData> {
  override readonly kind = 'GP';

  #options: ChartPanelOptions;
  #chart: TerminalChart | undefined;
  #priceSeries: ISeriesApi<'Candlestick'> | ISeriesApi<'Area'> | undefined;
  #legend: HTMLElement | undefined;
  #stats: HTMLElement | undefined;
  #latest: ChartData | undefined;
  /** Set once the user has scrolled, so a refresh does not yank the view back. */
  #userMovedView = false;

  constructor(id: string, context: PanelContext, options: ChartPanelOptions) {
    super(id, context);
    this.#options = options;
    // 1-minute bars move constantly; daily bars do not. Poll accordingly.
    this.refreshMs = options.interval === 1 ? 15_000 : 60_000;
  }

  static idFor(ticker: string): string {
    return `gp:${ticker.toUpperCase()}`;
  }

  protected override title(): string {
    return this.#options.ticker;
  }

  protected override subtitle(): string {
    const { interval, style } = this.#options;
    const market = this.#latest?.market;
    const label = `${INTERVAL_LABEL[interval]} · ${style}`;
    return market ? `${label} · ${truncate(market.title, 60)}` : label;
  }

  protected override async load(signal: AbortSignal): Promise<ChartData> {
    const end = Math.floor(Date.now() / 1000);
    const start = end - this.#options.lookbackSeconds;

    // The quote header is a nicety; a market that 404s should not kill the chart.
    const [candles, market] = await Promise.all([
      kalshi.candles(this.#options.ticker, this.#options.interval, start, end, signal),
      kalshi.market(this.#options.ticker, signal).catch(() => null),
    ]);

    return { candles, market };
  }

  protected override render(data: ChartData): void {
    this.#latest = data;

    const host = el('div', { class: 'chart-host' });
    const legend = el('div', { class: 'chart-legend' });
    const stats = el('div', { class: 'chart-stats' });

    this.#legend = legend;
    this.#stats = stats;

    this.body.append(
      el('div', { class: 'chart-wrap' }, [legend, host]),
      stats,
    );

    if (data.candles.candles.length === 0) {
      host.append(
        el('div', { class: 'panel-empty' }, [
          el('div', { text: 'No candles in this window.' }),
          el('div', {
            class: 'panel-empty-hint',
            text: 'This market may not have traded yet. Try a wider range, e.g. `GP <ticker> 1d 1y`.',
          }),
        ]),
      );
      this.#renderStats(data);
      return;
    }

    // The old chart's canvases belong to the previous body; drop them first.
    this.#chart?.destroy();
    const chart = new TerminalChart(host);
    this.#chart = chart;

    const candles = data.candles.candles;

    if (this.#options.style === 'candle') {
      const series = chart.addCandles();
      series.setData(toCandleData(candles));
      this.#priceSeries = series;
    } else {
      const series = chart.addProbabilityArea();
      series.setData(toLineData(candles));
      this.#priceSeries = series;
    }

    chart.addVolume().setData(toVolumeData(candles, chartTheme()));

    chart.chart.subscribeCrosshairMove((param) => this.#updateLegend(param));
    chart.chart
      .timeScale()
      .subscribeVisibleLogicalRangeChange(() => {
        this.#userMovedView = true;
      });

    if (!this.#userMovedView) chart.fit();
    // The tile may not have its final size until after layout settles.
    requestAnimationFrame(() => chart.resize());

    this.#renderLegendFor(candles.at(-1)!);
    this.#renderStats(data);
  }

  #renderStats(data: ChartData): void {
    const stats = this.#stats;
    if (!stats) return;
    stats.replaceChildren();

    const market = data.market;
    const candles = data.candles.candles;
    const first = candles[0];
    const last = candles.at(-1);

    const windowChange = first && last ? last.close - first.open : null;
    const traded = candles.filter((c) => c.traded).length;

    const items: [string, string, string?][] = [
      ['BID', cents(market?.yesBid ?? null), ''],
      ['ASK', cents(market?.yesAsk ?? null), ''],
      ['LAST', cents(market?.lastPrice ?? last?.close ?? null), ''],
      ['CHG', signedCents(market?.change ?? null), direction(market?.change ?? null)],
      ['WIN', signedCents(windowChange), direction(windowChange)],
      ['VOL', compact(market?.volume ?? null), ''],
      ['24H', compact(market?.volume24h ?? null), ''],
      ['OI', compact(market?.openInterest ?? null), ''],
      ['BARS', `${candles.length} (${traded} traded)`, ''],
      ['CLOSE', countdown(market?.closeTime ?? null), ''],
    ];

    for (const [label, value, tone] of items) {
      stats.append(
        el('span', { class: 'stat' }, [
          el('span', { class: 'stat-label', text: label }),
          el('span', { class: `stat-value ${tone ?? ''}`.trim(), text: value }),
        ]),
      );
    }
  }

  #updateLegend(param: MouseEventParams): void {
    if (!this.#priceSeries || !this.#latest) return;

    // Cursor left the pane — fall back to the most recent bar.
    if (!param.time || param.point === undefined) {
      const last = this.#latest.candles.candles.at(-1);
      if (last) this.#renderLegendFor(last);
      return;
    }

    const candle = this.#latest.candles.candles.find((c) => c.time === Number(param.time));
    if (candle) this.#renderLegendFor(candle);
  }

  #renderLegendFor(candle: {
    time: number;
    open: number;
    high: number;
    low: number;
    close: number;
    volume: number;
    traded: boolean;
  }): void {
    const legend = this.#legend;
    if (!legend) return;

    const change = candle.close - candle.open;
    const stamp = new Date(candle.time * 1000);
    const when =
      this.#options.interval === 1440
        ? stamp.toISOString().slice(0, 10)
        : stamp.toISOString().slice(0, 16).replace('T', ' ');

    legend.replaceChildren();
    append(
      legend,
      el('span', { class: 'legend-time', text: `${when}Z` }),
      el('span', { class: 'legend-pair' }, [
        el('span', { class: 'legend-key', text: 'O' }),
        el('span', { text: cents(candle.open) }),
      ]),
      el('span', { class: 'legend-pair' }, [
        el('span', { class: 'legend-key', text: 'H' }),
        el('span', { text: cents(candle.high) }),
      ]),
      el('span', { class: 'legend-pair' }, [
        el('span', { class: 'legend-key', text: 'L' }),
        el('span', { text: cents(candle.low) }),
      ]),
      el('span', { class: 'legend-pair' }, [
        el('span', { class: 'legend-key', text: 'C' }),
        el('span', { class: direction(change), text: cents(candle.close) }),
      ]),
      el('span', { class: 'legend-pair' }, [
        el('span', { class: 'legend-key', text: 'V' }),
        el('span', { text: compact(candle.volume) }),
      ]),
      // A synthesised bar is not a real print; say so rather than implying a trade.
      candle.traded ? null : el('span', { class: 'legend-flag', text: 'NO TRADE' }),
    );
  }

  /** Re-configure in place — used by `GP` re-issued with different arguments. */
  reconfigure(options: Partial<ChartPanelOptions>): void {
    this.#options = { ...this.#options, ...options };
    this.refreshMs = this.#options.interval === 1 ? 15_000 : 60_000;
    this.#userMovedView = false;
    void this.refresh();
  }

  get options(): ChartPanelOptions {
    return { ...this.#options };
  }

  override onResize(): void {
    this.#chart?.resize();
  }

  override destroy(): void {
    this.#chart?.destroy();
    this.#chart = undefined;
    this.#priceSeries = undefined;
    super.destroy();
  }
}
