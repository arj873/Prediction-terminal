/**
 * The command table.
 *
 * One entry per verb, each carrying its own help text, so `HELP` is generated
 * from the same source of truth that dispatches — help can never drift from
 * behaviour. Handlers receive the parsed command and a small context object;
 * they open panels and report back, and never touch the DOM directly.
 */

import type { AssetClass, CandleInterval, EntGenre, ImpliedMethod, Venue } from '../../shared/types.js';
import { ENT_GENRES } from '../../shared/types.js';
import { VENUE_IDS, formatRef, parseRef, parseVenue, type VenueRef } from '../../shared/venue.js';
import type { PanelManager } from '../panels/manager.js';
import type { Workspace } from '../state.js';
import { THEMES, type ThemeName } from '../state.js';
import { BillboardChartsPanel, BillboardPanel } from '../panels/billboard.js';
import { ChartPanel, type ChartStyle } from '../panels/chart.js';
import { EventPanel, SearchPanel, TopPanel, WatchlistPanel } from '../panels/browse.js';
import { ComparePanel, LinkedSeriesPanel } from '../panels/crossvenue.js';
import { EntPanel, RtPanel, RtSearchPanel } from '../panels/entertainment.js';
import { FredPanel, FredSearchPanel } from '../panels/fred.js';
import {
  BoxOfficePanel,
  NetflixPanel,
  SteamPanel,
  StreamChartPanel,
  TvPanel,
} from '../panels/mediadata.js';
import { DepthPanel, QuotePanel, TradesPanel } from '../panels/market.js';
import { HelpPanel } from '../panels/help.js';
import { SpotPanel, type SpotPanelOptions, type SpotStyle } from '../panels/spot.js';
import type { PanelContext } from '../panels/panel.js';
import {
  isIsoDate,
  looksLikeSymbol,
  looksLikeTicker,
  parseDuration,
  parseInterval,
  type ParsedCommand,
} from './parser.js';

export interface CommandContext {
  panels: PanelManager;
  workspace: Workspace;
  panelContext: PanelContext;
  log(message: string, level?: 'info' | 'warn' | 'error'): void;
  /** Re-dispatch a command string. */
  run(command: string): void;
}

export interface Command {
  verb: string;
  aliases?: string[];
  /** One-line summary shown in `HELP`. */
  summary: string;
  /** Usage line, e.g. `GP <ticker> [1m|1h|1d] [range]`. */
  usage: string;
  /** Worked examples, shown in `HELP <verb>`. */
  examples?: string[];
  group: 'markets' | 'data' | 'workspace';
  handler(command: ParsedCommand, context: CommandContext): void | Promise<void>;
}

class UsageError extends Error {}

function requireArg(command: ParsedCommand, index: number, name: string): string {
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
function requireRef(command: ParsedCommand, index: number, name: string): VenueRef {
  const raw = requireArg(command, index, name);
  try {
    return parseRef(raw);
  } catch (err) {
    throw new UsageError(err instanceof Error ? err.message : `Bad <${name}>`);
  }
}

/** Pull venue names out of an argument list, leaving the rest untouched. */
function takeVenues(args: string[]): { venues: readonly Venue[]; rest: string[] } {
  const venues: Venue[] = [];
  const rest: string[] = [];

  for (const token of args) {
    const venue = parseVenue(token);
    if (venue && !venues.includes(venue)) venues.push(venue);
    else if (!venue) rest.push(token);
  }

  return { venues: venues.length ? venues : VENUE_IDS, rest };
}

/* --------------------------------------------------------- GP argument parsing */

const DEFAULT_LOOKBACK: Record<CandleInterval, number> = {
  1: 6 * 3600,
  60: 30 * 86400,
  1440: 365 * 86400,
};

/**
 * `GP <ticker> [interval] [range] [line|candle]` — order-independent after the
 * ticker, because nobody remembers argument order under time pressure.
 */
export function parseChartArgs(args: string[]): {
  ref: VenueRef;
  interval: CandleInterval;
  lookbackSeconds: number;
  style: ChartStyle;
} {
  const ticker = args[0];
  if (!ticker || !looksLikeTicker(ticker)) throw new UsageError('Missing <ticker>');

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
    ref: parseRef(ticker),
    interval: resolvedInterval,
    lookbackSeconds: lookbackSeconds ?? DEFAULT_LOOKBACK[resolvedInterval],
    style,
  };
}

