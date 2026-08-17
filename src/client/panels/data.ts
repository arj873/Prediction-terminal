/**
 * Reference data — one chart panel for eight publishers, and the boards that
 * help you find an id for it.
 *
 * This began as the FRED panel and is unchanged in what it shows, because what
 * it showed was right: a number without its units, frequency, seasonal
 * adjustment and vintage is noise, so those lead and the chart follows. What
 * changed is that the same panel now renders a BLS series, an ECB SDMX key or a
 * CFTC net position, and names which publisher and which arm of it answered.
 */

import type {
  DataSearchResponse,
  DataSeriesResponse,
  DataSourceStatus,
} from '../../shared/types.js';
import { data } from '../lib/api.js';
import { TerminalChart, toFredData, type MouseEventParams } from '../lib/chart.js';
import { cell, el, field, row, table } from '../lib/dom.js';
import { day, direction, metric, truncate } from '../lib/format.js';
import { Panel, type PanelContext } from './panel.js';

export interface DataSeriesPanelOptions {
  /** `[source:]id`, exactly as typed. */
  ref: string;
  start?: string;
  end?: string;
}

export class DataSeriesPanel extends Panel<DataSeriesResponse> {
  override readonly kind = 'ECO';

  readonly #options: DataSeriesPanelOptions;
  #chart: TerminalChart | undefined;
  #legend: HTMLElement | undefined;

  constructor(id: string, context: PanelContext, options: DataSeriesPanelOptions) {
    super(id, context);
    this.#options = options;
    // Official statistics are revised on a release schedule, never intraday.
    this.refreshMs = 15 * 60_000;
  }

  static idFor(ref: string): string {
    return `eco:${ref.toLowerCase()}`;
  }

  protected override title(): string {
    return this.#options.ref.toUpperCase();
  }

  protected override subtitle(): string {
    const series = this.latest?.series;
    return series ? truncate(series.title, 70) : '';
  }

