/**
 * The command table.
 *
 * One entry per verb, each carrying its own help text, so `HELP` is generated
 * from the same source of truth that dispatches — help can never drift from
 * behaviour. Handlers receive the parsed command and a small context object;
 * they say what panel to open and report back, and never touch the DOM.
 *
 * A handler no longer builds a panel. It names a *kind* and the props that kind
 * takes, and the grid renders it: `panels.open({ id, kind, props })`. Because
 * the grid's `{#each}` is keyed by id, re-issuing a command for a panel already
 * on screen is a prop change rather than a teardown, which is what makes `GP X`
 * safe to mash and what removed the old "reconfigure in place or rebuild?"
 * branch from every handler here. `IMP` is the one command that means something
 * additive by being re-issued, and it says so with `open`'s `merge` callback.
 *
 * That change is also what breaks the import cycle this file used to sit in.
 * The old table imported every panel module, and the help panel imported the
 * table back — benign only because neither dereferenced the other at module
 * scope. Now the table names panel kinds as strings, so it imports no panel at
 * all: `panels/registry.ts` maps a kind to a component, and the help panel
 * imports this file the same way anything else does.
 */

import type { EntGenreFilter } from '$gen';
import { THEMES, type ThemeName } from '../state/workspace.svelte';
import {
  UsageError,
  countryCode,
  indexCommands,
  keyword,
  parsed,
  scanTokens,
  type Command,
  type CommandContext,
} from './command';
import { parseDataRef } from './dataset';
import { formatChord } from './keys';
import { isIsoDate, looksLikeTicker, parse } from './parser';
import {
  ENT_GENRES,
  guessAssetClass,
  joinCommand,
  panelId,
  parseChartArgs,
  parseNewsArgs,
  parseSort,
  parseSpotArgs,
  requireArg,
  requireRef,
  scopeFlag,
  takeSources,
  takeVenues,
  type SpotProps,
} from './registry';
import { formatRef } from './venue';

/**
 * Open or re-configure the chart for a symbol.
 *
 * One panel per symbol, so `STK AAPL` then `IMP AAPL` adds overlays to the
 * chart already on screen rather than tiling a second copy of it. Overlays
 * named on the command line are merged into whatever is already selected —
 * `IMP` is additive, matching what the picker does when clicked.
 */