/* ------------------------------------------- spot / implied argument parsing */

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
): SpotPanelOptions {
  const symbol = args[0];
  if (!symbol || !looksLikeSymbol(symbol)) throw new UsageError('Missing <symbol>');

  let interval: CandleInterval | null = null;
  let lookbackSeconds: number | null = null;
  let style: SpotStyle = 'candle';
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
  'BTC', 'XBT', 'BITCOIN', 'ETH', 'ETHEREUM', 'SOL', 'SOLANA', 'XRP', 'RIPPLE',
  'BNB', 'HYPE', 'DOGE', 'ADA', 'AVAX', 'LINK', 'LTC', 'BCH', 'DOT', 'XLM', 'ZEC',
]);

export function guessAssetClass(symbol: string): AssetClass {
  const upper = symbol.toUpperCase();
  const base = upper.split('-')[0] ?? upper;
  return CRYPTO_SYMBOLS.has(upper) || CRYPTO_SYMBOLS.has(base) ? 'crypto' : 'stock';
}

/**
 * Open or re-configure the chart for a symbol.
 *
 * One panel per symbol, so `STK AAPL` then `IMP AAPL` adds overlays to the
 * chart already on screen rather than tiling a second copy of it. Overlays
 * named on the command line are merged into whatever is already selected —
 * `IMP` is additive, matching what the picker does when clicked.
 */
function openSpot(options: SpotPanelOptions, { panels, panelContext }: CommandContext): void {
  const id = SpotPanel.idFor(options.symbol);
  const existing = panels.find(id);

  if (existing instanceof SpotPanel) {
    const merged = [...new Set([...existing.options.overlays, ...options.overlays])];
    existing.reconfigure({ ...options, overlays: merged });
    panels.focus(id);
    return;
  }

  panels.open(id, () => new SpotPanel(id, panelContext, options));
}

/* ------------------------------------------------------------------ table */

