/**
 * FRED — economic series chart plus the metadata that makes it readable.
 *
 * A number without its units is noise, so the panel leads with units,
 * frequency, seasonal adjustment and vintage, and shows whether the data was
 * scraped or came from the official API.
 */

import type { FredSearchResponse, FredSeriesResponse } from '../../shared/types.js';
import { fred } from '../lib/api.js';
import { TerminalChart, toFredData, type MouseEventParams } from '../lib/chart.js';
import { cell, el, field, row, table } from '../lib/dom.js';
import { day, direction, metric, truncate } from '../lib/format.js';
import { Panel, type PanelContext } from './panel.js';

export interface FredPanelOptions {
  id: string;
  start?: string;
  end?: string;
}

export class FredPanel extends Panel<FredSeriesResponse> {
  override readonly kind = 'FRED';

  readonly #options: FredPanelOptions;
  #chart: TerminalChart | undefined;
  #legend: HTMLElement | undefined;
  #latest: FredSeriesResponse | undefined;

  constructor(id: string, context: PanelContext, options: FredPanelOptions) {
    super(id, context);
    this.#options = options;
    // Macro data is revised on a release schedule, never intraday.
    this.refreshMs = 15 * 60_000;
  }

  static idFor(seriesId: string): string {
    return `fred:${seriesId.toUpperCase()}`;
  }

  protected override title(): string {
    return this.#options.id.toUpperCase();
  }

  protected override subtitle(): string {
    const series = this.#latest?.series;
    if (!series) return '';
    return truncate(series.title, 70);
  }

  protected override load(signal: AbortSignal): Promise<FredSeriesResponse> {
    return fred.series(this.#options.id, this.#options.start, this.#options.end, signal);
  }

  protected override render(data: FredSeriesResponse): void {
    this.#latest = data;
    const { series, observations } = data;

    const withValues = observations.filter((o) => o.value !== null);
    const last = withValues.at(-1);
    const previous = withValues.at(-2);
    const change = last && previous ? last.value! - previous.value! : null;

    // ---- metadata strip ---------------------------------------------------
    const meta = el('div', { class: 'meta-strip' }, [
      field('UNITS', series.units || '—'),
      field('FREQ', series.frequency || '—'),
      field('ADJ', series.seasonalAdjustment || '—'),
      field('RANGE', `${day(series.observationStart)} → ${day(series.observationEnd)}`),
      field('UPDATED', series.lastUpdated || '—'),
      field('SOURCE', series.source === 'api' ? 'FRED API' : 'scraped'),
    ]);

    const headline = el('div', { class: 'headline' }, [
      el('span', { class: 'headline-value', text: metric(last?.value ?? null) }),
      el('span', { class: 'headline-unit', text: series.unitsShort || series.units || '' }),
      el('span', {
        class: `headline-change ${direction(change)}`,
        text:
          change === null
            ? ''
            : `${change > 0 ? '+' : ''}${metric(change)} vs ${day(previous?.date ?? null)}`,
      }),
      el('span', { class: 'headline-date', text: last ? day(last.date) : '' }),
    ]);

    const legend = el('div', { class: 'chart-legend' });
    const host = el('div', { class: 'chart-host' });
    this.#legend = legend;

    this.body.append(headline, meta, el('div', { class: 'chart-wrap' }, [legend, host]));

    if (observations.length === 0) {
      host.append(el('div', { class: 'panel-empty', text: 'No observations returned.' }));
      return;
    }

    this.#chart?.destroy();
    const chart = new TerminalChart(host);
    this.#chart = chart;

    const series1 = chart.addFredArea();
    series1.setData(toFredData(observations));

    chart.chart.subscribeCrosshairMove((param: MouseEventParams) => {
      if (!param.time) {
        this.#setLegend(last?.date ?? null, last?.value ?? null);
        return;
      }
      const point = observations.find((o) => o.date === String(param.time));
      this.#setLegend(point?.date ?? null, point?.value ?? null);
    });

    chart.fit();
    requestAnimationFrame(() => chart.resize());
    this.#setLegend(last?.date ?? null, last?.value ?? null);

    if (series.notes) {
      this.body.append(
        el('details', { class: 'notes' }, [
          el('summary', { text: 'NOTES' }),
          el('p', { text: series.notes }),
        ]),
      );
    }
  }

  #setLegend(date: string | null, value: number | null): void {
    if (!this.#legend) return;
    this.#legend.replaceChildren(
      el('span', { class: 'legend-time', text: date ? day(date) : '—' }),
      el('span', { class: 'legend-pair' }, [
        el('span', { class: 'legend-key', text: 'VAL' }),
        el('span', { text: metric(value) }),
      ]),
    );
  }

  override onResize(): void {
    this.#chart?.resize();
  }

  override destroy(): void {
    this.#chart?.destroy();
    this.#chart = undefined;
    super.destroy();
  }
}

/* ------------------------------------------------------------ FRED search */

export class FredSearchPanel extends Panel<FredSearchResponse> {
  override readonly kind = 'FSRCH';

  readonly #query: string;

  constructor(id: string, context: PanelContext, query: string) {
    super(id, context);
    this.#query = query;
  }

  static idFor(query: string): string {
    return `fsrch:${query.toLowerCase()}`;
  }

  protected override title(): string {
    return `"${this.#query}"`;
  }

  protected override load(signal: AbortSignal): Promise<FredSearchResponse> {
    return fred.search(this.#query, 40, signal);
  }

  protected override render(data: FredSearchResponse): void {
    if (data.results.length === 0) {
      this.body.append(
        el('div', { class: 'panel-empty', text: `No FRED series match "${data.query}".` }),
      );
      return;
    }

    const rows = data.results.map((result) => {
      const tr = row([
        cell(result.id, 'mono strong'),
        cell(truncate(result.title, 72)),
        cell(result.units ?? '', 'dim'),
        cell(result.frequency ?? '', 'dim'),
      ]);
      tr.classList.add('clickable');
      tr.title = `Open ${result.id}`;
      tr.addEventListener('click', () => this.context.run(`FRED ${result.id}`));
      return tr;
    });

    this.body.append(
      el('div', { class: 'result-note', text: `${data.results.length} series · via ${data.source}` }),
      table(['SERIES', 'TITLE', 'UNITS', 'FREQ'], rows),
    );
  }
}