function openSpot(props: SpotProps, { panels }: CommandContext): void {
  panels.open(
    { id: panelId.spot(props.symbol), kind: 'spot', props },
    // `IMP` is additive: overlays named on the command line join the ones
    // already drawn, matching what clicking the picker does.
    (existing, incoming) => ({
      ...incoming,
      overlays: [...new Set([...existing.overlays, ...incoming.overlays])],
    }),
  );
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
    handler(command, { panels }) {
      // Re-issuing GP for an open chart re-cuts it rather than stacking a
      // second chart of the same market: the id carries the market and not the
      // bar size, so the props change and the panel keeps its instance.
      const props = parseChartArgs(command.args);
      panels.open({ id: panelId.chart(props.ref), kind: 'chart', props });
    },
  },
  {
    verb: 'DES',
    aliases: ['Q', 'QUOTE'],
    group: 'markets',
    summary: 'Quote and contract description',
    usage: 'DES [venue:]<ticker>',
    examples: ['DES KXFEDDECISION-27JAN-H26', 'DES gem:GEMI-FED260917-MAINTAIN'],
    handler(command, { panels }) {
      const ref = requireRef(command, 0, 'ticker');
      panels.open({ id: panelId.quote(ref), kind: 'quote', props: { ref } });
    },
  },
  {
    verb: 'OB',
    aliases: ['DEPTH', 'BOOK'],
    group: 'markets',
    summary: 'Order book ladder',
    usage: 'OB [venue:]<ticker>',
    examples: ['OB KXFEDDECISION-27JAN-H26', 'OB pf:big-game-champion-2027~26952'],
    handler(command, { panels }) {
      const ref = requireRef(command, 0, 'ticker');
      panels.open({ id: panelId.depth(ref), kind: 'depth', props: { ref } });
    },
  },
  {
    verb: 'TAS',
    aliases: ['TRADES', 'TAPE'],
    group: 'markets',
    summary: 'Time and sales tape',
    usage: 'TAS [venue:]<ticker>',
    examples: ['TAS KXFEDDECISION-27JAN-H26', 'TAS fx:HORC_1126_Republican'],
    handler(command, { panels }) {
      const ref = requireRef(command, 0, 'ticker');
      panels.open({ id: panelId.trades(ref), kind: 'trades', props: { ref } });
    },
  },
  {
    verb: 'SRCH',
    aliases: ['S', 'FIND'],
    group: 'markets',
    summary: 'Search open events across every venue',
    usage: 'SRCH <words> [kalshi|pm|pmus|gemini|pf|fex]',
    examples: ['SRCH fed decision', 'SRCH bitcoin pm', 'SRCH senate kalshi gemini'],
    handler(command, { panels }) {
      const { venues, rest } = takeVenues(command.args);
      const query = rest.join(' ').trim();
      if (!query) throw new UsageError('Missing <words>');
      panels.open({
        id: panelId.search(query, venues),
        kind: 'search',
        props: { query, venues },
      });
    },
  },
  {
    verb: 'EVT',
    aliases: ['EVENT', 'LADDER'],
    group: 'markets',
    summary: 'All contracts in an event',
    usage: 'EVT [venue:]<event-ticker>',
    examples: ['EVT KXFEDDECISION-27JAN', 'EVT gem:DEMNOM2028'],
    handler(command, { panels }) {
      const ref = requireRef(command, 0, 'event-ticker');
      panels.open({ id: panelId.event(ref), kind: 'event', props: { ref } });
    },
  },
  {
    verb: 'TOP',
    aliases: ['MOVERS'],
    group: 'markets',
    summary: 'Leaderboards: volume, movers, open interest',
    usage: 'TOP [volume|gainers|losers|oi|liquidity] [kalshi|pm|pmus|gemini|pf|fex]',
    examples: ['TOP', 'TOP gainers', 'TOP oi kalshi', 'TOP volume pf'],
    handler(command, { panels }) {
      const { venues, rest } = takeVenues(command.args);
      const sort = parseSort((rest[0] ?? 'volume').toLowerCase());
      panels.open({ id: panelId.top(sort, venues), kind: 'top', props: { sort, venues } });
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
    examples: ['IMP BTC', 'IMP BTC KXBTCD-26AUG1617 1h 7d', 'IMP SPX 1h 5d', 'IMP ETH mean'],
    handler(command, context) {
      const symbol = command.args[0];
      if (!symbol) throw new UsageError('Missing <symbol>');
      const props = parseSpotArgs(command.args, {
        assetClass: guessAssetClass(symbol),
        withPicker: true,
      });
      openSpot(props, context);
      if (props.overlays.length === 0) {
        context.log(
          `Pick a Kalshi expiry under the chart to overlay its implied price on ${props.symbol}.`,
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
    examples: [
      'XV',
      'XV fed',
      'XV senate',
      'XV KXFEDDECISION-26OCT',
      'XV pm:fed-decision-in-october',
    ],
    handler(command, { panels }) {
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
        panels.open({ id: panelId.compare(ref), kind: 'compare', props: { ref } });
        return;
      }

      const query = command.args.join(' ').trim();
      panels.open({
        id: panelId.linkedSeries(query),
        kind: 'linked-series',
        props: { query },
      });
    },
  },
  {
    verb: 'NEWS',
    aliases: ['N', 'WIRE'],
    group: 'data',
    summary: 'Market news wire, for a symbol or the whole tape',
    usage: 'NEWS [symbol…] [count] [window]',
    examples: ['NEWS', 'NEWS NVDA', 'NEWS AAPL MSFT 50', 'NEWS BTCUSD 30d'],
    handler(command, { panels }) {
      // Re-issuing NEWS for symbols already on screen re-runs that panel with
      // the new count and window rather than tiling the same wire twice.
      const props = parseNewsArgs(command.args);
      panels.open({ id: panelId.news(props.symbols), kind: 'news', props });
    },
  },
  {
    verb: 'OPT',
    aliases: ['CHAIN'],
    group: 'markets',
    summary: 'The option chain for one expiry, both legs against one ladder',
    usage: 'OPT <symbol> [expiry]',
    // The expiry accepts a date, a horizon or an index into the strip, because
    // nobody remembers which Friday a board lists.
    examples: ['OPT BTC', 'OPT AAPL', 'OPT SPY 30d', 'OPT BTC 2026-12-25', 'OPT NVDA 2'],
    handler(command, { panels }) {
      const symbol = requireArg(command, 0, 'symbol').toUpperCase();
      const expiry = command.args[1];
      panels.open({
        id: panelId.optionChain(symbol, expiry),
        kind: 'option-chain',
        props: { symbol, ...(expiry ? { expiry } : {}) },
      });
    },
  },
  {
    verb: 'OPD',
    group: 'markets',
    summary: 'One option contract, its pair leg and its Greeks',
    // Routed on the shape of the identifier rather than an asset class, so an
    // OCC symbol and a Deribit instrument name can both be pasted straight in.
    usage: 'OPD <contract>',
    examples: ['OPD BTC-25DEC26-104000-C', 'OPD AAPL260918C00300000'],
    handler(command, { panels }) {
      const contract = requireArg(command, 0, 'contract').toUpperCase();
      panels.open({
        id: panelId.optionQuote(contract),
        kind: 'option-quote',
        props: { contract },
      });
    },
  },
  {
    verb: 'VOL',
    aliases: ['SMILE'],
    group: 'markets',
    summary: 'The volatility smile for one expiry, and the term structure behind it',
    usage: 'VOL <symbol> [expiry]',
    examples: ['VOL BTC', 'VOL SPY', 'VOL AAPL 60d'],
    handler(command, { panels }) {
      const symbol = requireArg(command, 0, 'symbol').toUpperCase();
      const expiry = command.args[1];
      panels.open({
        id: panelId.optionVol(symbol, expiry),
        kind: 'option-vol',
        props: { symbol, ...(expiry ? { expiry } : {}) },
      });
    },
  },
  {
    verb: 'OI',
    aliases: ['PAIN'],
    group: 'markets',
    summary: 'Open interest by strike, and the max-pain curve',
    usage: 'OI <symbol> [expiry]',
    examples: ['OI BTC', 'OI SPY', 'OI TSLA 30d'],
    handler(command, { panels }) {
      const symbol = requireArg(command, 0, 'symbol').toUpperCase();
      const expiry = command.args[1];
      panels.open({
        id: panelId.optionPositioning(symbol, expiry),
        kind: 'option-positioning',
        props: { symbol, ...(expiry ? { expiry } : {}) },
      });
    },
  },
  {
    verb: 'ECO',
    // `FRED <id>` predates the other eleven publishers, and every habit,
    // example and README line in this terminal says it. It stays, and reaches
    // the same panel — an unprefixed reference is a FRED series.
    aliases: ['FRED'],
    group: 'data',
    summary: 'A published series at any of eight publishers',
    usage: 'ECO [source:]<id> [start] [end]',
    examples: [
      'ECO UNRATE',
      'ECO bls:LNS14000000',
      'ECO ecb:EXR/D.USD.EUR.SP00.A',
      'ECO fed:H15/RIFLGFCY10_N.B 2020-01-01',
    ],
    handler(command, { panels }) {
      const reference = parseDataRef(requireArg(command, 0, 'id'));
      const start = command.args[1];
      const end = command.args[2];

      if (start !== undefined && !isIsoDate(start)) {
        throw new UsageError(`Start date must be YYYY-MM-DD, got "${start}"`);
      }
      if (end !== undefined && !isIsoDate(end)) {
        throw new UsageError(`End date must be YYYY-MM-DD, got "${end}"`);
      }

      panels.open({
        id: panelId.dataSeries(reference),
        kind: 'data-series',
        props: { reference, ...(start ? { start } : {}), ...(end ? { end } : {}) },
      });
    },
  },
  {
    verb: 'SEC',
    aliases: ['EDGAR', 'FILINGS'],
    group: 'data',
    summary: 'What a company has filed with the SEC, newest first',
    // An 8-K lands here within seconds of acceptance, which is often before the
    // press release — so a form filter is the argument that matters.
    usage: 'SEC <ticker|CIK|name> [form]',
    examples: ['SEC AAPL', 'SEC TSLA 8-K', 'SEC 320193 10-K', 'SEC berkshire'],
    handler(command, { panels }) {
      const company = requireArg(command, 0, 'ticker|CIK|name');
      const form = command.args[1];
      panels.open({
        id: panelId.filings(company, form),
        kind: 'filings',
        props: { company, ...(form ? { form } : {}) },
      });
    },
  },
  {
    verb: 'CONG',
    aliases: ['BILL', 'BILLS'],
    group: 'data',
    summary: 'Bills before Congress, most recently acted on first',
    usage: 'CONG [words] [congress]',
    examples: ['CONG', 'CONG shutdown', 'CONG appropriations 119'],
    handler(command, { panels }) {
      // A trailing three-digit number is a Congress, not a search word: no bill
      // title is the bare string `119`, and typing it is how someone asks for a
      // past Congress.
      const args = [...command.args];
      const last = args.at(-1);
      const congress =
        last !== undefined && /^\d{2,3}$/.test(last) ? Number(args.pop()) : undefined;
      const query = args.join(' ').trim();
      panels.open({
        id: panelId.bills(query, congress),
        kind: 'bills',
        props: { query, ...(congress ? { congress } : {}) },
      });
    },
  },
  {
    verb: 'DGOV',
    aliases: ['DATASETS'],
    group: 'data',
    summary: "Search data.gov's dataset catalogue",
    usage: 'DGOV <words>',
    examples: ['DGOV unemployment insurance', 'DGOV crop yields', 'DGOV housing starts'],
    handler(command, { panels }) {
      const query = command.args.join(' ').trim();
      if (!query) throw new UsageError('Missing <words>');
      panels.open({ id: panelId.datasets(query), kind: 'datasets', props: { query } });
    },
  },
  {
    verb: 'ECOS',
    aliases: ['FSRCH'],
    group: 'data',
    summary: 'Search every data publisher at once for a series id',
    usage: 'ECOS <words> [publisher…]',
    examples: ['ECOS unemployment rate', 'ECOS oil eia', 'ECOS inflation ecb oecd'],
    handler(command, { panels }) {
      const { sources, rest } = takeSources(command.args);
      const query = rest.join(' ').trim();
      if (!query) throw new UsageError('Missing <words>');
      panels.open({
        id: panelId.dataSearch(query, sources),
        kind: 'data-search',
        props: { query, sources },
      });
    },
  },
  {
    verb: 'SRC',
    aliases: ['SOURCES'],
    group: 'data',
    summary: 'Which data publishers this deployment can serve',
    usage: 'SRC',
    examples: ['SRC'],
    handler(_command, { panels }) {
      panels.open({ id: panelId.sources(), kind: 'sources', props: {} });
    },
  },
  {
    verb: 'BB',
    aliases: ['BILLBOARD'],
    group: 'data',
    summary: 'Billboard chart (scraped from billboard.com)',
    usage: 'BB [chart-slug] [YYYY-MM-DD] | BB CHARTS',
    examples: ['BB', 'BB billboard-200', 'BB hot-100 2025-06-14', 'BB CHARTS'],
    handler(command, { panels }) {
      const first = command.args[0];

      if (first && first.toUpperCase() === 'CHARTS') {
        panels.open({ id: panelId.billboardCharts, kind: 'billboard-charts', props: {} });
        return;
      }

      // `BB 2025-06-14` — a bare date means the default chart for that week.
      const chart = first && !isIsoDate(first) ? first.toLowerCase() : 'hot-100';
      const date = command.args.find((arg) => isIsoDate(arg));

      panels.open({
        id: panelId.billboard(chart, date),
        kind: 'billboard',
        props: { chart, ...(date ? { date } : {}) },
      });
    },
  },
  {
    verb: 'ENT',
    aliases: ['SHOW', 'SHOWBIZ'],
    group: 'markets',
    summary: 'Kalshi entertainment markets by genre',
    usage: `ENT [${ENT_GENRES.join('|')}]`,
    examples: ['ENT', 'ENT music', 'ENT games', 'ENT film'],
    handler(command, { panels }) {
      const raw = (command.args[0] ?? 'all').toLowerCase();
      const genre = raw === 'all' ? 'all' : raw;
      if (genre !== 'all' && !(ENT_GENRES as readonly string[]).includes(genre)) {
        throw new UsageError(`Unknown genre "${command.args[0]}". Try: ${ENT_GENRES.join(', ')}`);
      }
      panels.open({
        id: panelId.ent(genre),
        kind: 'ent',
        props: { genre: genre as EntGenreFilter },
      });
    },
  },
  {
    verb: 'RT',
    aliases: ['TOMATO', 'SCORE'],
    group: 'data',
    summary: 'Rotten Tomatoes scores (settles Kalshi KXRT)',
    usage: 'RT <title> | RT SEARCH <words>',
    examples: ['RT dune part three', 'RT wicked_for_good', 'RT SEARCH wicked'],
    handler(command, { panels }) {
      const first = (command.args[0] ?? '').toUpperCase();

      if (first === 'SEARCH' || first === 'S') {
        const query = command.args.slice(1).join(' ').trim();
        if (!query) throw new UsageError('Missing <words>');
        panels.open({ id: panelId.rtSearch(query), kind: 'rt-search', props: { query } });
        return;
      }

      const query = command.args.join(' ').trim();
      if (!query) throw new UsageError('Missing <title>');
      panels.open({ id: panelId.rt(query), kind: 'rt', props: { query } });
    },
  },
  {
    verb: 'NFLX',
    aliases: ['NETFLIX'],
    group: 'data',
    summary: 'Netflix Top 10 (settles Kalshi KXNETFLIX*)',
    usage: 'NFLX [tv|films] [global|<country>]',
    examples: ['NFLX', 'NFLX films', 'NFLX tv global', 'NFLX films gb'],
    handler(command, { panels }) {
      let category = 'tv';
      let scope = 'us';

      // Order-independent: `NFLX films gb` and `NFLX gb films` are one chart.
      // `tv` names a category before it names Tuvalu, so it is claimed first.
      scanTokens(command.args, [
        keyword(['tv', 'shows', 'show', 'series'], 'tv', (v) => (category = v)),
        keyword(['films', 'film', 'movies', 'movie'], 'films', (v) => (category = v)),
        keyword(['global', 'world'], 'global', (v) => (scope = v)),
        countryCode((code) => (scope = code)),
      ]);

      panels.open({
        id: panelId.netflix(category, scope),
        kind: 'netflix',
        props: { category, scope },
      });
    },
  },
  {
    verb: 'SPOT',
    aliases: ['SPOTIFY'],
    group: 'data',
    summary: 'Spotify streaming charts (settles Kalshi KXTOPARTIST*)',
    usage: 'SPOT [global|<country>] [daily|weekly]',
    examples: ['SPOT', 'SPOT global', 'SPOT global weekly', 'SPOT gb daily'],
    handler(command, { panels }) {
      let scope = 'us';
      let period = 'daily';

      scanTokens(command.args, [
        keyword(['daily', 'day'], 'daily', (v) => (period = v)),
        keyword(['weekly', 'week'], 'weekly', (v) => (period = v)),
        keyword(['global', 'world'], 'global', (v) => (scope = v)),
        countryCode((code) => (scope = code)),
      ]);

      const props = { source: 'spotify' as const, scope, period };
      panels.open({
        id: panelId.streamChart(props.source, scope, period),
        kind: 'stream-chart',
        props,
      });
    },
  },
  {
    verb: 'YT',
    aliases: ['YOUTUBE'],
    group: 'data',
    summary: 'YouTube music video charts (settles Kalshi KXYT*)',
    usage: 'YT [today|alltime|trending]',
    examples: ['YT', 'YT alltime', 'YT trending'],
    handler(command, { panels }) {
      const view = (command.args[0] ?? 'today').toLowerCase();
      const valid = ['today', 'alltime', 'trending'];
      if (!valid.includes(view)) {
        throw new UsageError(`Unknown view "${command.args[0]}". Try: ${valid.join(', ')}`);
      }

      const props = { source: 'youtube' as const, scope: view, period: '' };
      panels.open({
        id: panelId.streamChart(props.source, props.scope, props.period),
        kind: 'stream-chart',
        props,
      });
    },
  },
  {
    verb: 'BO',
    aliases: ['BOXOFFICE'],
    group: 'data',
    summary: 'Domestic daily box office (Box Office Mojo)',
    usage: 'BO [YYYY-MM-DD]',
    examples: ['BO', 'BO 2026-08-14'],
    handler(command, { panels }) {
      const date = command.args[0];
      if (date !== undefined && !isIsoDate(date)) {
        throw new UsageError(`Date must be YYYY-MM-DD, got "${date}"`);
      }
      panels.open({
        id: panelId.boxOffice(date),
        kind: 'boxoffice',
        props: { ...(date ? { date } : {}) },
      });
    },
  },
  {
    verb: 'AWRD',
    aliases: ['AWARD', 'AWARDS', 'OSCARS'],
    group: 'data',
    summary: 'Award nominees and winners (settles Kalshi KXOSCAR*, KXEMMY*, KXGRAMMY*)',
    usage: 'AWRD <award> [year]',
    examples: [
      'AWRD best picture',
      'AWRD best picture 2026',
      'AWRD drama series',
      'AWRD album of the year',
      'AWRD game of the year',
    ],
    handler(command, { panels }) {
      // A trailing four-digit token is the ceremony; everything else is the
      // award's name, which is several words often enough that joining is the
      // only sane reading.
      const args = [...command.args];
      const last = args.at(-1) ?? '';
      const year = /^(19|20)\d{2}$/.test(last)
        ? Number.parseInt(args.pop() as string, 10)
        : undefined;
      const award = args.join(' ').trim();

      if (!award) throw new UsageError('Missing <award>');

      panels.open({
        id: panelId.awards(award, year),
        kind: 'awards',
        props: year === undefined ? { query: award } : { query: award, year },
      });
    },
  },
  {
    verb: 'TRND',
    aliases: ['TRENDS', 'TRENDING'],
    group: 'data',
    summary: 'Google trending searches (settles Kalshi KXGOOGLESEARCH*)',
    usage: 'TRND [country]',
    examples: ['TRND', 'TRND GB', 'TRND JP'],
    handler(command, { panels }) {
      const geo = (command.args[0] ?? 'US').toUpperCase();
      panels.open({ id: panelId.trends(geo), kind: 'trends', props: { geo } });
    },
  },
  {
    verb: 'REL',
    aliases: ['RELEASE', 'RELEASES'],
    group: 'data',
    summary: 'Release dates and pre-orders (settles Kalshi KXALBUMRELEASE*)',
    usage: 'REL <artist> [album|song]',
    examples: ['REL taylor swift', 'REL drake song', 'REL tate mcrae album'],
    handler(command, { panels }) {
      const args = [...command.args];
      // The kind is a trailing keyword, so an artist called "Song" is still
      // reachable as `REL song album`.
      const last = (args.at(-1) ?? '').toLowerCase();
      const kind = ['album', 'albums', 'song', 'songs'].includes(last)
        ? (args.pop() as string).toLowerCase().replace(/s$/, '')
        : 'album';
      const query = args.join(' ').trim();

      if (!query) throw new UsageError('Missing <artist>');

      panels.open({
        id: panelId.releases(query, kind),
        kind: 'releases',
        props: { query, kind },
      });
    },
  },
  {
    verb: 'POD',
    aliases: ['PODCAST', 'PODCASTS'],
    group: 'data',
    summary: 'Apple podcast charts (settles Kalshi KXTOPPOD, KXROGANGUEST)',
    usage: 'POD [top|episodes] [country]',
    examples: ['POD', 'POD episodes', 'POD top gb'],
    handler(command, { panels }) {
      let view = 'top';
      let country = 'us';

      for (const token of command.args) {
        const lower = token.toLowerCase();
        if (['top', 'shows', 'show', 'episodes', 'episode'].includes(lower)) {
          view = lower.startsWith('episode') ? 'episodes' : 'top';
        } else if (/^[a-z]{2}$/.test(lower)) {
          country = lower;
        }
      }

      panels.open({
        id: panelId.podcasts(view, country),
        kind: 'podcasts',
        props: { view, country },
      });
    },
  },
  {
    verb: 'STEAM',
    aliases: ['GAMES'],
    group: 'data',
    summary: 'Steam concurrent players (settles Kalshi KXSTEAM*)',
    usage: 'STEAM [game|appid]',
    examples: ['STEAM', 'STEAM counter-strike', 'STEAM 730'],
    handler(command, { panels }) {
      const query = command.args.join(' ').trim();
      panels.open({ id: panelId.steam(query), kind: 'steam', props: { query } });
    },
  },
  {
    verb: 'TV',
    aliases: ['SCHEDULE', 'GUIDE'],
    group: 'data',
    summary: 'TV schedule for a day (TVmaze)',
    usage: 'TV [YYYY-MM-DD] [country]',
    examples: ['TV', 'TV 2026-08-20', 'TV GB', 'TV 2026-08-20 CA'],
    handler(command, { panels }) {
      let date: string | undefined;
      let country: string | undefined;

      scanTokens(command.args, [
        parsed(
          (token) => (isIsoDate(token) ? token : null),
          (value) => (date = value),
        ),
        countryCode((code) => (country = code), 'upper'),
      ]);

      panels.open({
        id: panelId.tv(date, country),
        kind: 'tv',
        props: { ...(date ? { date } : {}), ...(country ? { country } : {}) },
      });
    },
  },
  {
    verb: 'W',
    aliases: ['WATCH', 'WATCHLIST'],
    group: 'workspace',
    summary: 'Watchlist monitor',
    usage: 'W | W ADD [venue:]<ticker> | W DEL [venue:]<ticker> | W CLEAR',
    examples: ['W', 'W ADD KXFEDDECISION-27JAN-H26', 'W ADD pm:fed-decision-in-october'],
    handler(command, { panels, workspace, log }) {
      const action = (command.args[0] ?? '').toUpperCase();
      // Opening an id that is already on screen focuses and refreshes it, so
      // one call covers "show me the watchlist" and "the watchlist just
      // changed" alike.
      const openWatchlist = (): void => {
        panels.open({ id: panelId.watchlist, kind: 'watchlist', props: {} });
      };

      if (action === 'ADD' || action === '+') {
        const ticker = formatRef(requireRef(command, 1, 'ticker'));
        log(
          workspace.addToWatchlist(ticker)
            ? `Added ${ticker} to the watchlist.`
            : `${ticker} is already on the watchlist (or it is full).`,
        );
        openWatchlist();
        return;
      }

      if (action === 'DEL' || action === 'RM' || action === '-') {
        const ticker = formatRef(requireRef(command, 1, 'ticker'));
        log(
          workspace.removeFromWatchlist(ticker)
            ? `Removed ${ticker} from the watchlist.`
            : `${ticker} is not on the watchlist.`,
        );
        // Only refresh the panel if it is open — `W DEL` does not open one.
        panels.handle(panelId.watchlist)?.refresh();
        return;
      }

      if (action === 'CLEAR') {
        log(`Cleared ${workspace.clearWatchlist()} tickers from the watchlist.`);
        panels.handle(panelId.watchlist)?.refresh();
        return;
      }

      if (action) throw new UsageError(`Unknown watchlist action "${command.args[0]}"`);
      openWatchlist();
    },
  },
  {
    verb: 'LAY',
    aliases: ['LAYOUT'],
    group: 'workspace',
    summary: 'Set the number of panel columns',
    usage: 'LAY <1-4|+|->',
    examples: ['LAY 1', 'LAY 3', 'LAY +'],
    handler(command, { panels, workspace, log }) {
      const argument = requireArg(command, 0, '1-4');
      // `+`/`-` are what a key binding wants: one keystroke, no argument to
      // remember, and it stops at the ends rather than wrapping to one column.
      const step =
        argument === '+' || argument.toUpperCase() === 'NEXT' ? 1 : argument === '-' ? -1 : 0;
      const value = step === 0 ? Number(argument) : panels.columns + step;

      if (!Number.isFinite(value) || value < 1 || value > 4) {
        if (step !== 0) return;
        throw new UsageError('Columns must be between 1 and 4');
      }

      panels.setColumns(value);
      workspace.setColumns(value);
      log(`Layout set to ${panels.columns} column${panels.columns === 1 ? '' : 's'}.`);
    },
  },
  {
    verb: 'FOCUS',
    aliases: ['FOC'],
    group: 'workspace',
    summary: 'Move panel focus, or hand the keyboard to the panels',
    usage: 'FOCUS <NEXT|PREV|1-9|LAST|NAV|CMD>',
    examples: ['FOCUS NEXT', 'FOCUS 3', 'FOCUS NAV', 'FOCUS CMD'],
    handler(command, { panels, setMode, log }) {
      const target = (command.args[0] ?? 'NEXT').toUpperCase();

      switch (target) {
        case 'NAV':
        case 'PANELS':
          setMode('nav');
          return;
        case 'CMD':
        case 'PROMPT':
          setMode('cmd');
          return;
        case 'NEXT':
        case '+':
          panels.cycleFocus(1);
          return;
        case 'PREV':
        case '-':
          panels.cycleFocus(-1);
          return;
        case 'LAST':
          if (!panels.focusAt(panels.count)) log('No panels are open.', 'warn');
          return;
        default:
          break;
      }

      const position = Number(target);
      if (!Number.isInteger(position) || position < 1) {
        throw new UsageError(`Unknown target "${command.args[0]}"`);
      }
      if (!panels.focusAt(position)) log(`There is no panel ${position}.`, 'warn');
    },
  },
  {
    verb: 'ROW',
    aliases: ['SEL'],
    group: 'workspace',
    summary: 'Drive the row cursor inside the focused panel',
    usage: 'ROW <NEXT|PREV|TOP|END|OPEN|ALT>',
    examples: ['ROW NEXT', 'ROW OPEN', 'ROW ALT'],
    handler(command, { panels, log }) {
      const panel = panels.focusedHandle;
      if (!panel) {
        log('No panel is focused.', 'warn');
        return;
      }

      const action = (command.args[0] ?? 'NEXT').toUpperCase();
      switch (action) {
        case 'NEXT':
        case 'DOWN':
          panel.moveCursor(1);
          return;
        case 'PREV':
        case 'UP':
          panel.moveCursor(-1);
          return;
        case 'TOP':
        case 'FIRST':
          panel.moveCursor('top');
          return;
        case 'END':
        case 'LAST':
          panel.moveCursor('end');
          return;
        case 'OPEN':
        case 'GO':
          if (!panel.activateCursor()) log('Nothing under the row cursor.', 'warn');
          return;
        case 'ALT':
        case 'ACTION':
          if (!panel.activateRowAction()) log('This row has no second action.', 'warn');
          return;
        default:
          throw new UsageError(`Unknown row action "${command.args[0]}"`);
      }
    },
  },
  {
    verb: 'ZOOM',
    aliases: ['MAX', 'SOLO'],
    group: 'workspace',
    summary: 'Give the focused panel the whole workspace, or restore the grid',
    usage: 'ZOOM',
    handler(_command, { panels, log }) {
      if (!panels.toggleZoom()) log('No panel is focused.', 'warn');
    },
  },
  {
    verb: 'CLR',
    aliases: ['CLEARLOG'],
    group: 'workspace',
    summary: 'Clear the message log',
    usage: 'CLR',
    handler(_command, { clearLog }) {
      clearLog();
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
      // Charts read their colours from CSS custom properties when they build
      // their palette, so a live chart keeps the old one until it reloads.
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
    summary: 'Force a reload of every panel, or just the focused one',
    usage: 'REFRESH [ALL|THIS]',
    examples: ['REFRESH', 'REFRESH THIS'],
    handler(command, { panels, log }) {
      const scope = (command.args[0] ?? 'ALL').toUpperCase();

      if (scope === 'THIS' || scope === 'PANEL') {
        if (!panels.refreshFocused()) log('No panel is focused.', 'warn');
        return;
      }

      if (scope !== 'ALL') throw new UsageError(`Unknown scope "${command.args[0]}"`);
      panels.refreshAll();
      log(`Refreshing ${panels.count} panels.`);
    },
  },
  {
    verb: 'MENU',
    aliases: ['BAR'],
    group: 'workspace',
    summary: 'The menu bar: every command, grouped and labelled',
    usage: 'MENU | MENU <name> | MENU NEXT|PREV|CLOSE',
    examples: ['MENU', 'MENU markets', 'MENU learn', 'MENU CLOSE'],
    handler(command, { menu }) {
      const argument = command.args[0]?.toUpperCase();

      switch (argument) {
        case undefined:
          menu.toggle();
          return;
        case 'CLOSE':
        case 'HIDE':
          menu.close();
          return;
        case 'NEXT':
          menu.cycle(1);
          return;
        case 'PREV':
        case 'PREVIOUS':
          menu.cycle(-1);
          return;
        default:
          break;
      }

      // A name, in full or by prefix. Naming the headings on failure is more
      // use than "unknown menu" — there are five of them and they fit on a line.
      if (!menu.open(command.args.join(' '))) {
        throw new UsageError(
          `No menu called "${command.args.join(' ')}". Try: ${menu.titles().join(', ')}.`,
        );
      }
    },
  },
  {
    verb: 'KEYS',
    aliases: ['KEY', 'KEYMAP', 'BIND'],
    group: 'workspace',
    summary: 'The key map: read it, rebind it, reset it',
    usage: 'KEYS | KEYS <chord> <command…> | KEYS DEL <chord> | KEYS RESET',
    examples: [
      'KEYS',
      'KEYS alt+b OB $',
      'KEYS "g w" W',
      'KEYS alt+e EVT --global',
      'KEYS DEL alt+b',
      'KEYS RESET',
    ],
    handler(command, { panels, keys, log }) {
      // Opening an id already on screen refreshes it, so every branch below
      // ends the same way and the map on screen is never the pre-edit one.
      const openKeys = (): void => {
        panels.open({ id: panelId.keys, kind: 'keys', props: {} });
      };

      const first = command.args[0];
      if (first === undefined) {
        openKeys();
        return;
      }

      const action = first.toUpperCase();

      if (action === 'RESET') {
        log(`Restored the presets, forgetting ${keys.reset()} edits.`);
        openKeys();
        return;
      }

      if (action === 'DEL' || action === 'RM' || action === 'UNBIND' || action === '-') {
        const chord = requireArg(command, 1, 'chord');
        const removed = keys.remove(chord, scopeFlag(command));
        log(
          removed ? `Unbound ${formatChord(removed.chord)}.` : `${chord} is not bound to anything.`,
        );
        openKeys();
        return;
      }

      // `KEYS SET <chord> …` and `KEYS <chord> …` mean the same thing; the verb
      // is optional because under time pressure nobody types it.
      const offset = action === 'SET' || action === 'ADD' || action === 'BIND' ? 1 : 0;
      const chord = requireArg(command, offset, 'chord');
      const line = joinCommand(command.args.slice(offset + 1));

      if (!line) {
        // A chord on its own is a question, not a binding.
        const existing = keys.describe(chord, scopeFlag(command));
        log(
          existing
            ? `${formatChord(existing.chord)} runs ${existing.command} (${existing.scope}, ${existing.source}).`
            : `${chord} is not bound to anything.`,
        );
        openKeys();
        return;
      }

      const bound = keys.set(chord, line, scopeFlag(command));
      // A binding that names no command is a binding that will fail under the
      // finger rather than here, so say so now.
      const verb = parse(
        bound.command.startsWith('>') ? bound.command.slice(1) : bound.command,
      ).verb;
      if (verb && !COMMAND_INDEX.has(verb)) {
        log(`"${verb}" is not a command — this binding will report that when pressed.`, 'warn');
      }

      log(`${formatChord(bound.chord)} runs ${bound.command} (${bound.scope}).`);
      openKeys();
    },
  },
  {
    verb: 'HELP',
    aliases: ['?', 'MAN'],
    group: 'workspace',
    summary: 'Command reference',
    usage: 'HELP [command]',
    examples: ['HELP', 'HELP GP'],
    handler(command, { panels }) {
      const topic = command.args[0]?.toUpperCase();
      panels.open({
        id: panelId.help(topic),
        kind: 'help',
        props: { ...(topic ? { topic } : {}) },
      });
    },
  },
];

/** Verb (or alias) → command. Built once; aliases are first-class. */
export const COMMAND_INDEX: Map<string, Command> = indexCommands(COMMANDS);

/** Every verb and alias, for autocomplete. */
export const ALL_VERBS: string[] = [...COMMAND_INDEX.keys()].sort();
