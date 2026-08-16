/**
 * STK / CRY / IMP — the underlying price chart, with implied-price overlays.
 *
 * The true price is drawn as candles or a line; on top of it go dashed lines,
 * one per selected Kalshi ladder, showing what that ladder implied the price
 * would be — computed at every historical bucket, not just now. Reading the two
 * together is the point of the panel: the gap between them is the market's
 * forward basis, and watching it close as expiry approaches is the whole story
 * a prediction market tells about a price.
 *
 * Which ladders are drawn is the reader's choice. The picker under the chart
 * lists every Kalshi expiry that prices this symbol with its live implied
 * value, and clicking one toggles its overlay — the same code path the
 * `IMP <symbol> <event>…` command takes, so mouse and keyboard agree.
 */

import type {
  AssetClass,
  CandleInterval,
  ImpliedCandidate,
  ImpliedMethod,
  ImpliedSeriesResponse,
  SpotCandle,
  SpotCandlesResponse,
  SpotQuote,
} from '../../shared/types.js';
import { ApiRequestError, implied as impliedApi, spot as spotApi } from '../lib/api.js';
import {
  TerminalChart,
  chartTheme,
  overlayPalette,
  precisionFor,
  toCandleData,
  toImpliedData,
  toLineData,
  toVolumeData,
  type ISeriesApi,
  type MouseEventParams,
} from '../lib/chart.js';
import {
  compact,
  direction,
  price as priceText,
  signedPercent,
  signedPrice,
  stamp,
  truncate,
} from '../lib/format.js';
import { append, el } from '../lib/dom.js';
import { Panel, type PanelContext } from './panel.js';

export type SpotStyle = 'candle' | 'line';

export interface SpotPanelOptions {
  symbol: string;
  assetClass: AssetClass;
  interval: CandleInterval;
  /** Look-back window in seconds. */
  lookbackSeconds: number;
  style: SpotStyle;
  /** Kalshi event tickers currently overlaid, in selection order. */
  overlays: string[];
  method: ImpliedMethod;
  /** Open with the picker expanded — how `IMP` differs from `STK`/`CRY`. */
  showPicker: boolean;
}

interface SpotData {
  quote: SpotQuote | null;
  candles: SpotCandlesResponse;
  candidates: ImpliedCandidate[];
  /** Why no ladders are offered, when that is the interesting answer. */
  candidatesNote: string | null;
  overlays: ImpliedSeriesResponse[];
  /** Overlays that were requested but failed, so the picker can say so. */
  failed: { eventTicker: string; message: string }[];
}

const INTERVAL_LABEL: Record<CandleInterval, string> = { 1: '1m', 60: '1h', 1440: '1d' };

export class SpotPanel extends Panel<SpotData> {
  /** Derived, not stored: `CRY BTC` then `STK BTC` re-labels the same panel. */
  override get kind(): string {
    return this.#options.assetClass === 'crypto' ? 'CRY' : 'STK';
  }

  #options: SpotPanelOptions;
  #chart: TerminalChart | undefined;
  #priceSeries: ISeriesApi<'Candlestick'> | ISeriesApi<'Line'> | undefined;
  #overlaySeries: { eventTicker: string; series: ISeriesApi<'Line'>; color: string }[] = [];
  #legend: HTMLElement | undefined;
  #stats: HTMLElement | undefined;
  #latest: SpotData | undefined;
  #userMovedView = false;

  constructor(id: string, context: PanelContext, options: SpotPanelOptions) {
    super(id, context);
    this.#options = options;
    this.refreshMs = options.interval === 1 ? 15_000 : 60_000;
  }

  static idFor(symbol: string): string {
    return `spot:${symbol.toUpperCase()}`;
  }

  protected override title(): string {
    return this.#options.symbol;
  }

