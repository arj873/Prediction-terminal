/**
 * The entertainment data feeds: NFLX, SPOT, YT, BO, STEAM and TV.
 *
 * All six read like the Billboard panel on purpose — rank is the price, the
 * period move is the change — because that is the frame a trader already has
 * loaded when they are looking at a market that settles on one of these
 * numbers. Reading alike is now enforced rather than intended: each is a list
 * of columns over the shared table panel, so the parts they have in common
 * cannot drift apart again.
 */

import type {
  BoxOfficeDay,
  NetflixTop10,
  SteamChart,
  StreamChart,
  StreamEntry,
  TvEpisode,
  TvSchedule,
} from '../../shared/types.js';
import { ent } from '../lib/api.js';
import { el } from '../lib/dom.js';
import { compact, day, group, money, rankMove, signedPercent, truncate } from '../lib/format.js';
import { type PanelContext } from './panel.js';
import { TablePanel, movementNote, stackedCell, type Column, type TableSpec } from './table.js';

/** The move column, shared by every ranked feed here. */
function moveColumn<R extends { move: number | null; isNew: boolean }>(): Column<R> {
  return {
    header: 'MOVE',
    cell: (entry) => rankMove(entry.move, entry.isNew).text,
    class: (entry) => `num ${rankMove(entry.move, entry.isNew).tone}`,
  };
}

/** The tone for a signed figure, where a null is neither up nor down. */
function signedTone(value: number | null): string {
  return `num ${value === null ? 'dim' : value > 0 ? 'up' : 'down'}`;
}

/** `null` prints as an em dash, for the count columns that are often absent. */
function countOrDash(value: number | null): string {
  return value === null ? '—' : String(value);
}

/* ------------------------------------------------------------------ NFLX */

export interface NetflixPanelOptions {
  category: string;
  scope: string;
}

type NetflixEntry = NetflixTop10['entries'][number];

export class NetflixPanel extends TablePanel<NetflixTop10, NetflixEntry> {
  override readonly kind = 'NFLX';

