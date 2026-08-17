/**
 * ENT — Kalshi's entertainment book, and RT — the Rotten Tomatoes scores its
 * busiest series settles against.
 *
 * The two belong together: `ENT film` lists what is trading, and the FEED column
 * on each row is a live link into the data that will resolve it. Clicking
 * through from a market to its settlement source is the whole idea.
 */

import type {
  EntEvent,
  EntGenre,
  EntResponse,
  RtScore,
  RtSearchResponse,
  RtTitle,
} from '../../shared/types.js';
import { ent } from '../lib/api.js';
import { append, cell, el, field, row, table } from '../lib/dom.js';
import { compact, countdown, truncate } from '../lib/format.js';
import { Panel, type PanelContext } from './panel.js';
import { bindRow } from './table.js';

/* ------------------------------------------------------------------- ENT */

const GENRE_LABEL: Record<string, string> = {
  all: 'everything',
  music: 'music',
  film: 'film',
  tv: 'television',
  games: 'video games',
  awards: 'awards',
  celeb: 'celebrity',
};

export class EntPanel extends Panel<EntResponse> {
  override readonly kind = 'ENT';

  readonly #genre: EntGenre | 'all';

  constructor(id: string, context: PanelContext, genre: EntGenre | 'all') {
    super(id, context);
    this.#genre = genre;
    this.refreshMs = 60_000;
  }

  static idFor(genre: string): string {
    return `ent:${genre.toLowerCase()}`;
  }

  protected override title(): string {
    return this.#genre.toUpperCase();
  }

  protected override subtitle(): string {
    const data = this.latest;
    if (!data) return '';
    return `${data.events.length} events · ${GENRE_LABEL[data.genre] ?? data.genre}`;
  }

  protected override load(signal: AbortSignal): Promise<EntResponse> {
    return ent.markets(this.#genre, 60, signal);
  }

  protected override render(data: EntResponse): void {
    // Genre chips double as the navigation: the counts say what is worth
    // opening, and clicking one re-runs `ENT <genre>`.
    const chips = el('div', { class: 'result-note' }, [
      el('span', { class: 'dim', text: 'GENRES  ' }),
    ]);
    for (const [genre, count] of Object.entries(data.counts)) {
      if (count === 0) continue;
      const chip = el('span', {
        class: `ent-chip${genre === data.genre ? ' ent-chip-active' : ''}`,
        text: `${genre.toUpperCase()} ${count}`,
      });
      chip.title = `Show ${GENRE_LABEL[genre] ?? genre} markets`;
      chip.addEventListener('click', () => this.context.run(`ENT ${genre}`));
      chips.append(chip);
    }
    this.body.append(chips);

    if (data.events.length === 0) {
      this.body.append(
        el('div', { class: 'panel-empty' }, [
          el('div', { text: `Nothing open in ${GENRE_LABEL[data.genre] ?? data.genre}.` }),
          el('div', {
            class: 'panel-empty-hint',
            text: `Scanned ${data.scanned.toLocaleString()} open events, snapshot ${data.snapshotAgeSeconds}s old.`,
          }),
        ]),
      );
      return;
    }

    const rows = data.events.map((event) => this.#eventRow(event));

    this.body.append(
      table(
        ['EVENT', 'MARKET', 'GENRE', 'MKTS', 'VOL 24H', 'OI', 'CLOSES', 'FEED'],
        rows,
        'ent-table',
      ),
    );
  }

  #eventRow(event: EntEvent): HTMLTableRowElement {
    // The feed cell is its own click target: the row opens the market, the
    // feed opens the data. Both from one line, neither stealing the other.
    const feedCell = cell('');
    if (event.feed) {
      const link = el('span', { class: 'ent-feed', text: event.feed.command.split(' ')[0] ?? '' });
      link.title = `${event.feed.source} — run \`${event.feed.command}\``;
      link.addEventListener('click', (e) => {
        e.stopPropagation();
        this.context.run(event.feed?.command ?? '');
      });
      feedCell.append(link);
    } else {
      feedCell.textContent = '—';
      feedCell.className = 'dim';
    }

    const tr = row([
      cell(event.eventTicker, 'mono strong'),
      cell(truncate(event.title, 46), undefined, 'td', event.title),
      cell(event.genres.join(' '), 'dim'),
      cell(String(event.markets.length), 'num dim'),
      cell(compact(event.volume24h), 'num'),
      cell(compact(event.openInterest), 'num dim'),
      cell(countdown(event.closeTime), 'num dim'),
      feedCell,
    ]);
    bindRow(
      tr,
      `EVT ${event.eventTicker}`,
      (command) => this.context.run(command),
      `${event.title}\nClick to open the ladder for ${event.eventTicker}`,
    );
    return tr;
  }
}

/* -------------------------------------------------------------------- RT */

/** A score bar, so 92 vs 41 reads before the digits do. */
function meter(score: RtScore, label: string): HTMLElement {
  const value = score.score;
  const tone = value === null ? 'flat' : value >= 60 ? 'up' : 'down';

  const bar = el('div', { class: 'rt-meter-track' }, [
    el('div', { class: `rt-meter-fill ${tone}` }),
  ]);
  // Width is the only thing set imperatively; everything else is a class.
  (bar.firstElementChild as HTMLElement).style.width = `${value ?? 0}%`;

  return el('div', { class: 'rt-meter' }, [
    el('div', { class: 'rt-meter-head' }, [
      el('span', { class: 'rt-meter-label', text: label }),
      el('span', {
        class: `rt-meter-score ${tone}`,
        text: value === null ? '--' : `${value}%`,
      }),
    ]),
    bar,
    el('div', { class: 'rt-meter-foot' }, [
      el('span', { class: 'dim', text: score.state }),
      el('span', {
        class: 'dim',
        text: score.reviewCount ? `${score.reviewCount.toLocaleString()} reviews` : '',
      }),
    ]),
  ]);
}

export class RtPanel extends Panel<RtTitle> {
  override readonly kind = 'RT';

