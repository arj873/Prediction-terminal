/**
 * The four feeds added for the entertainment book's uncovered half: AWRD, TRND,
 * REL and POD.
 *
 * Each one is the public evidence behind a cluster of Kalshi markets that
 * previously had no companion command — awards ceremonies, Google search
 * rankings, release-date markets and the podcast book. They render like the rest
 * of the data panels: rank as the price, one row per contender, and every row
 * clickable through to whatever the natural next question is.
 */

import type {
  AwardEntry,
  AwardResult,
  PodcastChart,
  Release,
  ReleaseList,
  TrendList,
} from '../../shared/types.js';
import { ent } from '../lib/api.js';
import { append, cell, el, row, table } from '../lib/dom.js';
import { compact, day, truncate } from '../lib/format.js';
import { Panel, type PanelContext } from './panel.js';

/* ------------------------------------------------------------------ AWRD */

export interface AwardPanelOptions {
  award: string;
  year?: number;
}

export class AwardPanel extends Panel<AwardResult> {
  override readonly kind = 'AWRD';

  readonly #options: AwardPanelOptions;
  #latest: AwardResult | undefined;

  constructor(id: string, context: PanelContext, options: AwardPanelOptions) {
    super(id, context);
    this.#options = options;
    // A ceremony is one night a year. Polling this hard would be theatre.
    this.refreshMs = 30 * 60_000;
  }

  static idFor(options: AwardPanelOptions): string {
    return `awrd:${options.award}:${options.year ?? 'all'}`.toLowerCase();
  }

