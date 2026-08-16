/**
 * BB — Billboard charts as a ranked table.
 *
 * Reads like a market monitor on purpose: rank is the price, the weekly move is
 * the change, and peak/weeks-on-chart are the fundamentals. Movement is
 * coloured so a week's action is legible at a glance.
 */

import type { BillboardChart, BillboardChartListItem } from '../../shared/types.js';
import { billboard } from '../lib/api.js';
import { el } from '../lib/dom.js';
import { day, rankMove, truncate } from '../lib/format.js';
import { type PanelContext } from './panel.js';
import { TablePanel, movementNote, stackedCell, type TableSpec } from './table.js';

export interface BillboardPanelOptions {
  chart: string;
  date?: string;
}

type BillboardRow = BillboardChart['entries'][number];

/**
 * Artwork, fetched through the server rather than straight from Billboard's
 * CDN — same origin, and it works on networks that cannot reach the CDN.
 * A failure collapses to the empty placeholder instead of leaving a broken
 * image icon in every row.
 */
function artwork(url: string | null): HTMLElement {
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

export class BillboardPanel extends TablePanel<BillboardChart, BillboardRow> {
  override readonly kind = 'BB';

  readonly #options: BillboardPanelOptions;

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
    const chart = this.latest;
    return chart ? `${chart.title} · week of ${day(chart.date)}` : '';
  }

  protected override load(signal: AbortSignal): Promise<BillboardChart> {
    return billboard.chart(this.#options.chart, this.#options.date, signal);
  }

  protected override spec(): TableSpec<BillboardChart, BillboardRow> {
    return {
      rows: (data) => data.entries,
      tableClass: 'bb-table',
      note: (data) => [
        { text: `${data.entries.length} entries · week of ${day(data.date)}` },
        ...movementNote(data.entries).slice(1),
      ],
      columns: [
        { header: '#', cell: (entry) => String(entry.rank), class: 'num strong' },
        { header: '', cell: (entry) => artwork(entry.imageUrl), class: 'bb-art-cell' },
        {
          header: 'TITLE / ARTIST',
          cell: (entry) =>
            stackedCell(
              truncate(entry.title, 44),
              entry.artist ? truncate(entry.artist, 44) : null,
              { title: entry.artist ? `${entry.title} — ${entry.artist}` : entry.title },
            ),
        },
        {
          header: 'MOVE',
          cell: (entry) => rankMove(entry.move, entry.isNew).text,
          class: (entry) => `num ${rankMove(entry.move, entry.isNew).tone}`,
        },
        {
          header: 'LW',
          cell: (entry) => (entry.lastWeek === null ? '—' : String(entry.lastWeek)),
          class: 'num dim',
        },
        {
          header: 'PK',
          cell: (entry) => (entry.peak === null ? '—' : String(entry.peak)),
          class: 'num',
        },
        {
          header: 'WKS',
          cell: (entry) => (entry.weeksOnChart === null ? '—' : String(entry.weeksOnChart)),
          class: 'num dim',
        },
      ],
    };
  }
}

/** `BB CHARTS` — the catalogue of slugs this terminal knows. */
export class BillboardChartsPanel extends TablePanel<
  { charts: BillboardChartListItem[] },
  BillboardChartListItem
> {
  override readonly kind = 'BB';

  static readonly ID = 'bb:charts';

  protected override title(): string {
    return 'CHARTS';
  }

  protected override load(signal: AbortSignal): Promise<{ charts: BillboardChartListItem[] }> {
    return billboard.charts(signal);
  }

  protected override spec(): TableSpec<
    { charts: BillboardChartListItem[] },
    BillboardChartListItem
  > {
    return {
      rows: (data) => data.charts,
      note: () => 'Any billboard.com chart slug works — these are the shortcuts.',
      rowCommand: (chart) => `BB ${chart.slug}`,
      rowTitle: (chart) => `Open ${chart.slug}`,
      columns: [
        { header: 'SLUG', cell: (chart) => chart.slug, class: 'mono strong' },
        { header: 'CHART', cell: (chart) => chart.name },
      ],
    };
  }
}

export type { PanelContext };