  readonly #query: string;

  constructor(id: string, context: PanelContext, query: string) {
    super(id, context);
    this.#query = query;
    // Scores move as reviews land, but not minute to minute.
    this.refreshMs = 10 * 60_000;
  }

  static idFor(query: string): string {
    return `rt:${query.toLowerCase()}`;
  }

  protected override title(): string {
    return this.latest ? truncate(this.latest.title, 40) : this.#query.toUpperCase();
  }

  protected override subtitle(): string {
    const data = this.latest;
    if (!data) return '';
    return [data.year, data.mediaType].filter(Boolean).join(' · ');
  }

  protected override load(signal: AbortSignal): Promise<RtTitle> {
    return ent.rt(this.#query, signal);
  }

  protected override render(data: RtTitle): void {
    this.body.append(
      el('div', { class: 'rt-meters' }, [
        meter(data.critics, 'TOMATOMETER'),
        meter(data.audience, 'POPCORNMETER'),
      ]),
    );

    // A film with no Tomatometer yet is the interesting case, not an error:
    // that is precisely when the Kalshi ladder has something to price.
    if (data.critics.score === null) {
      this.body.append(
        el('div', {
          class: 'result-note dim',
          text: 'No Tomatometer yet — the score has not been issued.',
        }),
      );
    }

    const details = el('div', { class: 'meta-strip' });
    append(
      details,
      data.year ? field('YEAR', data.year) : null,
      field('TYPE', data.mediaType),
      data.critics.averageRating ? field('AVG CRITIC', `${data.critics.averageRating}/10`) : null,
      data.audience.averageRating ? field('AVG AUDIENCE', `${data.audience.averageRating}/5`) : null,
      field('SLUG', data.slug),
    );
    this.body.append(details);

    if (data.synopsis) {
      this.body.append(
        el('details', { class: 'notes' }, [
          el('summary', { text: 'SYNOPSIS' }),
          el('p', { text: data.synopsis }),
        ]),
      );
    }

    // Same footer idiom as the quote panel: the obvious next commands, one click
    // away. `SRCH` finds the Kalshi ladder trading on this very score.
    this.body.append(
      el('div', { class: 'panel-actions' }, [
        this.#action('SRCH', `SRCH ${data.title}`),
        this.#action('SEARCH RT', `RT SEARCH ${this.#query}`),
        this.#action('ENT FILM', 'ENT film'),
      ]),
    );
  }

  #action(label: string, command: string): HTMLElement {
    const button = el('button', { class: 'action', type: 'button', text: label });
    button.addEventListener('click', () => this.context.run(command));
    return button;
  }
}

/** `RT SEARCH <words>` — pick between same-named titles. */
export class RtSearchPanel extends Panel<RtSearchResponse> {
  override readonly kind = 'RT';

  readonly #query: string;

  constructor(id: string, context: PanelContext, query: string) {
    super(id, context);
    this.#query = query;
  }

  static idFor(query: string): string {
    return `rt:search:${query.toLowerCase()}`;
  }

  protected override title(): string {
    return `SEARCH "${this.#query}"`;
  }

  protected override load(signal: AbortSignal): Promise<RtSearchResponse> {
    return ent.rtSearch(this.#query, 20, signal);
  }

  protected override render(data: RtSearchResponse): void {
    if (data.results.length === 0) {
      this.body.append(
        el('div', { class: 'panel-empty', text: `Rotten Tomatoes has nothing for "${data.query}".` }),
      );
      return;
    }

    const rows = data.results.map((result) => {
      const score = result.criticsScore;
      const tone = score === null ? 'dim' : score >= 60 ? 'up' : 'down';
      const tr = row([
        cell(score === null ? '--' : `${score}%`, `num ${tone}`),
        cell(truncate(result.title, 46), 'strong'),
        cell(result.year || '—', 'num dim'),
        cell(result.mediaType, 'dim'),
        cell(result.slug, 'mono dim'),
      ]);
      bindRow(tr, `RT ${result.slug}`, (command) => this.context.run(command), `Open ${result.slug}`);
      return tr;
    });

    this.body.append(table(['SCORE', 'TITLE', 'YEAR', 'TYPE', 'SLUG'], rows));
  }
}