  protected override title(): string {
    return truncate(this.#options.award.toUpperCase(), 34);
  }

  protected override subtitle(): string {
    const data = this.#latest;
    if (!data) return '';
    return data.year === null ? data.award : `${data.award} · ${data.year}`;
  }

  protected override load(signal: AbortSignal): Promise<AwardResult> {
    return ent.awards(this.#options.award, this.#options.year, signal);
  }

  protected override render(data: AwardResult): void {
    this.#latest = data;

    const winners = data.entries.filter((e) => e.won).length;

    this.body.append(
      el('div', { class: 'result-note' }, [
        el('span', { text: truncate(data.award, 52) }),
        el('span', { class: 'dim', text: `  ${data.entries.length} records` }),
        winners > 0 ? el('span', { class: 'up', text: `  ${winners} won` }) : null,
        // Wikidata is a community database, not the Academy. Say so, the same
        // way the streaming panels name kworb rather than passing it off as
        // Spotify's own numbers.
        el('span', { class: 'dim', text: '  · via Wikidata' }),
      ]),
    );

    // An unannounced ceremony is the state these markets exist to price, so it
    // gets the body rather than a footnote — and it is not an error.
    if (data.entries.length === 0) {
      this.body.append(
        el('div', { class: 'panel-empty' }, [
          el('div', { text: data.note ?? 'No records for this award.' }),
          el('div', {
            class: 'panel-empty-hint',
            text:
              data.years.length > 0
                ? `Ceremonies on record: ${data.years.slice(0, 12).join(', ')}`
                : 'Try `AWRD` with no argument for the list of awards.',
          }),
        ]),
      );
      return;
    }

    if (data.note) this.body.append(el('div', { class: 'result-note dim', text: data.note }));

    // Nominee lists lag the winner by days to weeks, and a slate that looks
    // complete but is not is the failure mode worth naming outright.
    if (data.year !== null) {
      this.body.append(
        el('div', {
          class: 'result-note dim',
          text:
            'Winners are recorded within minutes; losing nominees are filled in over ' +
            'the following days. Treat the field as a floor, not a full ballot.',
        }),
      );
    }

    const rows = data.entries.map((entry) => this.#row(entry, data.year === null));
    const headers = data.year === null
      ? ['YEAR', 'RESULT', 'RECIPIENT', 'FOR']
      : ['RESULT', 'RECIPIENT', 'FOR'];

    this.body.append(table(headers, rows, 'awrd-table'));
  }

  #row(entry: AwardEntry, showYear: boolean): HTMLTableRowElement {
    const cells = showYear
      ? [cell(entry.year === null ? '—' : String(entry.year), 'num dim')]
      : [];

    cells.push(
      cell(entry.won ? 'WON' : 'NOM', entry.won ? 'strong up' : 'dim'),
      cell(truncate(entry.name, 40), entry.won ? 'strong' : undefined, 'td', entry.name),
      cell(truncate(entry.work, 34), 'dim', 'td', entry.work),
    );

    const tr = row(cells, entry.won ? 'awrd-won' : undefined);
    // A contender's name is the search term for the market trading on it.
    tr.classList.add('clickable');
    tr.title = `Search Kalshi for "${entry.name}"`;
    tr.addEventListener('click', () => this.context.run(`SRCH ${entry.name}`));
    return tr;
  }
}

/** `AWRD` with no argument: what this terminal knows how to look up. */
export class AwardListPanel extends Panel<{ awards: { key: string; label: string }[] }> {
  override readonly kind = 'AWRD';
  static readonly ID = 'awrd:list';

  protected override title(): string {
    return 'AWARDS';
  }

  protected override load(signal: AbortSignal): Promise<{ awards: { key: string; label: string }[] }> {
    return ent.awardList(signal);
  }

  protected override render(data: { awards: { key: string; label: string }[] }): void {
    this.body.append(
      el('div', {
        class: 'result-note',
        text: 'Award categories Kalshi trades. Any Wikidata award name also works.',
      }),
    );

    const rows = data.awards.map((award) => {
      const tr = row([
        cell(award.key, 'mono strong'),
        cell(award.label, 'dim'),
      ]);
      tr.classList.add('clickable');
      tr.title = `AWRD ${award.key}`;
      tr.addEventListener('click', () => this.context.run(`AWRD ${award.key}`));
      return tr;
    });

    this.body.append(table(['NAME', 'AWARD'], rows, 'awrd-list-table'));
  }
}

/* ------------------------------------------------------------------ TRND */

export class TrendsPanel extends Panel<TrendList> {
  override readonly kind = 'TRND';

  readonly #geo: string;
  #latest: TrendList | undefined;

  constructor(id: string, context: PanelContext, geo: string) {
    super(id, context);
    this.#geo = geo;
    this.refreshMs = 10 * 60_000;
  }

  static idFor(geo: string): string {
    return `trnd:${geo.toLowerCase()}`;
  }

  protected override title(): string {
    return this.#geo.toUpperCase();
  }

  protected override subtitle(): string {
    return this.#latest ? `${this.#latest.geoLabel} · ${this.#latest.entries.length} trending` : '';
  }

  protected override load(signal: AbortSignal): Promise<TrendList> {
    return ent.trends(this.#geo, 25, signal);
  }

  protected override render(data: TrendList): void {
    this.#latest = data;

    this.body.append(
      el('div', { class: 'result-note' }, [
        el('span', { text: `Google Trends · ${data.geoLabel}` }),
        // The markets settle on Google's December "Year in Search" list. This
        // is today's list, which is evidence for it and not the same thing.
        el('span', {
          class: 'dim',
          text: '  · searches trending now, not the annual Year in Search ranking',
        }),
      ]),
    );

    const rows = data.entries.map((entry) => {
      const titleCell = cell('');
      append(
        titleCell,
        el('div', { class: 'bb-title', text: truncate(entry.query, 40) }),
        entry.headline
          ? el('div', {
              class: 'bb-artist',
              text: truncate(
                entry.headlineSource ? `${entry.headlineSource} — ${entry.headline}` : entry.headline,
                52,
              ),
            })
          : null,
      );
      titleCell.title = entry.headline ? `${entry.query}\n${entry.headline}` : entry.query;

      const tr = row([
        cell(String(entry.rank), 'num strong'),
        titleCell,
        // Google states a floor, so the `+` is part of the number's meaning.
        cell(entry.trafficFloor === null ? '--' : `${compact(entry.trafficFloor)}+`, 'num'),
        cell(entry.articles === 0 ? '—' : String(entry.articles), 'num dim'),
      ]);

      tr.classList.add('clickable');
      tr.title = `Search Kalshi for "${entry.query}"`;
      tr.addEventListener('click', () => this.context.run(`SRCH ${entry.query}`));
      return tr;
    });

    this.body.append(table(['#', 'SEARCH / WHY', 'SEARCHES', 'ARTS'], rows, 'trnd-table'));
  }
}

/* ------------------------------------------------------------------- REL */

export interface ReleasePanelOptions {
  query: string;
  kind: string;
}

export class ReleasePanel extends Panel<ReleaseList> {
  override readonly kind = 'REL';

  readonly #options: ReleasePanelOptions;
  #latest: ReleaseList | undefined;

  constructor(id: string, context: PanelContext, options: ReleasePanelOptions) {
    super(id, context);
    this.#options = options;
    this.refreshMs = 60 * 60_000;
  }

  static idFor(options: ReleasePanelOptions): string {
    return `rel:${options.kind}:${options.query}`.toLowerCase();
  }

  protected override title(): string {
    return truncate(this.#options.query.toUpperCase(), 30);
  }

  protected override subtitle(): string {
    const data = this.#latest;
    if (!data) return '';
    const ahead = data.releases.filter((r) => r.upcoming).length;
    return ahead > 0 ? `${data.kindLabel} · ${ahead} upcoming` : data.kindLabel;
  }

  protected override load(signal: AbortSignal): Promise<ReleaseList> {
    return ent.releases(this.#options.query, this.#options.kind, 25, signal);
  }

  protected override render(data: ReleaseList): void {
    this.#latest = data;

    const upcoming = data.releases.filter((r) => r.upcoming);

    this.body.append(
      el('div', { class: 'result-note' }, [
        el('span', { text: `${data.kindLabel} · ${data.releases.length}` }),
        upcoming.length > 0
          ? el('span', { class: 'up', text: `  ${upcoming.length} announced ahead` })
          : el('span', { class: 'dim', text: '  nothing announced ahead' }),
        el('span', { class: 'dim', text: '  · via Apple' }),
      ]),
    );

    const rows = data.releases.map((release) => this.#row(release));
    this.body.append(table(['DATE', 'TITLE', 'ARTIST', 'TRK', 'GENRE'], rows, 'rel-table'));
  }

  #row(release: Release): HTMLTableRowElement {
    const dateCell = cell(release.date, release.upcoming ? 'num strong up' : 'num dim');
    if (release.upcoming) dateCell.title = 'Announced, not yet released';

    const tr = row(
      [
        dateCell,
        cell(truncate(release.title, 38), release.upcoming ? 'strong' : undefined, 'td', release.title),
        cell(truncate(release.artist, 24), 'dim', 'td', release.artist),
        cell(release.trackCount === null ? '—' : String(release.trackCount), 'num dim'),
        cell(truncate(release.genre, 16), 'dim'),
      ],
      release.upcoming ? 'rel-upcoming' : undefined,
    );

    tr.classList.add('clickable');
    tr.title = `${release.title}\nSearch Kalshi for this release`;
    tr.addEventListener('click', () => this.context.run(`SRCH ${release.title}`));
    return tr;
  }
}

/* ------------------------------------------------------------------- POD */

export interface PodcastPanelOptions {
  view: string;
  country: string;
}

export class PodcastPanel extends Panel<PodcastChart> {
  override readonly kind = 'POD';

  readonly #options: PodcastPanelOptions;
  #latest: PodcastChart | undefined;

  constructor(id: string, context: PanelContext, options: PodcastPanelOptions) {
    super(id, context);
    this.#options = options;
    this.refreshMs = 60 * 60_000;
  }

  static idFor(options: PodcastPanelOptions): string {
    return `pod:${options.view}:${options.country}`.toLowerCase();
  }

  protected override title(): string {
    return `${this.#options.view.toUpperCase()} ${this.#options.country.toUpperCase()}`;
  }

  protected override subtitle(): string {
    const data = this.#latest;
    return data ? `${data.viewLabel} · ${data.country}` : '';
  }

  protected override load(signal: AbortSignal): Promise<PodcastChart> {
    return ent.podcasts(this.#options.view, this.#options.country, 50, signal);
  }

  protected override render(data: PodcastChart): void {
    this.#latest = data;

    const isEpisodes = data.view === 'episodes';
    const explicit = data.entries.filter((e) => e.explicit).length;

    // Apple dates the show chart and not the episode chart, which is the
    // opposite way round from what the columns suggest — so the column is driven
    // by what the rows carry rather than by which chart this is.
    const hasDates = data.entries.some((e) => e.released !== '');

    this.body.append(
      el('div', { class: 'result-note' }, [
        el('span', { text: `${data.viewLabel} · ${data.country}` }),
        el('span', { class: 'dim', text: `  ${data.entries.length} entries` }),
        explicit > 0 ? el('span', { class: 'dim', text: `  ${explicit} explicit` }) : null,
        el('span', { class: 'dim', text: '  · via Apple' }),
      ]),
    );

    const rows = data.entries.map((entry) => {
      const cells = [
        cell(String(entry.rank), 'num strong'),
        cell(truncate(entry.name, 44), 'strong', 'td', entry.name),
        cell(truncate(entry.publisher, 26), 'dim', 'td', entry.publisher),
        cell(truncate(entry.genre, 18), 'dim'),
        cell(entry.explicit ? 'E' : '', 'dim'),
      ];
      if (hasDates) cells.push(cell(entry.released ? day(entry.released) : '—', 'num dim'));

      const tr = row(cells);
      tr.classList.add('clickable');
      // On the episode chart the *show* is the tradable entity, not the episode
      // title — a guest market is about the podcast, not about one instalment.
      const term = isEpisodes && entry.publisher ? entry.publisher : entry.name;
      tr.title = `Search Kalshi for "${term}"`;
      tr.addEventListener('click', () => this.context.run(`SRCH ${term}`));
      return tr;
    });

    const headers = [
      '#',
      isEpisodes ? 'EPISODE' : 'SHOW',
      isEpisodes ? 'SHOW' : 'PUBLISHER',
      'GENRE',
      'E',
    ];
    if (hasDates) headers.push('UPDATED');

    this.body.append(table(headers, rows, 'pod-table'));
  }
}
