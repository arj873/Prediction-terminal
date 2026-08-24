/**
 * The command grammar: how an argument list becomes a panel's props.
 *
 * Split out of the command table itself (`commands.ts`) rather than sharing a
 * file with it. The table names the panel *kinds* it opens and nothing else, so
 * it can be read — by `HELP`, by autocomplete, by the key map's sanity check —
 * without dragging the grammar along; and the grammar can be tested against
 * fixed argument lists with no command context to build. Imports run one way,
 * `commands.ts` → here, so the two never form the cycle the old registry had
 * with the help panel.
 *
 * Three rules run through everything below and are worth stating once:
 *
 *   *Order-independence.* After the first argument, arguments are claimed by
 *   shape rather than position — `GP X 1d 1y line` and `GP X line 1y 1d` are one
 *   chart, because nobody remembers argument order under time pressure.
 *
 *   *Precedence is list order.* Where two shapes overlap, the earlier claim
 *   wins, and several of those orderings are load-bearing: `1d` is a bar size
 *   before it is a one-day window, `30` is a headline count before it is a
 *   ticker, `tv` names a category before it names Tuvalu.
 *
 *   *Case belongs to the venue.* A reference is read by `parseRef`, which folds
 *   per venue — Kalshi shouts, both Polymarkets do not. Nothing here upper-cases
 *   an identifier itself.
 */

import type { AssetClass, EntGenre, ImpliedMethod, MoverSort, Venue } from '$gen';
import type { CandleInterval } from '../api/client';
import { UsageError } from './command';
import {
  looksLikeSymbol,
  looksLikeTicker,
  parseDuration,
  parseInterval,
  type ParsedCommand,
} from './parser';
import type { KeyScope } from './keys';
import { VENUE_IDS, parseRef, parseVenue, type VenueRef } from './venue';
import { formatDataRef, parseDataSource, type DataRef, type DataSource } from './dataset';

/**
 * The genres `ENT` filters by, in the order it lists them.
 *
 * Typed against the generated `EntGenre` rather than restated as strings, so a
 * genre added or dropped on the server is a compile error here instead of a
 * filter that silently returns nothing.
 */
export const ENT_GENRES: readonly EntGenre[] = [
  'music',
  'film',
  'tv',
  'games',
  'awards',
  'celeb',
] as const;

/** How a price series is drawn. Shared by `GP` and by `STK`/`CRY`/`IMP`. */
export type ChartStyle = 'candle' | 'line';

/* ----------------------------------------------------------- reading arguments */

export function requireArg(command: ParsedCommand, index: number, name: string): string {
  const value = command.args[index];
  if (!value) throw new UsageError(`Missing <${name}>`);
  return value;
}

/**
 * Read `[venue:]identifier` from an argument.
 *
 * Unprefixed means Kalshi, so every command, example and habit that predates
 * the other two venues still means what it always did. A bad prefix is a usage
 * error rather than a request the terminal sends to the wrong exchange.
 */
export function requireRef(command: ParsedCommand, index: number, name: string): VenueRef {
  const raw = requireArg(command, index, name);
  try {
    return parseRef(raw);
  } catch (err) {
    throw new UsageError(err instanceof Error ? err.message : `Bad <${name}>`);
  }
}

/**
 * Pull venue names out of an argument list, leaving the rest untouched.
 *
 * Read in `word` context, so aliases that are also ordinary English — `us`,
 * `intl`, a bare `k`, `gem`, `fx`, `predict`, `forecast` — are not claimed out
 * of a query. `SRCH us election` is a search for two words, not a filtered
 * search for one, and a row click that dispatches `SRCH The Office US` must not
 * quietly change which exchange was searched. The `pmus:` prefix, and each
 * venue's unambiguous name, remain the ways to say it.
 *
 * A repeated venue token stays in `rest` rather than vanishing: dropping it
 * from both lists silently ate a word.
 */