  readonly #options: NetflixPanelOptions;

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
    const data = this.latest;
    return data ? `${data.scopeLabel} · week of ${day(data.week)}` : '';
  }

  protected override load(signal: AbortSignal): Promise<NetflixTop10> {
    return ent.netflix(this.#options.category, this.#options.scope, signal);
  }

  protected override spec(): TableSpec<NetflixTop10, NetflixEntry> {
    // Country feeds are rank-only; Netflix publishes views and hours globally.
    const hasViews = (rows: readonly NetflixEntry[]): boolean =>
      rows.some((entry) => entry.views !== null);

    return {
      rows: (data) => data.entries,
      tableClass: 'nflx-table',
      note: (data) => [
        { text: `Netflix Top 10 · ${data.categoryLabel} · ${data.scopeLabel}` },
        { text: `  week of ${day(data.week)}`, tone: 'dim' },
        ...(hasViews(data.entries)
          ? []
          : [{ text: '  · ranks only for this country', tone: 'dim' }]),
      ],
      columns: [
        { header: '#', cell: (entry) => String(entry.rank), class: 'num strong' },
        {
          header: 'TITLE',
          cell: (entry) => truncate(entry.title, 42),
          title: (entry) => entry.title,
        },
        { header: 'SEASON', cell: (entry) => truncate(entry.season, 20), class: 'dim' },
        { header: 'VIEWS', cell: (entry) => compact(entry.views), class: 'num', when: hasViews },
        {
          header: 'HOURS',
          cell: (entry) => compact(entry.hoursViewed),
          class: 'num dim',
          when: hasViews,
        },
        { header: 'WKS', cell: (entry) => countOrDash(entry.weeksInTop10), class: 'num dim' },
      ],
    };
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

export class StreamChartPanel extends TablePanel<StreamChart, StreamEntry> {
  override readonly kind: string;

  readonly #options: StreamPanelOptions;

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
    const data = this.latest;
    return data ? truncate(data.title, 52) : '';
  }

  protected override load(signal: AbortSignal): Promise<StreamChart> {
    return this.#options.source === 'spotify'
      ? ent.spotify(this.#options.scope, this.#options.period, 200, signal)
      : ent.youtube(this.#options.scope, 200, signal);
  }

  protected override spec(): TableSpec<StreamChart, StreamEntry> {
    return {
      rows: (data) => data.entries,
      tableClass: 'stream-table',
      note: (data) => [
        ...movementNote(data.entries),
        // Say where the numbers came from: this is a mirror of Spotify and
        // YouTube, not the platforms themselves.
        { text: '  · via kworb.net', tone: 'dim' },
      ],
      columns: [
        { header: '#', cell: (entry) => String(entry.rank), class: 'num strong' },
        moveColumn<StreamEntry>(),
        {
          header: 'TITLE / ARTIST',
          cell: (entry) =>
            stackedCell(
              truncate(entry.title, 44),
              entry.artist ? truncate(entry.artist, 44) : null,
              { title: entry.artist ? `${entry.artist} — ${entry.title}` : entry.title },
            ),
        },
        {
          header: this.#options.source === 'youtube' ? 'VIEWS' : 'STREAMS',
          class: 'num',
          // The delta belongs beside the figure it moved, not in its own column.
          cell: (entry) =>
            el('div', {}, [
              el('div', { class: 'num', text: group(entry.streams) }),
              entry.streamsChange === null
                ? null
                : el('div', {
                    class: `num tiny ${
                      entry.streamsChange > 0 ? 'up' : entry.streamsChange < 0 ? 'down' : 'dim'
                    }`,
                    text: `${entry.streamsChange > 0 ? '+' : ''}${compact(entry.streamsChange)}`,
                  }),
            ]),
        },
        {
          header: 'PK',
          cell: (entry) => countOrDash(entry.peak),
          class: 'num dim',
          when: (rows) => rows.some((entry) => entry.peak !== null),
        },
        {
          header: 'TOTAL',
          cell: (entry) => compact(entry.total),
          class: 'num dim',
          when: (rows) => rows.some((entry) => entry.total !== null),
        },
      ],
    };
  }
}

/* -------------------------------------------------------------------- BO */

type BoxOfficeEntry = BoxOfficeDay['entries'][number];

export class BoxOfficePanel extends TablePanel<BoxOfficeDay, BoxOfficeEntry> {
  override readonly kind = 'BO';

  readonly #date: string | undefined;

  constructor(id: string, context: PanelContext, date?: string) {
    super(id, context);
    this.#date = date;
    this.refreshMs = 30 * 60_000;
  }

  static idFor(date?: string): string {
    return `bo:${date ?? 'latest'}`;
  }

  protected override title(): string {
    return this.latest ? this.latest.date : (this.#date ?? 'LATEST');
  }

  protected override subtitle(): string {
    const data = this.latest;
    return data ? `${data.entries.length} releases · ${money(data.totalGross)} total` : '';
  }

  protected override load(signal: AbortSignal): Promise<BoxOfficeDay> {
    return ent.boxOffice(this.#date, signal);
  }

  protected override spec(): TableSpec<BoxOfficeDay, BoxOfficeEntry> {
    return {
      rows: (data) => data.entries,
      tableClass: 'bo-table',
      note: (data) => [
        { text: data.title },
        { text: `  · ${money(data.totalGross)} across ${data.entries.length}`, tone: 'dim' },
      ],
      // The Rotten Tomatoes score is the natural next question about a release.
      rowCommand: (entry) => `RT ${entry.title}`,
      rowTitle: (entry) => `${entry.title}\nClick for the Rotten Tomatoes score`,
      columns: [
        { header: '#', cell: (entry) => String(entry.rank), class: 'num strong' },
        moveColumn<BoxOfficeEntry>(),
        {
          header: 'RELEASE',
          cell: (entry) => truncate(entry.title, 40),
          title: (entry) => entry.title,
        },
        { header: 'DAILY', cell: (entry) => money(entry.gross), class: 'num' },
        {
          header: '%YD',
          cell: (entry) => signedPercent(entry.changeDay, 1),
          class: (entry) => signedTone(entry.changeDay),
        },
        {
          header: '%LW',
          cell: (entry) => signedPercent(entry.changeWeek, 1),
          class: (entry) => signedTone(entry.changeWeek),
        },
        { header: 'THTRS', cell: (entry) => group(entry.theaters), class: 'num dim' },
        { header: 'TO DATE', cell: (entry) => money(entry.totalGross), class: 'num' },
        { header: 'DAYS', cell: (entry) => countOrDash(entry.daysInRelease), class: 'num dim' },
        { header: 'STUDIO', cell: (entry) => truncate(entry.distributor, 22), class: 'dim' },
      ],
    };
  }
}

/* ----------------------------------------------------------------- STEAM */

type SteamGameRow = SteamChart['games'][number];

export class SteamPanel extends TablePanel<SteamChart, SteamGameRow> {
  override readonly kind = 'STEAM';

  readonly #query: string;

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
    const data = this.latest;
    if (!data) return '';
    if (data.view === 'game') return data.games[0]?.name ?? '';
    const total = data.games.reduce((sum, g) => sum + (g.currentPlayers ?? 0), 0);
    return `${compact(total)} players in the top ${data.games.length}`;
  }

  protected override load(signal: AbortSignal): Promise<SteamChart> {
    return ent.steam(this.#query || undefined, 25, signal);
  }

  protected override spec(): TableSpec<SteamChart, SteamGameRow> {
    return {
      rows: (data) => data.games,
      tableClass: 'steam-table',
      note: (data) =>
        data.view === 'game'
          ? 'Live concurrent players, from Valve’s own API.'
          : 'Most-played on Steam right now.',
      // Only a game the chart named an app id for can be drilled into.
      rowCommand: (game) => (game.appId ? `STEAM ${game.appId}` : null),
      rowTitle: (game) => `Live player count for ${game.name}`,
      columns: [
        { header: '#', cell: (game) => countOrDash(game.rank), class: 'num strong' },
        { header: 'GAME', cell: (game) => truncate(game.name, 44), title: (game) => game.name },
        { header: 'PLAYERS', cell: (game) => group(game.currentPlayers), class: 'num' },
        { header: 'PEAK 24H', cell: (game) => group(game.peakPlayers), class: 'num dim' },
        {
          header: 'APPID',
          cell: (game) => (game.appId ? String(game.appId) : '—'),
          class: 'mono dim',
        },
      ],
    };
  }
}

/* -------------------------------------------------------------------- TV */

export interface TvPanelOptions {
  date?: string;
  country?: string;
}

export class TvPanel extends TablePanel<TvSchedule, TvEpisode> {
  override readonly kind = 'TV';

  readonly #options: TvPanelOptions;

  constructor(id: string, context: PanelContext, options: TvPanelOptions) {
    super(id, context);
    this.#options = options;
    this.refreshMs = 30 * 60_000;
  }

  static idFor(options: TvPanelOptions): string {
    return `tv:${(options.country ?? 'us').toLowerCase()}:${options.date ?? 'today'}`;
  }

  protected override title(): string {
    const data = this.latest;
    return data ? `${data.country} ${data.date}` : (this.#options.date ?? 'TODAY');
  }

  protected override subtitle(): string {
    const data = this.latest;
    return data ? `${data.episodes.length} airings` : '';
  }

  protected override load(signal: AbortSignal): Promise<TvSchedule> {
    return ent.tv(this.#options.date, this.#options.country, signal);
  }

  protected override spec(): TableSpec<TvSchedule, TvEpisode> {
    return {
      rows: (data) => data.episodes,
      tableClass: 'tv-table',
      note: (data) =>
        data.episodes.length === 0
          ? null
          : `${data.episodes.length} airings · ${data.country} · ${day(data.date)} · via TVmaze`,
      empty: (data) => ({
        message: `Nothing scheduled for ${data.country} on ${day(data.date)}.`,
        hint: 'Try another date: `TV 2026-08-20`.',
      }),
      // A show is the natural search term for the market that trades on it.
      rowCommand: (episode) => `SRCH ${episode.show}`,
      rowTitle: (episode) => `Search Kalshi for "${episode.show}"`,
      columns: [
        { header: 'TIME', cell: (episode) => episode.airtime || '—', class: 'num' },
        {
          header: 'SHOW',
          cell: (episode) => truncate(episode.show, 34),
          class: 'strong',
          title: (episode) => episode.show,
        },
        { header: 'NETWORK', cell: (episode) => truncate(episode.network, 18), class: 'dim' },
        {
          header: 'EP',
          class: 'mono dim',
          cell: (episode) =>
            episode.season !== null && episode.episode !== null
              ? `S${String(episode.season).padStart(2, '0')}E${String(episode.episode).padStart(2, '0')}`
              : '—',
        },
        {
          header: 'EPISODE',
          cell: (episode) => truncate(episode.name, 34),
          title: (episode) => episode.name,
        },
        {
          header: 'RUN',
          cell: (episode) => (episode.runtime === null ? '—' : `${episode.runtime}m`),
          class: 'num dim',
        },
      ],
    };
  }
}