  protected override load(signal: AbortSignal): Promise<DataSeriesResponse> {
    return data.series(this.#options.ref, this.#options.start, this.#options.end, signal);
  }

  protected override render(payload: DataSeriesResponse): void {
    // Drop the previous chart before any early return — see ChartPanel.
    this.#chart?.destroy();
    this.#chart = undefined;

    const { series, observations } = payload;

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
      // Both halves matter and they answer different questions: who published
      // it, and which arm of them this deployment reached.
      field('SOURCE', `${series.provider.toUpperCase()} · ${series.source}`),
      field('N', String(observations.length)),
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

    const chart = new TerminalChart(host);
    this.#chart = chart;

    const line = chart.addFredArea();
    line.setData(toFredData(observations));

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

    if (series.notes || series.sourceUrl) {
      this.body.append(
        el('details', { class: 'notes' }, [
          el('summary', { text: 'NOTES' }),
          series.notes ? el('p', { text: series.notes }) : null,
          series.sourceUrl
            ? el('p', {}, [
                el('a', {
                  href: series.sourceUrl,
                  target: '_blank',
                  rel: 'noopener noreferrer',
                  text: series.sourceUrl,
                }),
              ])
            : null,
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

/* --------------------------------------------------------------- search */

export class DataSearchPanel extends Panel<DataSearchResponse> {
  override readonly kind = 'ECOS';

  readonly #query: string;
  readonly #sources: readonly string[];

  constructor(id: string, context: PanelContext, query: string, sources: readonly string[]) {
    super(id, context);
    this.#query = query;
    this.#sources = sources;
  }

  static idFor(query: string, sources: readonly string[]): string {
    return `ecos:${[...sources].sort().join('+')}:${query.toLowerCase()}`;
  }

  protected override title(): string {
    return `"${this.#query}"`;
  }

  protected override subtitle(): string {
    return this.#sources.length ? this.#sources.join(', ') : '';
  }

  protected override load(signal: AbortSignal): Promise<DataSearchResponse> {
    return data.search(this.#query, [...this.#sources], 60, signal);
  }

  protected override render(payload: DataSearchResponse): void {
    // A publisher that failed or was skipped is reported whether or not there
    // were hits: "no match" and "no match among the six we could ask" are
    // different answers, and only one of them means the series does not exist.
    const notes: HTMLElement[] = [];
    for (const gap of payload.unavailable) {
      notes.push(
        el('div', { class: 'result-note warn', text: `${gap.provider}: ${gap.error}` }),
      );
    }
    for (const gap of payload.skipped) {
      notes.push(el('div', { class: 'result-note dim', text: `${gap.provider}: ${gap.hint}` }));
    }

    if (payload.results.length === 0) {
      this.body.append(
        el('div', { class: 'panel-empty', text: `No series match "${payload.query}".` }),
        ...notes,
      );
      return;
    }

    const rows = payload.results.map((result) => {
      // The reference is what `ECO` takes, which is not always the bare id:
      // only FRED prints without a prefix.
      const ref = result.provider === 'fred' ? result.id : `${result.provider}:${result.id}`;
      const tr = row([
        cell(result.provider.toUpperCase(), 'dim'),
        cell(result.id, 'mono strong'),
        cell(truncate(result.title, 70)),
        cell(result.units ?? '', 'dim'),
        cell(result.frequency ?? '', 'dim'),
      ]);
      tr.classList.add('clickable');
      tr.title = `Open ${ref}`;
      tr.addEventListener('click', () => this.context.run(`ECO ${ref}`));
      return tr;
    });

    this.body.append(
      el('div', {
        class: 'result-note',
        text: `${payload.results.length} series across ${new Set(payload.results.map((r) => r.provider)).size} publishers`,
      }),
      table(['SRC', 'ID', 'TITLE', 'UNITS', 'FREQ'], rows),
      ...notes,
    );
  }
}

/* --------------------------------------------------------------- sources */

/**
 * What this deployment can actually serve.
 *
 * Worth a panel of its own because half of these publishers behave differently
 * depending on a key that may or may not be set, and the alternative way to
 * find out is to type a command and read an error.
 */
export class SourcesPanel extends Panel<{ sources: DataSourceStatus[] }> {
  override readonly kind = 'SRC';

  static readonly ID = 'src';

  constructor(id: string, context: PanelContext) {
    super(id, context);
  }

  protected override title(): string {
    return 'DATA SOURCES';
  }

  protected override subtitle(): string {
    const sources = this.latest?.sources ?? [];
    if (sources.length === 0) return '';
    return `${sources.filter((s) => s.available).length}/${sources.length} available`;
  }

  protected override load(signal: AbortSignal): Promise<{ sources: DataSourceStatus[] }> {
    return data.sources(signal);
  }

  protected override render(payload: { sources: DataSourceStatus[] }): void {
    const rows = payload.sources.map((source) => {
      const tr = row([
        cell(source.code, 'mono strong'),
        cell(source.label),
        cell(source.kind, 'dim'),
        cell(source.available ? 'YES' : 'NO', source.available ? 'up' : 'down'),
        cell(source.idExample, 'mono dim'),
        cell(truncate(source.covers, 64), 'dim'),
      ]);

      if (source.note) tr.title = source.note;
      if (source.kind === 'series') {
        tr.classList.add('clickable');
        tr.addEventListener('click', () => this.context.run(`ECO ${source.idExample}`));
      }
      return tr;
    });

    const notes = payload.sources
      .filter((s) => s.note)
      .map((s) =>
        el('div', {
          class: `result-note ${s.available ? 'dim' : 'warn'}`,
          text: `${s.code}: ${s.note}`,
        }),
      );

    this.body.append(
      table(['SRC', 'PUBLISHER', 'KIND', 'READY', 'EXAMPLE', 'COVERS'], rows),
      ...notes,
    );
  }
}