export function takeSources(args: string[]): {
  sources: readonly DataSource[];
  rest: string[];
} {
  const sources: DataSource[] = [];
  const rest: string[] = [];

  for (const arg of args) {
    const source = parseDataSource(arg, 'word');
    // A repeated name stays in `rest` rather than vanishing: dropping it from
    // both lists silently ate a word. Same rule as `takeVenues`.
    if (source && !sources.includes(source)) {
      sources.push(source);
    } else {
      rest.push(arg);
    }
  }

  return { sources, rest };
}

export function takeVenues(args: string[]): { venues: readonly Venue[]; rest: string[] } {
  const venues: Venue[] = [];
  const rest: string[] = [];

  for (const token of args) {
    const venue = parseVenue(token, 'word');
    if (venue && !venues.includes(venue)) venues.push(venue);
    else rest.push(token);
  }

  return { venues: venues.length ? venues : VENUE_IDS, rest };
}

/* --------------------------------------------------------- GP argument parsing */

const DEFAULT_LOOKBACK: Record<CandleInterval, number> = {
  1: 6 * 3600,
  60: 30 * 86400,
  1440: 365 * 86400,
};

/** What `GP` opens a chart panel with. */
export type ChartProps = {
  ref: VenueRef;
  interval: CandleInterval;
  /** Look-back window in seconds. */
  lookbackSeconds: number;
  style: ChartStyle;
};

/**
 * `GP <ticker> [interval] [range] [line|candle]` — order-independent after the
 * ticker, because nobody remembers argument order under time pressure.
 */
export function parseChartArgs(args: string[]): ChartProps {
  const ticker = args[0];
  if (!ticker || !looksLikeTicker(ticker)) throw new UsageError('Missing <ticker>');
  // Read the reference up front and as a usage error: `looksLikeTicker` admits
  // a colon, so `GP foo:bar` reaches parseRef, which throws a plain Error and
  // loses the ` · usage:` line that every other command's bad ref gets.
  let ref: VenueRef;
  try {
    ref = parseRef(ticker);
  } catch (err) {
    throw new UsageError(err instanceof Error ? err.message : 'Bad <ticker>');
  }

  let interval: CandleInterval | null = null;
  let lookbackSeconds: number | null = null;
  let style: ChartStyle = 'candle';

  for (const token of args.slice(1)) {
    const lower = token.toLowerCase();
    if (lower === 'line' || lower === 'area') {
      style = 'line';
      continue;
    }
    if (lower === 'candle' || lower === 'candles' || lower === 'ohlc') {
      style = 'candle';
      continue;
    }

    // An interval alias wins over a duration reading: `1d` means daily bars,
    // which is far more often what someone typing `GP X 1d` wants.
    const asInterval = parseInterval(token);
    if (asInterval !== null && interval === null) {
      interval = asInterval;
      continue;
    }

    const asDuration = parseDuration(token);
    if (asDuration !== null) {
      lookbackSeconds = asDuration;
      continue;
    }

    throw new UsageError(`Unrecognised argument "${token}"`);
  }

  const resolvedInterval = interval ?? 60;
  return {
    ref,
    interval: resolvedInterval,
    lookbackSeconds: lookbackSeconds ?? DEFAULT_LOOKBACK[resolvedInterval],
    style,
  };
}

/* ------------------------------------------- spot / implied argument parsing */

/** What `STK`, `CRY` and `IMP` open a spot panel with. */
export type SpotProps = {
  symbol: string;
  assetClass: AssetClass;
  interval: CandleInterval;
  /** Look-back window in seconds. */
  lookbackSeconds: number;
  style: ChartStyle;
  /** Kalshi event tickers currently overlaid, in selection order. */
  overlays: string[];
  method: ImpliedMethod;
  /** Open with the picker expanded — how `IMP` differs from `STK`/`CRY`. */
  showPicker: boolean;
};

