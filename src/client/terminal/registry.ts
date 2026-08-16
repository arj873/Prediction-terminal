/**
 * The command table.
 *
 * One entry per verb, each carrying its own help text, so `HELP` is generated
 * from the same source of truth that dispatches — help can never drift from
 * behaviour. Handlers receive the parsed command and a small context object;
 * they open panels and report back, and never touch the DOM directly.
 */

import type { CandleInterval } from '../../shared/types.js';
import type { PanelManager } from '../panels/manager.js';
import type { Workspace } from '../state.js';
import { THEMES, type ThemeName } from '../state.js';
import { BillboardChartsPanel, BillboardPanel } from '../panels/billboard.js';
import { ChartPanel, type ChartStyle } from '../panels/chart.js';
import { EventPanel, SearchPanel, TopPanel, WatchlistPanel } from '../panels/browse.js';
import { FredPanel, FredSearchPanel } from '../panels/fred.js';
import { DepthPanel, QuotePanel, TradesPanel } from '../panels/market.js';
import { HelpPanel } from '../panels/help.js';
import type { PanelContext } from '../panels/panel.js';
import { isIsoDate, looksLikeTicker, parseDuration, parseInterval, type ParsedCommand } from './parser.js';

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
  ticker: string;
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
    ticker: ticker.toUpperCase(),
    interval: resolvedInterval,
    lookbackSeconds: lookbackSeconds ?? DEFAULT_LOOKBACK[resolvedInterval],
    style,
  };
}

/* ------------------------------------------------------------------ table */

export const COMMANDS: Command[] = [
  {
    verb: 'GP',
    aliases: ['CHART', 'GRAPH'],
    group: 'markets',
    summary: 'Price chart for a Kalshi market',
    usage: 'GP <ticker> [1m|1h|1d] [range] [candle|line]',
    examples: [
      'GP KXFEDDECISION-27JAN-H26',
      'GP KXFEDDECISION-27JAN-H26 1d 1y',
      'GP KXHIGHNY-26AUG16-B82.5 1m 6h line',
    ],
    handler(command, { panels, panelContext }) {
      const options = parseChartArgs(command.args);
      const id = ChartPanel.idFor(options.ticker);
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
    usage: 'DES <ticker>',
    examples: ['DES KXFEDDECISION-27JAN-H26'],
    handler(command, { panels, panelContext }) {
      const ticker = requireArg(command, 0, 'ticker').toUpperCase();
      const id = QuotePanel.idFor(ticker);
      panels.open(id, () => new QuotePanel(id, panelContext, ticker));
    },
  },
  {
    verb: 'OB',
    aliases: ['DEPTH', 'BOOK'],
    group: 'markets',
    summary: 'Order book ladder',
    usage: 'OB <ticker>',
    examples: ['OB KXFEDDECISION-27JAN-H26'],
    handler(command, { panels, panelContext }) {
      const ticker = requireArg(command, 0, 'ticker').toUpperCase();
      const id = DepthPanel.idFor(ticker);
      panels.open(id, () => new DepthPanel(id, panelContext, ticker));
    },
  },
  {
    verb: 'TAS',
    aliases: ['TRADES', 'TAPE'],
    group: 'markets',
    summary: 'Time and sales tape',
    usage: 'TAS <ticker>',
    examples: ['TAS KXFEDDECISION-27JAN-H26'],
    handler(command, { panels, panelContext }) {
      const ticker = requireArg(command, 0, 'ticker').toUpperCase();
      const id = TradesPanel.idFor(ticker);
      panels.open(id, () => new TradesPanel(id, panelContext, ticker));
    },
  },
  {
    verb: 'SRCH',
    aliases: ['S', 'FIND'],
    group: 'markets',
    summary: 'Search open Kalshi events',
    usage: 'SRCH <words>',
    examples: ['SRCH fed decision', 'SRCH bitcoin', 'SRCH nyc temperature'],
    handler(command, { panels, panelContext }) {
      const query = command.args.join(' ').trim();
      if (!query) throw new UsageError('Missing <words>');
      const id = SearchPanel.idFor(query);
      panels.open(id, () => new SearchPanel(id, panelContext, query));
    },
  },
  {
    verb: 'EVT',
    aliases: ['EVENT', 'LADDER'],
    group: 'markets',
    summary: 'All contracts in an event',
    usage: 'EVT <event-ticker>',
    examples: ['EVT KXFEDDECISION-27JAN'],
    handler(command, { panels, panelContext }) {
      const ticker = requireArg(command, 0, 'event-ticker').toUpperCase();
      const id = EventPanel.idFor(ticker);
      panels.open(id, () => new EventPanel(id, panelContext, ticker));
    },
  },
  {
    verb: 'TOP',
    aliases: ['MOVERS'],
    group: 'markets',
    summary: 'Leaderboards: volume, movers, open interest',
    usage: 'TOP [volume|gainers|losers|oi|liquidity]',
    examples: ['TOP', 'TOP gainers', 'TOP oi'],
    handler(command, { panels, panelContext }) {
      const raw = (command.args[0] ?? 'volume').toLowerCase();
      const sort =
        raw === 'oi' ? 'open_interest' : raw === 'liq' ? 'liquidity' : raw;
      const valid = ['volume', 'gainers', 'losers', 'open_interest', 'liquidity'];
      if (!valid.includes(sort)) {
        throw new UsageError(`Unknown sort "${raw}". Try: ${valid.join(', ')}`);
      }
      const id = TopPanel.idFor(sort);
      panels.open(id, () => new TopPanel(id, panelContext, sort));
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
    verb: 'W',
    aliases: ['WATCH', 'WATCHLIST'],
    group: 'workspace',
    summary: 'Watchlist monitor',
    usage: 'W | W ADD <ticker> | W DEL <ticker> | W CLEAR',
    examples: ['W', 'W ADD KXFEDDECISION-27JAN-H26', 'W DEL KXFEDDECISION-27JAN-H26'],
    handler(command, { panels, workspace, panelContext, log }) {
      const action = (command.args[0] ?? '').toUpperCase();
      const openPanel = (): void => {
        panels.open(
          WatchlistPanel.ID,
          () => new WatchlistPanel(WatchlistPanel.ID, panelContext, workspace),
        );
      };

      if (action === 'ADD' || action === '+') {
        const ticker = requireArg(command, 1, 'ticker').toUpperCase();
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
        const ticker = requireArg(command, 1, 'ticker').toUpperCase();
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