export const COMMANDS: Command[] = [
  {
    verb: 'GP',
    aliases: ['CHART', 'GRAPH'],
    group: 'markets',
    summary: 'Price chart for a market at any venue',
    usage: 'GP [venue:]<ticker> [1m|1h|1d] [range] [candle|line]',
    examples: [
      'GP KXFEDDECISION-27JAN-H26',
      'GP KXFEDDECISION-27JAN-H26 1d 1y',
      'GP pm:will-there-be-no-change-in-fed-interest-rates-after-the-september-2026-meeting-615 1h 7d',
      'GP KXHIGHNY-26AUG16-B82.5 1m 6h line',
    ],
    handler(command, { panels, panelContext }) {
      const options = parseChartArgs(command.args);
      const id = ChartPanel.idFor(options.ref);
      const existing = panels.find(id);

      // Re-issuing GP for an open chart re-configures it rather than stacking
      // a second chart of the same market.
      if (existing instanceof ChartPanel) {
        existing.reconfigure(options);
        panels.focus(id);
        return;
      }
      panels.open(id, () => new ChartPanel(id, panelContext, options));
    },
  },
  {
    verb: 'DES',
    aliases: ['Q', 'QUOTE'],
    group: 'markets',
    summary: 'Quote and contract description',
    usage: 'DES [venue:]<ticker>',
    examples: ['DES KXFEDDECISION-27JAN-H26', 'DES pmus:apdc-jerpowgov-2026-12-31'],
    handler(command, { panels, panelContext }) {
      const ref = requireRef(command, 0, 'ticker');
      const id = QuotePanel.idFor(ref);
      panels.open(id, () => new QuotePanel(id, panelContext, ref));
    },
  },
  {
    verb: 'OB',
    aliases: ['DEPTH', 'BOOK'],
    group: 'markets',
    summary: 'Order book ladder',
    usage: 'OB [venue:]<ticker>',
    examples: ['OB KXFEDDECISION-27JAN-H26', 'OB pmus:tec-mlb-champ-2026-09-27-lad'],
    handler(command, { panels, panelContext }) {
      const ref = requireRef(command, 0, 'ticker');
      const id = DepthPanel.idFor(ref);
      panels.open(id, () => new DepthPanel(id, panelContext, ref));
    },
  },
  {
    verb: 'TAS',
    aliases: ['TRADES', 'TAPE'],
    group: 'markets',
    summary: 'Time and sales tape',
    usage: 'TAS [venue:]<ticker>',
    examples: ['TAS KXFEDDECISION-27JAN-H26', 'TAS pm:fed-decision-in-october'],
    handler(command, { panels, panelContext }) {
      const ref = requireRef(command, 0, 'ticker');
      const id = TradesPanel.idFor(ref);
      panels.open(id, () => new TradesPanel(id, panelContext, ref));
    },
  },
  {
    verb: 'SRCH',
    aliases: ['S', 'FIND'],
    group: 'markets',
    summary: 'Search open events across every venue',
    usage: 'SRCH <words> [kalshi|pm|pmus]',
    examples: ['SRCH fed decision', 'SRCH bitcoin pm', 'SRCH senate kalshi pmus'],
    handler(command, { panels, panelContext }) {
      const { venues, rest } = takeVenues(command.args);
      const query = rest.join(' ').trim();
      if (!query) throw new UsageError('Missing <words>');
      const id = SearchPanel.idFor(query, venues);
      panels.open(id, () => new SearchPanel(id, panelContext, query, venues));
    },
  },
  {
    verb: 'EVT',
    aliases: ['EVENT', 'LADDER'],
    group: 'markets',
    summary: 'All contracts in an event',
    usage: 'EVT [venue:]<event-ticker>',
    examples: ['EVT KXFEDDECISION-27JAN', 'EVT pm:fed-decision-in-september-762'],
    handler(command, { panels, panelContext }) {
      const ref = requireRef(command, 0, 'event-ticker');
      const id = EventPanel.idFor(ref);
      panels.open(id, () => new EventPanel(id, panelContext, ref));
    },
  },
  {
    verb: 'TOP',
    aliases: ['MOVERS'],
    group: 'markets',
    summary: 'Leaderboards: volume, movers, open interest',
    usage: 'TOP [volume|gainers|losers|oi|liquidity] [kalshi|pm|pmus]',
    examples: ['TOP', 'TOP gainers', 'TOP oi kalshi', 'TOP volume pm'],
    handler(command, { panels, panelContext }) {
      const { venues, rest } = takeVenues(command.args);
      const raw = (rest[0] ?? 'volume').toLowerCase();
      const sort =
        raw === 'oi' ? 'open_interest' : raw === 'liq' ? 'liquidity' : raw;
      const valid = ['volume', 'gainers', 'losers', 'open_interest', 'liquidity'];
      if (!valid.includes(sort)) {
        throw new UsageError(`Unknown sort "${raw}". Try: ${valid.join(', ')}`);
      }
      const id = TopPanel.idFor(sort, venues);
      panels.open(id, () => new TopPanel(id, panelContext, sort, venues));
    },
  },
  {
    verb: 'STK',
    aliases: ['EQ', 'STOCK'],
    group: 'markets',
    summary: 'Live stock, ETF or index chart',
    usage: 'STK <symbol> [1m|1h|1d] [range] [candle|line]',
    examples: ['STK AAPL', 'STK NVDA 1d 1y line', 'STK ^GSPC 1h 5d', 'STK SPX 1m 6h'],
    handler(command, context) {
      openSpot(parseSpotArgs(command.args, { assetClass: 'stock', withPicker: false }), context);
    },
  },
  {
    verb: 'CRY',
    aliases: ['COIN', 'CRYPTO'],
    group: 'markets',
    summary: 'Live crypto chart',
    usage: 'CRY <symbol> [1m|1h|1d] [range] [candle|line]',
    examples: ['CRY BTC', 'CRY ETH 1h 30d', 'CRY SOL 1d 1y line'],
    handler(command, context) {
      openSpot(parseSpotArgs(command.args, { assetClass: 'crypto', withPicker: false }), context);
    },
  },
  {
    verb: 'IMP',
    aliases: ['IMPLIED'],
    group: 'markets',
    summary: "Overlay Kalshi's implied price on the true price",
    usage: 'IMP <symbol> [event-ticker…] [1m|1h|1d] [range] [median|mean]',
    examples: [
      'IMP BTC',
      'IMP BTC KXBTCD-26AUG1617 1h 7d',
      'IMP SPX 1h 5d',
      'IMP ETH mean',
    ],
    handler(command, context) {
      const symbol = command.args[0];
      if (!symbol) throw new UsageError('Missing <symbol>');
      const options = parseSpotArgs(command.args, {
        assetClass: guessAssetClass(symbol),
        withPicker: true,
      });
      openSpot(options, context);
      if (options.overlays.length === 0) {
        context.log(
          `Pick a Kalshi expiry under the chart to overlay its implied price on ${options.symbol}.`,
        );
      }
    },
  },
  {
    verb: 'XV',
    aliases: ['CROSS', 'CMP'],
    group: 'markets',
    summary: 'The same question, priced at every broker that lists it',
    usage: 'XV [words] | XV [venue:]<event-ticker>',
    examples: ['XV', 'XV fed', 'XV senate', 'XV KXFEDDECISION-26OCT', 'XV pm:fed-decision-in-october'],
    handler(command, { panels, panelContext }) {
      const first = command.args[0];

      // An event ticker names one question to price; anything else is a search
      // for questions. Telling them apart: an identifier carries a venue prefix
      // or a hyphen, and a query does not — `XV fed` is words, `XV pm:fed-…`
      // and `XV KXFEDDECISION-26OCT` are references.
      const isReference =
        command.args.length === 1 &&
        first !== undefined &&
        looksLikeTicker(first) &&
        (first.includes(':') || first.includes('-'));

      if (isReference) {
        const ref = requireRef(command, 0, 'event-ticker');
        const id = ComparePanel.idFor(ref);
        panels.open(id, () => new ComparePanel(id, panelContext, ref));
        return;
      }

      const query = command.args.join(' ').trim();
      const id = LinkedSeriesPanel.idFor(query);
      panels.open(id, () => new LinkedSeriesPanel(id, panelContext, query));
    },
  },
  {
    verb: 'FRED',
    aliases: ['ECO'],
    group: 'data',
    summary: 'FRED economic series (scraped from stlouisfed.org)',
    usage: 'FRED <series-id> [start] [end]',
    examples: ['FRED UNRATE', 'FRED CPIAUCSL 2015-01-01', 'FRED DGS10 2020-01-01 2024-12-31'],
    handler(command, { panels, panelContext }) {
      const seriesId = requireArg(command, 0, 'series-id').toUpperCase();
      const start = command.args[1];
      const end = command.args[2];

      if (start !== undefined && !isIsoDate(start)) {
        throw new UsageError(`Start date must be YYYY-MM-DD, got "${start}"`);
      }
      if (end !== undefined && !isIsoDate(end)) {
        throw new UsageError(`End date must be YYYY-MM-DD, got "${end}"`);
      }

      const id = FredPanel.idFor(seriesId);
      panels.open(
        id,
        () => new FredPanel(id, panelContext, { id: seriesId, start, end }),
      );
    },
  },
  {
    verb: 'FSRCH',
    aliases: ['ECOS'],
    group: 'data',
    summary: 'Search FRED for a series id',
    usage: 'FSRCH <words>',
    examples: ['FSRCH unemployment rate', 'FSRCH "real gdp"'],
    handler(command, { panels, panelContext }) {
      const query = command.args.join(' ').trim();
      if (!query) throw new UsageError('Missing <words>');
      const id = FredSearchPanel.idFor(query);
      panels.open(id, () => new FredSearchPanel(id, panelContext, query));
    },
  },
  {
    verb: 'BB',
    aliases: ['BILLBOARD'],
    group: 'data',
    summary: 'Billboard chart (scraped from billboard.com)',
    usage: 'BB [chart-slug] [YYYY-MM-DD] | BB CHARTS',
    examples: ['BB', 'BB billboard-200', 'BB hot-100 2025-06-14', 'BB CHARTS'],
    handler(command, { panels, panelContext }) {
      const first = command.args[0];

      if (first && first.toUpperCase() === 'CHARTS') {
        panels.open(
          BillboardChartsPanel.ID,
          () => new BillboardChartsPanel(BillboardChartsPanel.ID, panelContext),
        );
        return;
      }

      // `BB 2025-06-14` — a bare date means the default chart for that week.
      const chart = first && !isIsoDate(first) ? first.toLowerCase() : 'hot-100';
      const date = command.args.find((arg) => isIsoDate(arg));

      const id = BillboardPanel.idFor(chart, date);
      panels.open(id, () => new BillboardPanel(id, panelContext, { chart, date }));
    },
  },
  {
    verb: 'ENT',
    aliases: ['SHOW', 'SHOWBIZ'],
    group: 'markets',
    summary: 'Kalshi entertainment markets by genre',
    usage: `ENT [${ENT_GENRES.join('|')}]`,
    examples: ['ENT', 'ENT music', 'ENT games', 'ENT film'],
    handler(command, { panels, panelContext }) {
      const raw = (command.args[0] ?? 'all').toLowerCase();
      const genre = raw === 'all' ? 'all' : raw;
      if (genre !== 'all' && !(ENT_GENRES as readonly string[]).includes(genre)) {
        throw new UsageError(`Unknown genre "${command.args[0]}". Try: ${ENT_GENRES.join(', ')}`);
      }
      const id = EntPanel.idFor(genre);
      panels.open(id, () => new EntPanel(id, panelContext, genre as EntGenre | 'all'));
    },
  },
  {
    verb: 'RT',
    aliases: ['TOMATO', 'SCORE'],
    group: 'data',
    summary: 'Rotten Tomatoes scores (settles Kalshi KXRT)',
    usage: 'RT <title> | RT SEARCH <words>',
    examples: ['RT dune part three', 'RT wicked_for_good', 'RT SEARCH wicked'],
    handler(command, { panels, panelContext }) {
      const first = (command.args[0] ?? '').toUpperCase();

      if (first === 'SEARCH' || first === 'S') {
        const query = command.args.slice(1).join(' ').trim();
        if (!query) throw new UsageError('Missing <words>');
        const id = RtSearchPanel.idFor(query);
        panels.open(id, () => new RtSearchPanel(id, panelContext, query));
        return;
      }

      const query = command.args.join(' ').trim();
      if (!query) throw new UsageError('Missing <title>');
      const id = RtPanel.idFor(query);
      panels.open(id, () => new RtPanel(id, panelContext, query));
    },
  },
  {
    verb: 'NFLX',
    aliases: ['NETFLIX'],
    group: 'data',
    summary: 'Netflix Top 10 (settles Kalshi KXNETFLIX*)',
    usage: 'NFLX [tv|films] [global|<country>]',
    examples: ['NFLX', 'NFLX films', 'NFLX tv global', 'NFLX films gb'],
    handler(command, { panels, panelContext }) {
      let category = 'tv';
      let scope = 'us';

      // Order-independent: `NFLX films gb` and `NFLX gb films` are one chart.
      for (const token of command.args) {
        const lower = token.toLowerCase();
        if (['tv', 'shows', 'show', 'series'].includes(lower)) category = 'tv';
        else if (['films', 'film', 'movies', 'movie'].includes(lower)) category = 'films';
        else if (lower === 'global' || lower === 'world') scope = 'global';
        else if (/^[a-z]{2}$/.test(lower)) scope = lower;
        else throw new UsageError(`Unrecognised argument "${token}"`);
      }

      const id = NetflixPanel.idFor(category, scope);
      panels.open(id, () => new NetflixPanel(id, panelContext, { category, scope }));
    },
  },
  {
    verb: 'SPOT',
    aliases: ['SPOTIFY'],
    group: 'data',
    summary: 'Spotify streaming charts (settles Kalshi KXTOPARTIST*)',
    usage: 'SPOT [global|<country>] [daily|weekly]',
    examples: ['SPOT', 'SPOT global', 'SPOT global weekly', 'SPOT gb daily'],
    handler(command, { panels, panelContext }) {
      let scope = 'us';
      let period = 'daily';

      for (const token of command.args) {
        const lower = token.toLowerCase();
        if (lower === 'daily' || lower === 'day') period = 'daily';
        else if (lower === 'weekly' || lower === 'week') period = 'weekly';
        else if (lower === 'global' || lower === 'world') scope = 'global';
        else if (/^[a-z]{2}$/.test(lower)) scope = lower;
        else throw new UsageError(`Unrecognised argument "${token}"`);
      }

      const options = { source: 'spotify' as const, scope, period };
      const id = StreamChartPanel.idFor(options);
      panels.open(id, () => new StreamChartPanel(id, panelContext, options));
    },
  },
  {
    verb: 'YT',
    aliases: ['YOUTUBE'],
    group: 'data',
    summary: 'YouTube music video charts (settles Kalshi KXYT*)',
    usage: 'YT [today|alltime|trending]',
    examples: ['YT', 'YT alltime', 'YT trending'],
    handler(command, { panels, panelContext }) {
      const view = (command.args[0] ?? 'today').toLowerCase();
      const valid = ['today', 'alltime', 'trending'];
      if (!valid.includes(view)) {
        throw new UsageError(`Unknown view "${command.args[0]}". Try: ${valid.join(', ')}`);
      }

      const options = { source: 'youtube' as const, scope: view, period: '' };
      const id = StreamChartPanel.idFor(options);
      panels.open(id, () => new StreamChartPanel(id, panelContext, options));
    },
  },
  {
    verb: 'BO',
    aliases: ['BOXOFFICE'],
    group: 'data',
    summary: 'Domestic daily box office (Box Office Mojo)',
    usage: 'BO [YYYY-MM-DD]',
    examples: ['BO', 'BO 2026-08-14'],
    handler(command, { panels, panelContext }) {
      const date = command.args[0];
      if (date !== undefined && !isIsoDate(date)) {
        throw new UsageError(`Date must be YYYY-MM-DD, got "${date}"`);
      }
      const id = BoxOfficePanel.idFor(date);
      panels.open(id, () => new BoxOfficePanel(id, panelContext, date));
    },
  },
  {
    verb: 'STEAM',
    aliases: ['GAMES'],
    group: 'data',
    summary: 'Steam concurrent players (settles Kalshi KXSTEAM*)',
    usage: 'STEAM [game|appid]',
    examples: ['STEAM', 'STEAM counter-strike', 'STEAM 730'],
    handler(command, { panels, panelContext }) {
      const query = command.args.join(' ').trim();
      const id = SteamPanel.idFor(query);
      panels.open(id, () => new SteamPanel(id, panelContext, query));
    },
  },
  {
    verb: 'TV',
    aliases: ['SCHEDULE', 'GUIDE'],
    group: 'data',
    summary: 'TV schedule for a day (TVmaze)',
    usage: 'TV [YYYY-MM-DD] [country]',
    examples: ['TV', 'TV 2026-08-20', 'TV GB', 'TV 2026-08-20 CA'],
    handler(command, { panels, panelContext }) {
      let date: string | undefined;
      let country: string | undefined;

      for (const token of command.args) {
        if (isIsoDate(token)) date = token;
        else if (/^[a-z]{2}$/i.test(token)) country = token.toUpperCase();
        else throw new UsageError(`Unrecognised argument "${token}"`);
      }

      const options = { ...(date ? { date } : {}), ...(country ? { country } : {}) };
      const id = TvPanel.idFor(options);
      panels.open(id, () => new TvPanel(id, panelContext, options));
    },
  },
  {
    verb: 'W',
    aliases: ['WATCH', 'WATCHLIST'],
    group: 'workspace',
    summary: 'Watchlist monitor',
    usage: 'W | W ADD [venue:]<ticker> | W DEL [venue:]<ticker> | W CLEAR',
    examples: ['W', 'W ADD KXFEDDECISION-27JAN-H26', 'W ADD pm:fed-decision-in-october'],
    handler(command, { panels, workspace, panelContext, log }) {
      const action = (command.args[0] ?? '').toUpperCase();
      const openPanel = (): void => {
        panels.open(
          WatchlistPanel.ID,
          () => new WatchlistPanel(WatchlistPanel.ID, panelContext, workspace),
        );
      };

      if (action === 'ADD' || action === '+') {
        const ticker = formatRef(requireRef(command, 1, 'ticker'));
        log(
          workspace.addToWatchlist(ticker)
            ? `Added ${ticker} to the watchlist.`
            : `${ticker} is already on the watchlist (or it is full).`,
        );
        openPanel();
        void panels.find(WatchlistPanel.ID)?.refresh();
        return;
      }

      if (action === 'DEL' || action === 'RM' || action === '-') {
        const ticker = formatRef(requireRef(command, 1, 'ticker'));
        log(
          workspace.removeFromWatchlist(ticker)
            ? `Removed ${ticker} from the watchlist.`
            : `${ticker} is not on the watchlist.`,
        );
        void panels.find(WatchlistPanel.ID)?.refresh();
        return;
      }

      if (action === 'CLEAR') {
        log(`Cleared ${workspace.clearWatchlist()} tickers from the watchlist.`);
        void panels.find(WatchlistPanel.ID)?.refresh();
        return;
      }

      if (action) throw new UsageError(`Unknown watchlist action "${command.args[0]}"`);
      openPanel();
    },
  },
  {
    verb: 'LAY',
    aliases: ['LAYOUT'],
    group: 'workspace',
    summary: 'Set the number of panel columns',
    usage: 'LAY <1-4>',
    examples: ['LAY 1', 'LAY 3'],
    handler(command, { panels, workspace, log }) {
      const value = Number(requireArg(command, 0, '1-4'));
      if (!Number.isFinite(value) || value < 1 || value > 4) {
        throw new UsageError('Columns must be between 1 and 4');
      }
      panels.setColumns(value);
      workspace.setColumns(value);
      log(`Layout set to ${panels.columns} column${panels.columns === 1 ? '' : 's'}.`);
    },
  },
  {
    verb: 'THEME',
    group: 'workspace',
    summary: 'Switch the colour scheme',
    usage: `THEME <${THEMES.join('|')}>`,
    examples: ['THEME amber', 'THEME green', 'THEME ice'],
    handler(command, { workspace, panels, log }) {
      const name = requireArg(command, 0, THEMES.join('|')).toLowerCase();
      if (!(THEMES as string[]).includes(name)) {
        throw new UsageError(`Unknown theme "${name}". Try: ${THEMES.join(', ')}`);
      }
      workspace.setTheme(name as ThemeName);
      // Charts read their colours from CSS variables at construction time.
      panels.refreshAll();
      log(`Theme set to ${name}.`);
    },
  },
  {
    verb: 'CLS',
    aliases: ['CLOSE'],
    group: 'workspace',
    summary: 'Close the focused panel, or all of them',
    usage: 'CLS [ALL]',
    examples: ['CLS', 'CLS ALL'],
    handler(command, { panels, log }) {
      if ((command.args[0] ?? '').toUpperCase() === 'ALL') {
        log(`Closed ${panels.closeAll()} panels.`);
        return;
      }
      const focused = panels.focused;
      if (!focused) {
        log('No panel is focused.', 'warn');
        return;
      }
      panels.close(focused.id);
    },
  },
  {
    verb: 'REFRESH',
    aliases: ['R'],
    group: 'workspace',
    summary: 'Force a reload of every panel',
    usage: 'REFRESH',
    handler(_command, { panels, log }) {
      panels.refreshAll();
      log(`Refreshing ${panels.panels.length} panels.`);
    },
  },
  {
    verb: 'HELP',
    aliases: ['?', 'MAN'],
    group: 'workspace',
    summary: 'Command reference',
    usage: 'HELP [command]',
    examples: ['HELP', 'HELP GP'],
    handler(command, { panels, panelContext }) {
      const topic = command.args[0]?.toUpperCase();
      const id = HelpPanel.idFor(topic);
      panels.open(id, () => new HelpPanel(id, panelContext, topic));
    },
  },
];

/** Verb (or alias) → command. Built once; aliases are first-class. */
export const COMMAND_INDEX: Map<string, Command> = new Map();
for (const command of COMMANDS) {
  COMMAND_INDEX.set(command.verb, command);
  for (const alias of command.aliases ?? []) COMMAND_INDEX.set(alias, command);
}

/** Every verb and alias, for autocomplete. */
export const ALL_VERBS: string[] = [...COMMAND_INDEX.keys()].sort();

export { UsageError };