/**
 * `STK <symbol> [interval] [range] [line|candle]`, and `IMP` additionally takes
 * Kalshi event tickers to overlay.
 *
 * Order-independent after the symbol, like `GP`. Telling an event ticker from a
 * symbol is unambiguous in practice: a Kalshi event ticker contains a hyphen and
 * a symbol does not — except for crypto pairs like `BTC-USD`, which is why the
 * *first* argument is always read as the symbol and never as an overlay.
 */
export function parseSpotArgs(
  args: string[],
  defaults: { assetClass: AssetClass; withPicker: boolean },
): SpotProps {
  const symbol = args[0];
  if (!symbol || !looksLikeSymbol(symbol)) throw new UsageError('Missing <symbol>');

  let interval: CandleInterval | null = null;
  let lookbackSeconds: number | null = null;
  let style: ChartStyle = 'candle';
  let method: ImpliedMethod = 'median';
  const overlays: string[] = [];

  for (const token of args.slice(1)) {
    const lower = token.toLowerCase();
    if (lower === 'line' || lower === 'area') {
      style = 'line';
      continue;
    }
    if (lower === 'candle' || lower === 'candles' || lower === 'ohlc') {
      style = 'candle';
      continue;
    }
    if (lower === 'median' || lower === 'mean') {
      method = lower;
      continue;
    }

    const asInterval = parseInterval(token);
    if (asInterval !== null && interval === null) {
      interval = asInterval;
      continue;
    }

    const asDuration = parseDuration(token);
    if (asDuration !== null) {
      lookbackSeconds = asDuration;
      continue;
    }

    // Anything left with a hyphen is a Kalshi event ticker to overlay.
    if (token.includes('-')) {
      overlays.push(token.toUpperCase());
      continue;
    }

    throw new UsageError(`Unrecognised argument "${token}"`);
  }

  const resolvedInterval = interval ?? 60;
  return {
    symbol: symbol.toUpperCase(),
    assetClass: defaults.assetClass,
    interval: resolvedInterval,
    lookbackSeconds: lookbackSeconds ?? DEFAULT_LOOKBACK[resolvedInterval],
    style,
    overlays,
    method,
    showPicker: defaults.withPicker,
  };
}

/**
 * Crypto or equity, from the symbol alone.
 *
 * `STK` and `CRY` state it outright; `IMP BTC` has to work it out. A bare
 * three-or-four letter symbol is ambiguous in principle — `LINK` is both a
 * token and an NYSE listing — so this only claims the ones Kalshi actually
 * lists crypto ladders for, and treats everything else as an equity.
 */
const CRYPTO_SYMBOLS = new Set([
  'BTC',
  'XBT',
  'BITCOIN',
  'ETH',
  'ETHEREUM',
  'SOL',
  'SOLANA',
  'XRP',
  'RIPPLE',
  'BNB',
  'HYPE',
  'DOGE',
  'ADA',
  'AVAX',
  'LINK',
  'LTC',
  'BCH',
  'DOT',
  'XLM',
  'ZEC',
]);

export function guessAssetClass(symbol: string): AssetClass {
  const upper = symbol.toUpperCase();
  const base = upper.split('-')[0] ?? upper;
  return CRYPTO_SYMBOLS.has(upper) || CRYPTO_SYMBOLS.has(base) ? 'crypto' : 'stock';
}

/* ---------------------------------------------------------- NEWS arguments */

const NEWS_DEFAULT_LIMIT = 30;
const NEWS_DEFAULT_DAYS = 7;
/** Alpaca caps a page at 50; the window cap is this terminal's own sanity bound. */
const NEWS_MAX_LIMIT = 50;
const NEWS_MAX_DAYS = 90;

/** What `NEWS` opens the wire with. */
export type NewsProps = {
  /** Symbols to filter to. Empty means the whole wire. */
  symbols: string[];
  limit: number;
  days: number;
};

/**
 * `NEWS [symbol…] [count] [window]` — every argument optional, in any order.
 *
 * The three argument kinds are told apart by shape rather than position: a bare
 * integer is a headline count, a duration is the look-back window, and anything
 * else has to be a symbol. That ordering matters — `30` is a valid symbol shape
 * too, so the count is claimed first, and `7d` parses as a duration before it
 * can be mistaken for a ticker.
 */
