/**
 * BB — Billboard charts as a ranked table.
 *
 * Reads like a market monitor on purpose: rank is the price, the weekly move is
 * the change, and peak/weeks-on-chart are the fundamentals. Movement is
 * coloured so a week's action is legible at a glance.
 */

import type { BillboardChart, BillboardChartListItem } from '../../shared/types.js';
import { billboard } from '../lib/api.js';
import { append, cell, el, row, table } from '../lib/dom.js';
import { day, move, truncate } from '../lib/format.js';
import { Panel, type PanelContext } from './panel.js';

export interface BillboardPanelOptions {
  chart: string;
  date?: string;
}

export class BillboardPanel extends Panel<BillboardChart> {
  override readonly kind = 'BB';

  readonly #options: BillboardPanelOptions;
  #latest: BillboardChart | undefined;

  constructor(id: string, context: PanelContext, options: BillboardPanelOptions) {
    super(id, context);
    this.#options = options;
    // Charts refresh weekly; an hourly poll is already generous.
    this.refreshMs = 60 * 60_000;
  }

  static idFor(chart: string, date?: string): string {
    return `bb:${chart.toLowerCase()}:${date ?? 'latest'}`;
  }

  protected override title(): string {
    return this.#options.chart.toUpperCase();
  }

  protected override subtitle(): string {
    const chart = this.#latest;
    if (!chart) return '';
    return `${chart.title} · week of ${day(chart.date)}`;
  }

  protected override load(signal: AbortSignal): Promise<BillboardChart> {
    return billboard.chart(this.#options.chart, this.#options.date, signal);
  }

  protected override render(data: BillboardChart): void {
    this.#latest = data;

    const risers = data.entries.filter((e) => (e.move ?? 0) > 0).length;
    const fallers = data.entries.filter((e) => (e.move ?? 0) < 0).length;
    const debuts = data.entries.filter((e) => e.isNew).length;

    this.body.append(
      el('div', { class: 'result-note' }, [
        el('span', { text: `${data.entries.length} entries · week of ${day(data.date)}` }),
        el('span', { class: 'up', text: `  ▲ ${risers}` }),
        el('span', { class: 'down', text: `  ▼ ${fallers}` }),
        el('span', { class: 'dim', text: `  NEW ${debuts}` }),
      ]),
    );

    const rows = data.entries.map((entry) => {
      const moveText = move(entry.move);
      const moveClass = entry.isNew ? 'new' : (entry.move ?? 0) > 0 ? 'up' : (entry.move ?? 0) < 0 ? 'down' : 'dim';

      const artCell = cell('');
      artCell.className = 'bb-art-cell';
      artCell.append(this.#artwork(entry.imageUrl));

      const titleCell = cell('');
      append(
        titleCell,
        el('div', { class: 'bb-title', text: truncate(entry.title, 44) }),
        entry.artist ? el('div', { class: 'bb-artist', text: truncate(entry.artist, 44) }) : null,
      );
      titleCell.title = entry.artist ? `${entry.title} — ${entry.artist}` : entry.title;

      return row([
        cell(String(entry.rank), 'num strong'),
        artCell,
        titleCell,
        cell(moveText, `num ${moveClass}`),
        cell(entry.lastWeek === null ? '—' : String(entry.lastWeek), 'num dim'),
        cell(entry.peak === null ? '—' : String(entry.peak), 'num'),
        cell(entry.weeksOnChart === null ? '—' : String(entry.weeksOnChart), 'num dim'),
      ]);
    });

    this.body.append(table(['#', '', 'TITLE / ARTIST', 'MOVE', 'LW', 'PK', 'WKS'], rows, 'bb-table'));
  }

  /**
   * Artwork, fetched through the server rather than straight from Billboard's
   * CDN — same origin, and it works on networks that cannot reach the CDN.
   * A failure collapses to the empty placeholder instead of leaving a broken
   * image icon in every row.
   */
  #artwork(url: string | null): HTMLElement {
    if (!url) return el('span', { class: 'bb-art bb-art-empty' });

    const img = el('img', {
      class: 'bb-art',
      src: `/api/billboard/art?u=${encodeURIComponent(url)}`,
      alt: '',
      loading: 'lazy',
      decoding: 'async',
    });
    img.addEventListener(
      'error',
      () => img.replaceWith(el('span', { class: 'bb-art bb-art-empty' })),
      { once: true },
    );
    return img;
  }
}

/** `BB CHARTS` — the catalogue of slugs this terminal knows. */
export class BillboardChartsPanel extends Panel<{ charts: BillboardChartListItem[] }> {
  override readonly kind = 'BB';

  static readonly ID = 'bb:charts';

  protected override title(): string {
    return 'CHARTS';
  }

  protected override load(signal: AbortSignal): Promise<{ charts: BillboardChartListItem[] }> {
    return billboard.charts(signal);
  }

  protected override render(data: { charts: BillboardChartListItem[] }): void {
    const rows = data.charts.map((chart) => {
      const tr = row([cell(chart.slug, 'mono strong'), cell(chart.name)]);
      tr.classList.add('clickable');
      tr.title = `Open ${chart.slug}`;
      tr.addEventListener('click', () => this.context.run(`BB ${chart.slug}`));
      return tr;
    });

    this.body.append(
      el('div', {
        class: 'result-note',
        text: 'Any billboard.com chart slug works — these are the shortcuts.',
      }),
      table(['SLUG', 'CHART'], rows),
    );
  }
}

export type { PanelContext };