  protected override subtitle(): string {
    const { interval, style, overlays } = this.#options;
    const parts = [`${INTERVAL_LABEL[interval]} · ${style}`];
    const name = this.#latest?.quote?.name;
    if (name && name !== this.#options.symbol) parts.push(truncate(name, 44));
    if (overlays.length > 0) {
      parts.push(`${overlays.length} implied · ${this.#options.method}`);
    }
    return parts.join(' · ');
  }

  protected override async load(signal: AbortSignal): Promise<SpotData> {
    const { symbol, assetClass, interval, lookbackSeconds, method, overlays } = this.#options;
    const end = Math.floor(Date.now() / 1000);
    const start = end - lookbackSeconds;

    // The chart is the panel; a failed quote or an unmapped symbol must not
    // take it down, so only the candles are allowed to reject.
    const [candles, quote, candidates] = await Promise.all([
      spotApi.candles(assetClass, symbol, interval, start, end, signal),
      spotApi.quote(assetClass, symbol, signal).catch(() => null),
      impliedApi
        .candidates(symbol, signal)
        .then((response) => ({
          candidates: response.candidates,
          note: response.note ?? null,
        }))
        .catch((err: unknown) => {
          if ((err as Error)?.name === 'AbortError') throw err;
          return {
            candidates: [] as ImpliedCandidate[],
            note: err instanceof ApiRequestError ? (err.hint ?? err.message) : String(err),
          };
        }),
    ]);

    const settled = await Promise.all(
      overlays.map((eventTicker) =>
        impliedApi
          .series(eventTicker, interval, start, end, method, signal)
          .then((series) => ({ ok: true as const, series }))
          .catch((err: unknown) => {
            if ((err as Error)?.name === 'AbortError') throw err;
            return {
              ok: false as const,
              eventTicker,
              message: err instanceof Error ? err.message : String(err),
            };
          }),
      ),
    );

    return {
      quote,
      candles,
      candidates: candidates.candidates,
      candidatesNote: candidates.note,
      overlays: settled.filter((s) => s.ok).map((s) => s.series),
      failed: settled
        .filter((s) => !s.ok)
        .map((s) => ({ eventTicker: s.eventTicker, message: s.message })),
    };
  }

  protected override render(data: SpotData): void {
    this.#latest = data;
    this.#overlaySeries = [];

    const host = el('div', { class: 'chart-host' });
    const legend = el('div', { class: 'chart-legend' });
    const picker = el('div', { class: 'implied-picker' });
    const stats = el('div', { class: 'chart-stats' });

    this.#legend = legend;
    this.#stats = stats;

    // The picker earns its row when there is something to pick, or when the
    // reader asked for implied prices by name — `IMP AAPL` deserves an answer
    // about why there is no overlay, while a plain `STK AAPL` does not need a
    // line of chrome telling it so. It also stays while anything is selected:
    // a transient failure to list candidates must not strand a drawn overlay
    // with no chip left to untick it.
    const wantsPicker =
      data.candidates.length > 0 ||
      this.#options.overlays.length > 0 ||
      this.#options.showPicker;
    this.body.append(
      el('div', { class: 'chart-wrap' }, [legend, host]),
      ...(wantsPicker ? [picker] : []),
      stats,
    );

    if (wantsPicker) this.#renderPicker(picker, data);
    this.#renderStats(data);

    if (data.candles.candles.length === 0) {
      host.append(
        el('div', { class: 'panel-empty' }, [
          el('div', { text: 'No price history in this window.' }),
          el('div', {
            class: 'panel-empty-hint',
            text:
              this.#options.interval === 1
                ? 'Intraday history is short-lived upstream. Try `1h` or `1d`.'
                : 'Try a wider range, e.g. `1d 1y`.',
          }),
        ]),
      );
      return;
    }

    this.#chart?.destroy();
    const chart = new TerminalChart(host);
    this.#chart = chart;

    const candles = data.candles.candles;
    const precision = precisionFor(candles.at(-1)?.close ?? 1);

    if (this.#options.style === 'candle') {
      const series = chart.addSpotCandles(precision);
      series.setData(toCandleData(candles));
      this.#priceSeries = series;
    } else {
      const series = chart.addSpotLine(precision);
      series.setData(toLineData(candles));
      this.#priceSeries = series;
    }

    chart.addVolume().setData(toVolumeData(candles, chartTheme()));

    const palette = overlayPalette();
    data.overlays.forEach((overlay, index) => {
      const color = palette[index % palette.length]!;
      const series = chart.addImpliedLine(color, precision);
      series.setData(toImpliedData(overlay.points));
      this.#overlaySeries.push({ eventTicker: overlay.eventTicker, series, color });
    });

    chart.chart.subscribeCrosshairMove((param) => this.#updateLegend(param));

    if (!this.#userMovedView) chart.fit();

    // Subscribe to range changes only *after* our own fit has settled.
    // `subscribeVisibleLogicalRangeChange` fires for programmatic moves as well
    // as user ones, so subscribing first makes the panel believe the reader
    // scrolled the moment it drew itself — and then never re-fits again. That
    // is invisible while the data extent is stable and very visible here, where
    // a ladder that opened yesterday and a price series that stopped at Friday's
    // close barely overlap: whichever was drawn first filled the viewport and
    // the other sat off-screen.
    requestAnimationFrame(() => {
      chart.resize();
      requestAnimationFrame(() => {
        chart.chart.timeScale().subscribeVisibleLogicalRangeChange(() => {
          this.#userMovedView = true;
        });
      });
    });

    this.#renderLegendFor(candles.at(-1)!, new Map());
  }

  /* -------------------------------------------------------------- picker */

  /**
   * The ladder picker.
   *
   * Every candidate is one Kalshi expiry with its live implied price, so the
   * choice between four expiries is made on what they actually say rather than
   * on their tickers. A ticked chip carries the colour of its line, which is
   * the only thing tying a dashed line on the chart to a contract ticker.
   */
  #renderPicker(host: HTMLElement, data: SpotData): void {
    const label = el('span', { class: 'picker-label', text: 'IMPLIED' });
    const offered = pickerRows(data, this.#options.overlays);

    if (offered.length === 0) {
      host.append(
        label,
        el('span', {
          class: 'picker-empty',
          text: data.candidatesNote ?? `No Kalshi ladder prices ${this.#options.symbol}.`,
        }),
      );
      return;
    }

    const palette = overlayPalette();
    const chips = el('div', { class: 'picker-chips' });

    offered.forEach((candidate) => {
      const index = this.#options.overlays.indexOf(candidate.eventTicker);
      const selected = index >= 0;
      const color = selected ? palette[index % palette.length]! : '';
      const failure = data.failed.find((f) => f.eventTicker === candidate.eventTicker);

      const chip = el('button', {
        class: `picker-chip${selected ? ' on' : ''}${failure ? ' failed' : ''}`,
        type: 'button',
        title: chipTooltip(candidate, failure?.message),
      });
      if (selected && color) chip.style.setProperty('--chip-color', color);

      append(
        chip,
        el('span', { class: 'chip-swatch' }),
        el('span', { class: 'chip-ticker', text: shortLabel(candidate) }),
        el('span', {
          class: 'chip-implied',
          text: candidate.implied === null ? '--' : priceText(candidate.implied),
        }),
      );

      chip.addEventListener('click', () => this.#toggleOverlay(candidate.eventTicker));
      chips.append(chip);
    });

    host.append(label, chips);

    if (data.failed.length > 0) {
      host.append(
        el('span', {
          class: 'picker-error',
          text: `${data.failed.length} overlay${data.failed.length === 1 ? '' : 's'} failed to load`,
        }),
      );
    }

    // A ladder that loaded cleanly but has no history yet is the confusing
    // case: nothing is drawn and nothing is wrong. It happens constantly with
    // same-day expiries, which open hours before they settle and so have no
    // completed hourly bucket at all. Say so, and name the interval that would
    // have data.
    const empty = data.overlays.filter((overlay) => lastValueOf(overlay) === null);
    if (empty.length > 0) {
      const finer = this.#options.interval === 1440 ? '1h' : '1m';
      host.append(
        el('span', {
          class: 'picker-note',
          text:
            `${empty.map((o) => shortEventLabel(o.eventTicker)).join(', ')}: ` +
            `no ${INTERVAL_LABEL[this.#options.interval]} history yet — try ${finer}`,
        }),
      );
    }
  }

  /** Toggle a ladder on or off, then reload. Also the `IMP` command's path. */
  #toggleOverlay(eventTicker: string): void {
    const adding = !this.#options.overlays.includes(eventTicker);
    const overlays = adding
      ? [...this.#options.overlays, eventTicker]
      : this.#options.overlays.filter((t) => t !== eventTicker);

    this.#options = { ...this.#options, overlays };
    // Adding a ladder can bring in bars outside the current view — a contract
    // that opened yesterday against a year of price history, or the reverse —
    // so re-fit to show what was just asked for. Removing one only takes data
    // away, and there the reader's place is worth keeping.
    if (adding) this.#userMovedView = false;
    void this.refresh();
  }

  /* --------------------------------------------------------------- stats */

  #renderStats(data: SpotData): void {
    const stats = this.#stats;
    if (!stats) return;
    stats.replaceChildren();

    const quote = data.quote;
    const candles = data.candles.candles;
    const last = candles.at(-1);
    const first = candles[0];

    const lastPrice = quote?.price ?? last?.close ?? null;
    const windowChange = first && last ? last.close - first.open : null;
    const windowPercent =
      first && last && first.open !== 0 ? ((last.close - first.open) / first.open) * 100 : null;

    // Every price-shaped figure is formatted against `lastPrice`, so a change,
    // a basis and the price itself all carry the same number of decimals.
    const scale = lastPrice ?? undefined;

    const items: [string, string, string?][] = [
      ['LAST', priceText(lastPrice), ''],
      ['CHG', signedPrice(quote?.change ?? null, scale), direction(quote?.change ?? null)],
      ['%', signedPercent(quote?.changePercent ?? null), direction(quote?.changePercent ?? null)],
      ['OPEN', priceText(quote?.dayOpen ?? null, scale), ''],
      ['HIGH', priceText(quote?.dayHigh ?? null, scale), ''],
      ['LOW', priceText(quote?.dayLow ?? null, scale), ''],
      ['VOL', compact(quote?.volume ?? null), ''],
      ['WIN', signedPrice(windowChange, scale), direction(windowChange)],
      ['WIN%', signedPercent(windowPercent), direction(windowPercent)],
      ['BARS', String(candles.length), ''],
    ];

    // One entry per overlay: what it implies now, and the basis against spot.
    // The basis is the number the whole panel exists to show.
    for (const overlay of data.overlays) {
      const value = lastValueOf(overlay);
      const basis = value !== null && lastPrice !== null ? value - lastPrice : null;
      items.push([
        `IMP ${shortEventLabel(overlay.eventTicker)}`,
        value === null ? '--' : `${priceText(value, scale)} (${signedPrice(basis, scale)})`,
        direction(basis),
      ]);
    }

    for (const [label, value, tone] of items) {
      stats.append(
        el('span', { class: 'stat' }, [
          el('span', { class: 'stat-label', text: label }),
          el('span', { class: `stat-value ${tone ?? ''}`.trim(), text: value }),
        ]),
      );
    }

    if (data.candles.source) {
      stats.append(
        el('span', { class: 'stat' }, [
          el('span', { class: 'stat-label', text: 'SRC' }),
          el('span', { class: 'stat-value dim', text: data.candles.source }),
        ]),
      );
    }
  }

  /* -------------------------------------------------------------- legend */

  #updateLegend(param: MouseEventParams): void {
    if (!this.#priceSeries || !this.#latest) return;

    const impliedNow = new Map<string, number>();
    for (const overlay of this.#overlaySeries) {
      const point = param.seriesData.get(overlay.series) as { value?: number } | undefined;
      if (point?.value !== undefined) impliedNow.set(overlay.eventTicker, point.value);
    }

    if (!param.time || param.point === undefined) {
      const last = this.#latest.candles.candles.at(-1);
      if (last) this.#renderLegendFor(last, new Map());
      return;
    }

    const candle = this.#latest.candles.candles.find((c) => c.time === Number(param.time));
    if (candle) this.#renderLegendFor(candle, impliedNow);
  }

  #renderLegendFor(candle: SpotCandle, impliedNow: Map<string, number>): void {
    const legend = this.#legend;
    if (!legend) return;

    const change = candle.close - candle.open;
    const at = new Date(candle.time * 1000);
    const when =
      this.#options.interval === 1440
        ? at.toISOString().slice(0, 10)
        : at.toISOString().slice(0, 16).replace('T', ' ');

    const scale = candle.close;

    legend.replaceChildren();
    append(
      legend,
      el('span', { class: 'legend-time', text: `${when}Z` }),
      legendPair('O', priceText(candle.open, scale)),
      legendPair('H', priceText(candle.high, scale)),
      legendPair('L', priceText(candle.low, scale)),
      legendPair('C', priceText(candle.close, scale), direction(change)),
      legendPair('V', compact(candle.volume)),
    );

    // Each overlay's value at the cursor, and its basis against the bar's
    // close — the same colour as its line so the pairing is unambiguous.
    for (const overlay of this.#overlaySeries) {
      const value = impliedNow.get(overlay.eventTicker);
      if (value === undefined) continue;
      const basis = value - candle.close;
      const pair = el('span', { class: 'legend-pair implied' }, [
        el('span', { class: 'legend-key', text: shortEventLabel(overlay.eventTicker) }),
        el('span', { text: priceText(value, scale) }),
        el('span', {
          class: `legend-basis ${direction(basis)}`,
          text: `(${signedPrice(basis, scale)})`,
        }),
      ]);
      pair.style.setProperty('--chip-color', overlay.color);
      legend.append(pair);
    }
  }

  /* ------------------------------------------------------------ lifecycle */

  /** Re-configure in place — used when `STK`/`CRY`/`IMP` is re-issued. */
  reconfigure(options: Partial<SpotPanelOptions>): void {
    const before = this.#options;
    this.#options = { ...before, ...options };
    this.refreshMs = this.#options.interval === 1 ? 15_000 : 60_000;
    // A different window or interval is a different picture; a different set of
    // overlays is the same picture with another line on it.
    if (
      options.interval !== undefined ||
      options.lookbackSeconds !== undefined ||
      options.style !== undefined
    ) {
      this.#userMovedView = false;
    }
    void this.refresh();
  }

  get options(): SpotPanelOptions {
    return { ...this.#options, overlays: [...this.#options.overlays] };
  }

  override onResize(): void {
    this.#chart?.resize();
  }

  override destroy(): void {
    this.#chart?.destroy();
    this.#chart = undefined;
    this.#priceSeries = undefined;
    this.#overlaySeries = [];
    super.destroy();
  }
}

/* ----------------------------------------------------------------- helpers */

function legendPair(key: string, value: string, tone = ''): HTMLElement {
  return el('span', { class: 'legend-pair' }, [
    el('span', { class: 'legend-key', text: key }),
    el('span', { class: tone, text: value }),
  ]);
}

/**
 * The chips to offer: every candidate, plus any selected ladder the candidate
 * list has stopped mentioning.
 *
 * A ladder can drop off the list while its overlay is drawn — it expires, or a
 * discovery call fails transiently. Rendering only the current candidates would
 * leave that line on the chart with no chip left to turn it off, so a stub is
 * synthesised from what the loaded series already told us.
 */
function pickerRows(data: SpotData, overlays: string[]): ImpliedCandidate[] {
  const rows = [...data.candidates];
  const known = new Set(rows.map((c) => c.eventTicker));

  for (const eventTicker of overlays) {
    if (known.has(eventTicker)) continue;
    const loaded = data.overlays.find((o) => o.eventTicker === eventTicker);
    rows.push({
      eventTicker,
      seriesTicker: '',
      title: loaded?.title ?? eventTicker,
      subTitle: '',
      strikeDate: loaded?.strikeDate ?? '',
      strikes: loaded?.contributors.length ?? 0,
      quoted: loaded?.contributors.length ?? 0,
      volume24h: 0,
      strikeLow: null,
      strikeHigh: null,
      implied: loaded ? lastValueOf(loaded) : null,
      tailMass: null,
    });
  }

  return rows;
}

/** The last bucket where the ladder actually produced a price. */
function lastValueOf(series: ImpliedSeriesResponse): number | null {
  for (let i = series.points.length - 1; i >= 0; i--) {
    const value = series.points[i]!.value;
    if (value !== null) return value;
  }
  return null;
}

/**
 * `KXBTCD-26AUG1617` → `BTCD·26AUG1617`.
 *
 * The expiry alone is not enough. Kalshi lists two ladders over the same
 * underlying and the same instant — an above/below ladder (`KXBTCD`) and a
 * mutually-exclusive range ladder (`KXBTC`) — and they imply slightly different
 * prices. Labelling both `26AUG1617` would put two different lines on the chart
 * under one name. The `KX` prefix carries no information and is dropped.
 */
function shortEventLabel(eventTicker: string): string {
  const dash = eventTicker.indexOf('-');
  if (dash === -1) return eventTicker;
  const series = eventTicker.slice(0, dash).replace(/^KX/, '');
  return `${series}·${eventTicker.slice(dash + 1)}`;
}

function shortLabel(candidate: ImpliedCandidate): string {
  return shortEventLabel(candidate.eventTicker);
}

function chipTooltip(candidate: ImpliedCandidate, failure?: string): string {
  const lines = [
    candidate.title,
    `Event    ${candidate.eventTicker}`,
    `Settles  ${candidate.strikeDate ? stamp(candidate.strikeDate) : 'unknown'}`,
    `Strikes  ${candidate.quoted} quoted of ${candidate.strikes}` +
      (candidate.strikeLow !== null && candidate.strikeHigh !== null
        ? ` (${priceText(candidate.strikeLow)} – ${priceText(candidate.strikeHigh)})`
        : ''),
    `Implied  ${candidate.implied === null ? 'not derivable from the current book' : priceText(candidate.implied)}`,
    `Tail     ${candidate.tailMass === null ? '--' : `${(candidate.tailMass * 100).toFixed(1)}% of probability outside the strikes`}`,
    `24h vol  ${compact(candidate.volume24h)}`,
  ];
  if (failure) lines.push('', `Overlay failed: ${failure}`);
  return lines.join('\n');
}