export function parseNewsArgs(args: string[]): NewsProps {
  const symbols: string[] = [];
  let limit = NEWS_DEFAULT_LIMIT;
  let days = NEWS_DEFAULT_DAYS;

  for (const token of args) {
    if (/^\d+$/.test(token)) {
      const value = Number(token);
      if (value < 1 || value > NEWS_MAX_LIMIT) {
        throw new UsageError(`Headline count must be between 1 and ${NEWS_MAX_LIMIT}`);
      }
      limit = value;
      continue;
    }

    const duration = parseDuration(token);
    if (duration !== null) {
      // Sub-day windows are legal to type and round up to a day: the wire is
      // sparse enough per ticker that anything shorter usually shows nothing.
      days = Math.min(Math.max(Math.round(duration / 86400), 1), NEWS_MAX_DAYS);
      continue;
    }

    if (!looksLikeSymbol(token)) throw new UsageError(`Unrecognised argument "${token}"`);
    const symbol = token.toUpperCase();
    if (!symbols.includes(symbol)) symbols.push(symbol);
  }

  return { symbols, limit, days };
}

/* ------------------------------------------------------------ TOP arguments */

/** The rankings `TOP` will accept, in the order its error message lists them. */
const MOVER_SORTS: readonly MoverSort[] = [
  'volume',
  'gainers',
  'losers',
  'open_interest',
  'liquidity',
];

/**
 * `TOP [sort]`, with the two abbreviations worth having.
 *
 * `oi` and `liq` are what the column headers say and what a trader types; the
 * long names are what the API takes. `raw` is reported back verbatim, so pass
 * it in the case the reader should see it named in.
 */
export function parseSort(raw: string): MoverSort {
  const sort = raw === 'oi' ? 'open_interest' : raw === 'liq' ? 'liquidity' : raw;
  if (!(MOVER_SORTS as readonly string[]).includes(sort)) {
    throw new UsageError(`Unknown sort "${raw}". Try: ${MOVER_SORTS.join(', ')}`);
  }
  return sort as MoverSort;
}

/* ---------------------------------------------------------- KEYS arguments */

/** `--global` / `--panel` / `--scope=panel`, or nothing and let the chord say. */
export function scopeFlag(command: ParsedCommand): KeyScope | undefined {
  const named = (command.flags['scope'] ?? '').toLowerCase();
  if (command.flags['global'] === 'true' || named === 'global') return 'global';
  if (command.flags['panel'] === 'true' || named === 'panel' || named === 'nav') return 'panel';
  return undefined;
}

/**
 * Re-join the arguments that make up a bound command line.
 *
 * Quoting has already been stripped by the tokeniser, so an argument that
 * contained spaces gets its quotes back — otherwise `KEYS alt+f FSRCH "real
 * gdp"` would store a three-argument command that means something else.
 */
export function joinCommand(args: string[]): string {
  return args.map((arg) => (/\s/.test(arg) ? `"${arg}"` : arg)).join(' ');
}

/* ----------------------------------------------------------------- panel ids */

/**
 * What each panel is keyed by.
 *
 * The id *is* the identity rule: two commands that produce the same id are the
 * same panel, so re-issuing `GP X` re-cuts the chart already on screen instead
 * of tiling a second copy of it, and `STK AAPL` then `IMP AAPL` reaches one
 * panel rather than two. Everything that distinguishes a panel from its
 * siblings goes in — the venue as well as the ticker, the venue filter as well
 * as the query — and everything that is a *setting* of one panel (bar size,
 * window, style) stays out, or changing the bar size would open a second chart.
 *
 * Kept together here, rather than beside each handler, because the rule is only
 * legible as a set.
 */
