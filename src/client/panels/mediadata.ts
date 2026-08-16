/**
 * The entertainment data feeds: NFLX, SPOT, YT, BO, STEAM and TV.
 *
 * All six read like the Billboard panel on purpose — rank is the price, the
 * period move is the change — because that is the frame a trader already has
 * loaded when they are looking at a market that settles on one of these
 * numbers.
 */

import type {
  BoxOfficeDay,
  NetflixTop10,
  SteamChart,
  StreamChart,
  StreamEntry,
  TvSchedule,
} from '../../shared/types.js';
import { ent } from '../lib/api.js';
import { append, cell, el, row, table } from '../lib/dom.js';
import { compact, day, group, truncate } from '../lib/format.js';
import { Panel, type PanelContext } from './panel.js';

/**
 * Rank movement, with `unknown` kept distinct from `held`.
 *
 * `format.move()` reads a null as a debut, which is right for Billboard but
 * wrong for a chart that simply has no movement column — an all-time table
 * would report every row as NEW.
 */
function rankMove(move: number | null, isNew: boolean): { text: string; tone: string } {
  if (isNew) return { text: 'NEW', tone: 'new' };
  if (move === null) return { text: '—', tone: 'dim' };
  if (move === 0) return { text: '=', tone: 'dim' };
  return { text: move > 0 ? `+${move}` : `${move}`, tone: move > 0 ? 'up' : 'down' };
}

/** `$19,000,000` → `$19.0M`, for money that belongs in a fixed-width column. */
function money(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return '--';
  return `$${compact(value)}`;
}

function signedPercent(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return '--';
  return `${value > 0 ? '+' : ''}${value.toFixed(1)}%`;
}

/* ------------------------------------------------------------------ NFLX */

export interface NetflixPanelOptions {
  category: string;
  scope: string;
}

export class NetflixPanel extends Panel<NetflixTop10> {
  override readonly kind = 'NFLX';

  readonly #options: NetflixPanelOptions;
  #latest: NetflixTop10 | undefined;

  constructor(id: string, context: PanelContext, options: NetflixPanelOptions) {
    super(id, context);
    this.#options = options;
    // Published weekly, on Tuesdays.
    this.refreshMs = 60 * 60_000;
  }

  static idFor(category: string, scope: string): string {
    return `nflx:${category}:${scope}`.toLowerCase();
  }

  protected override title(): string {
    return `${this.#options.category.toUpperCase()} ${this.#options.scope.toUpperCase()}`;
  }

  protected override subtitle(): string {
    const data = this.#latest;
    return data ? `${data.scopeLabel} · week of ${day(data.week)}` : '';
  }

  protected override load(signal: AbortSignal): Promise<NetflixTop10> {
    return ent.netflix(this.#options.category, this.#options.scope, signal);
  }

  protected override render(data: NetflixTop10): void {
    this.#latest = data;

    // Country feeds are rank-only; Netflix publishes views and hours globally.
    const hasViews = data.entries.some((e) => e.views !== null);

    this.body.append(
      el('div', { class: 'result-note' }, [
        el('span', { text: `Netflix Top 10 · ${data.categoryLabel} · ${data.scopeLabel}` }),
        el('span', { class: 'dim', text: `  week of ${day(data.week)}` }),
        hasViews ? null : el('span', { class: 'dim', text: '  · ranks only for this country' }),
      ]),
    );

    const rows = data.entries.map((entry) => {
      const cells = [
        cell(String(entry.rank), 'num strong'),
        cell(truncate(entry.title, 42), undefined, 'td', entry.title),
        cell(truncate(entry.season, 20), 'dim'),
      ];
      if (hasViews) {
        cells.push(
          cell(compact(entry.views), 'num'),
          cell(compact(entry.hoursViewed), 'num dim'),
        );
      }
      cells.push(cell(entry.weeksInTop10 === null ? '—' : String(entry.weeksInTop10), 'num dim'));
      return row(cells);
    });

    const headers = hasViews
      ? ['#', 'TITLE', 'SEASON', 'VIEWS', 'HOURS', 'WKS']
      : ['#', 'TITLE', 'SEASON', 'WKS'];

    this.body.append(table(headers, rows, 'nflx-table'));
  }
}

/* -------------------------------------------------------------- SPOT / YT */

export interface StreamPanelOptions {
  /** `spotify` or `youtube`. */
  source: 'spotify' | 'youtube';
  /** Spotify: country code. YouTube: the view name. */
  scope: string;
  /** Spotify only: `daily` or `weekly`. */
  period: string;
}

export class StreamChartPanel extends Panel<StreamChart> {
  override readonly kind: string;

  readonly #options: StreamPanelOptions;
  #latest: StreamChart | undefined;

  constructor(id: string, context: PanelContext, options: StreamPanelOptions) {
    super(id, context);
    this.#options = options;
    this.kind = options.source === 'spotify' ? 'SPOT' : 'YT';
    this.refreshMs = 30 * 60_000;
  }

  static idFor(options: StreamPanelOptions): string {
    return `${options.source}:${options.scope}:${options.period}`.toLowerCase();
  }

  protected override title(): string {
    return this.#options.source === 'spotify'
      ? `${this.#options.scope.toUpperCase()} ${this.#options.period.toUpperCase()}`
      : this.#options.scope.toUpperCase();
  }

  protected override subtitle(): string {
    return this.#latest ? truncate(this.#latest.title, 52) : '';
  }

  protected override load(signal: AbortSignal): Promise<StreamChart> {
    return this.#options.source === 'spotify'
      ? ent.spotify(this.#options.scope, this.#options.period, 200, signal)
      : ent.youtube(this.#options.scope, 200, signal);
  }

  protected override render(data: StreamChart): void {
    this.#latest = data;

    const risers = data.entries.filter((e) => (e.move ?? 0) > 0).length;
    const fallers = data.entries.filter((e) => (e.move ?? 0) < 0).length;
    const debuts = data.entries.filter((e) => e.isNew).length;

    this.body.append(
      el('div', { class: 'result-note' }, [
        el('span', { text: `${data.entries.length} entries` }),
        el('span', { class: 'up', text: `  ▲ ${risers}` }),
        el('span', { class: 'down', text: `  ▼ ${fallers}` }),
        el('span', { class: 'dim', text: `  NEW ${debuts}` }),
        // Say where the numbers came from: this is a mirror of Spotify and
        // YouTube, not the platforms themselves.
        el('span', { class: 'dim', text: '  · via kworb.net' }),
      ]),
    );

    const hasTotal = data.entries.some((e) => e.total !== null);
    const hasPeak = data.entries.some((e) => e.peak !== null);

    const rows = data.entries.map((entry) => this.#row(entry, hasTotal, hasPeak));

    const headers = ['#', 'MOVE', 'TITLE / ARTIST', this.#options.source === 'youtube' ? 'VIEWS' : 'STREAMS'];
    if (hasPeak) headers.push('PK');
    if (hasTotal) headers.push('TOTAL');

    this.body.append(table(headers, rows, 'stream-table'));
  }

  #row(entry: StreamEntry, hasTotal: boolean, hasPeak: boolean): HTMLTableRowElement {
    const { text, tone } = rankMove(entry.move, entry.isNew);

    const titleCell = cell('');
    append(
      titleCell,
      el('div', { class: 'bb-title', text: truncate(entry.title, 44) }),
      entry.artist ? el('div', { class: 'bb-artist', text: truncate(entry.artist, 44) }) : null,
    );
    titleCell.title = entry.artist ? `${entry.artist} — ${entry.title}` : entry.title;

    // The delta belongs beside the figure it moved, not in its own column.
    const streamsCell = cell('');
    append(
      streamsCell,
      el('div', { class: 'num', text: group(entry.streams) }),
      entry.streamsChange !== null
        ? el('div', {
            class: `num tiny ${entry.streamsChange > 0 ? 'up' : entry.streamsChange < 0 ? 'down' : 'dim'}`,
            text: `${entry.streamsChange > 0 ? '+' : ''}${compact(entry.streamsChange)}`,
          })
        : null,
    );
    streamsCell.className = 'num';

    const cells = [
      cell(String(entry.rank), 'num strong'),
      cell(text, `num ${tone}`),
      titleCell,
      streamsCell,
    ];
    if (hasPeak) cells.push(cell(entry.peak === null ? '—' : String(entry.peak), 'num dim'));
    if (hasTotal) cells.push(cell(compact(entry.total), 'num dim'));

    return row(cells);
  }
}

/* -------------------------------------------------------------------- BO */

export class BoxOfficePanel extends Panel<BoxOfficeDay> {
  override readonly kind = 'BO';

  readonly #date: string | undefined;
  #latest: BoxOfficeDay | undefined;

  constructor(id: string, context: PanelContext, date?: string) {
    super(id, context);
    this.#date = date;
    this.refreshMs = 30 * 60_000;
  }

  static idFor(date?: string): string {
    return `bo:${date ?? 'latest'}`;
  }

  protected override title(): string {
    return this.#latest ? this.#latest.date : (this.#date ?? 'LATEST');
  }

  protected override subtitle(): string {
    const data = this.#latest;
    return data ? `${data.entries.length} releases · ${money(data.totalGross)} total` : '';
  }

  protected override load(signal: AbortSignal): Promise<BoxOfficeDay> {
    return ent.boxOffice(this.#date, signal);
  }

  protected override render(data: BoxOfficeDay): void {
    this.#latest = data;

    this.body.append(
      el('div', { class: 'result-note' }, [
        el('span', { text: data.title }),
        el('span', { class: 'dim', text: `  · ${money(data.totalGross)} across ${data.entries.length}` }),
      ]),
    );

    const rows = data.entries.map((entry) => {
      const { text, tone } = rankMove(entry.move, entry.isNew);
      const tr = row([
        cell(String(entry.rank), 'num strong'),
        cell(text, `num ${tone}`),
        cell(truncate(entry.title, 40), undefined, 'td', entry.title),
        cell(money(entry.gross), 'num'),
        cell(signedPercent(entry.changeDay), `num ${entry.changeDay === null ? 'dim' : entry.changeDay > 0 ? 'up' : 'down'}`),
        cell(signedPercent(entry.changeWeek), `num ${entry.changeWeek === null ? 'dim' : entry.changeWeek > 0 ? 'up' : 'down'}`),
        cell(group(entry.theaters), 'num dim'),
        cell(money(entry.totalGross), 'num'),
        cell(entry.daysInRelease === null ? '—' : String(entry.daysInRelease), 'num dim'),
        cell(truncate(entry.distributor, 22), 'dim'),
      ]);
      // The Rotten Tomatoes score is the natural next question about a release.
      tr.classList.add('clickable');
      tr.title = `${entry.title}\nClick for the Rotten Tomatoes score`;
      tr.addEventListener('click', () => this.context.run(`RT ${entry.title}`));
      return tr;
    });

    this.body.append(
      table(['#', 'MOVE', 'RELEASE', 'DAILY', '%YD', '%LW', 'THTRS', 'TO DATE', 'DAYS', 'STUDIO'], rows, 'bo-table'),
    );
  }
}

/* ----------------------------------------------------------------- STEAM */

export class SteamPanel extends Panel<SteamChart> {
  override readonly kind = 'STEAM';

  readonly #query: string;
  #latest: SteamChart | undefined;

  constructor(id: string, context: PanelContext, query: string) {
    super(id, context);
    this.#query = query;
    // Concurrent players are a live figure — this is the one feed worth polling.
    this.refreshMs = 120_000;
  }

  static idFor(query: string): string {
    return `steam:${query.toLowerCase() || 'top'}`;
  }

  protected override title(): string {
    return this.#query ? truncate(this.#query.toUpperCase(), 30) : 'TOP';
  }

  protected override subtitle(): string {
    const data = this.#latest;
    if (!data) return '';
    if (data.view === 'game') return data.games[0]?.name ?? '';
    const total = data.games.reduce((sum, g) => sum + (g.currentPlayers ?? 0), 0);
    return `${compact(total)} players in the top ${data.games.length}`;
  }

  protected override load(signal: AbortSignal): Promise<SteamChart> {
    return ent.steam(this.#query || undefined, 25, signal);
  }

  protected override render(data: SteamChart): void {
    this.#latest = data;

    this.body.append(
      el('div', {
        class: 'result-note',
        text:
          data.view === 'game'
            ? 'Live concurrent players, from Valve’s own API.'
            : 'Most-played on Steam right now.',
      }),
    );

    const rows = data.games.map((game) => {
      const tr = row([
        cell(game.rank === null ? '—' : String(game.rank), 'num strong'),
        cell(truncate(game.name, 44), undefined, 'td', game.name),
        cell(group(game.currentPlayers), 'num'),
        cell(group(game.peakPlayers), 'num dim'),
        cell(game.appId ? String(game.appId) : '—', 'mono dim'),
      ]);
      if (game.appId) {
        tr.classList.add('clickable');
        tr.title = `Live player count for ${game.name}`;
        tr.addEventListener('click', () => this.context.run(`STEAM ${game.appId}`));
      }
      return tr;
    });

    this.body.append(table(['#', 'GAME', 'PLAYERS', 'PEAK 24H', 'APPID'], rows, 'steam-table'));
  }
}

/* -------------------------------------------------------------------- TV */

export interface TvPanelOptions {
  date?: string;
  country?: string;
}

export class TvPanel extends Panel<TvSchedule> {
  override readonly kind = 'TV';

  readonly #options: TvPanelOptions;
  #latest: TvSchedule | undefined;

  constructor(id: string, context: PanelContext, options: TvPanelOptions) {
    super(id, context);
    this.#options = options;
    this.refreshMs = 30 * 60_000;
  }

  static idFor(options: TvPanelOptions): string {
    return `tv:${(options.country ?? 'us').toLowerCase()}:${options.date ?? 'today'}`;
  }

  protected override title(): string {
    const data = this.#latest;
    return data ? `${data.country} ${data.date}` : (this.#options.date ?? 'TODAY');
  }

  protected override subtitle(): string {
    return this.#latest ? `${this.#latest.episodes.length} airings` : '';
  }

  protected override load(signal: AbortSignal): Promise<TvSchedule> {
    return ent.tv(this.#options.date, this.#options.country, signal);
  }

  protected override render(data: TvSchedule): void {
    this.#latest = data;

    if (data.episodes.length === 0) {
      this.body.append(
        el('div', { class: 'panel-empty' }, [
          el('div', { text: `Nothing scheduled for ${data.country} on ${day(data.date)}.` }),
          el('div', { class: 'panel-empty-hint', text: 'Try another date: `TV 2026-08-20`.' }),
        ]),
      );
      return;
    }

    this.body.append(
      el('div', {
        class: 'result-note',
        text: `${data.episodes.length} airings · ${data.country} · ${day(data.date)} · via TVmaze`,
      }),
    );

    const rows = data.episodes.map((episode) => {
      const number =
        episode.season !== null && episode.episode !== null
          ? `S${String(episode.season).padStart(2, '0')}E${String(episode.episode).padStart(2, '0')}`
          : '—';

      const tr = row([
        cell(episode.airtime || '—', 'num'),
        cell(truncate(episode.show, 34), 'strong', 'td', episode.show),
        cell(truncate(episode.network, 18), 'dim'),
        cell(number, 'mono dim'),
        cell(truncate(episode.name, 34), undefined, 'td', episode.name),
        cell(episode.runtime === null ? '—' : `${episode.runtime}m`, 'num dim'),
      ]);
      // A show is the natural search term for the market that trades on it.
      tr.classList.add('clickable');
      tr.title = `Search Kalshi for "${episode.show}"`;
      tr.addEventListener('click', () => this.context.run(`SRCH ${episode.show}`));
      return tr;
    });

    this.body.append(table(['TIME', 'SHOW', 'NETWORK', 'EP', 'EPISODE', 'RUN'], rows, 'tv-table'));
  }
}