export const panelId = {
  quote: (ref: VenueRef): string => `des:${ref.venue}:${ref.id}`,
  depth: (ref: VenueRef): string => `ob:${ref.venue}:${ref.id}`,
  trades: (ref: VenueRef): string => `tas:${ref.venue}:${ref.id}`,
  chart: (ref: VenueRef): string => `gp:${ref.venue}:${ref.id}`,
  event: (ref: VenueRef): string => `evt:${ref.venue}:${ref.id}`,
  compare: (ref: VenueRef): string => `xvcmp:${ref.venue}:${ref.id}`,
  search: (query: string, venues: readonly Venue[]): string =>
    `srch:${venues.join('+')}:${query.toLowerCase()}`,
  top: (sort: string, venues: readonly Venue[]): string => `top:${venues.join('+')}:${sort}`,
  linkedSeries: (query: string): string => `xv:${query.toLowerCase()}`,
  watchlist: 'watchlist',
  spot: (symbol: string): string => `spot:${symbol.toUpperCase()}`,
  news: (symbols: readonly string[]): string => `news:${symbols.join(',').toLowerCase() || 'wire'}`,
  /**
   * The expiry is part of the identity, because two expiries of one underlying
   * are two different boards — where the underlying itself is one thing, however
   * many ways it is looked at.
   */
  optionChain: (symbol: string, expiry?: string): string =>
    `opt:${symbol.toUpperCase()}:${expiry ?? 'front'}`,
  optionQuote: (contract: string): string => `opd:${contract.toUpperCase()}`,
  optionVol: (symbol: string, expiry?: string): string =>
    `vol:${symbol.toUpperCase()}:${expiry ?? 'front'}`,
  optionPositioning: (symbol: string, expiry?: string): string =>
    `oi:${symbol.toUpperCase()}:${expiry ?? 'front'}`,
  /**
   * One panel per reference, so `ECO UNRATE` and `FRED UNRATE` land on the same
   * one rather than tiling two copies of the same chart.
   */
  dataSeries: (reference: DataRef): string => `eco:${formatDataRef(reference)}`,
  dataSearch: (query: string, sources: readonly DataSource[]): string =>
    `ecos:${[...sources].sort().join(',')}:${query.toLowerCase()}`,
  sources: (): string => 'src',
  filings: (company: string, form?: string): string =>
    `sec:${company.toUpperCase()}:${(form ?? 'all').toUpperCase()}`,
  bills: (query: string, congress?: number): string =>
    `cong:${congress ?? 'current'}:${query.toLowerCase()}`,
  datasets: (query: string): string => `dgov:${query.toLowerCase()}`,
  billboard: (chart: string, date?: string): string =>
    `bb:${chart.toLowerCase()}:${date ?? 'latest'}`,
  billboardCharts: 'bb:charts',
  ent: (genre: string): string => `ent:${genre.toLowerCase()}`,
  awards: (award: string, year?: number): string => `awrd:${award.toLowerCase()}:${year ?? 'all'}`,
  trends: (geo: string): string => `trnd:${geo.toLowerCase()}`,
  releases: (query: string, kind: string): string => `rel:${kind}:${query.toLowerCase()}`,
  podcasts: (view: string, country: string): string => `pod:${view}:${country}`,
  rt: (query: string): string => `rt:${query.toLowerCase()}`,
  rtSearch: (query: string): string => `rt:search:${query.toLowerCase()}`,
  netflix: (category: string, scope: string): string => `nflx:${category}:${scope}`.toLowerCase(),
  streamChart: (source: string, scope: string, period: string): string =>
    `${source}:${scope}:${period}`.toLowerCase(),
  boxOffice: (date?: string): string => `bo:${date ?? 'latest'}`,
  steam: (query: string): string => `steam:${query.toLowerCase() || 'top'}`,
  tv: (date?: string, country?: string): string =>
    `tv:${(country ?? 'us').toLowerCase()}:${date ?? 'today'}`,
  help: (topic?: string): string => (topic ? `help:${topic.toLowerCase()}` : 'help'),
  keys: 'keys',
} as const;
