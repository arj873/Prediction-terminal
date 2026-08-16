/**
 * Equity, ETF and index option chains.
 *
 * The same two providers as `sources/stocks.ts`, in the same order and for the
 * same reason. **Yahoo Finance** is primary: `/v7/finance/options` is the only
 * free, keyless source that carries the whole US listed board with bid, ask,
 * volume and open interest per contract. **Nasdaq** is the fallback, because
 * Yahoo's `v7` hosts answer 429 to shared datacentre addresses — and notably
 * harder than `v8/chart` does, so a deployment where `STK AAPL` works can still
 * find `OPT AAPL` blocked. Nasdaq answers from those networks.
 *
 * They disagree about almost everything except the numbers:
 *
 * |  | Yahoo | Nasdaq |
 * | --- | --- | --- |
 * | Shape | JSON per expiry | one flat table, paged |
 * | Expiry list | `expirationDates` | inferred from group headers |
 * | Contract id | OCC symbol | a drill-down URL |
 * | Numbers | native | strings, `"--"` for missing |
 *
 * Both are normalised to an {@link OptionBoard} keyed by **OCC contract
 * symbols** (`AAPL260918C00300000`), which Yahoo already emits and which are
 * reconstructed for Nasdaq. That matters beyond tidiness: it is what lets a row
 * clicked in a chain served by one provider open a contract panel served by the
 * other.
 *
 * No implied volatility is taken from either provider. Yahoo publishes one, but
 * against its own undisclosed carry assumptions — mixing that with a forward
 * this terminal fits from parity would put two disagreeing volatilities on one
 * screen and Greeks that match neither. Every vol here is solved from the book
 * mid against the fitted forward, so price, vol and Greeks are one consistent
 * set. See `shared/greeks.ts`.
 */

import type { OptionUnderlying } from '../../shared/types.js';
import type { BoardQuote, OptionBoard } from './optionboard.js';
import { equityExpiryInstant } from '../../shared/greeks.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';
import { getQuote, parseNasdaqNumber } from './stocks.js';

const YAHOO_BASE = process.env.YAHOO_API_BASE ?? 'https://query1.finance.yahoo.com';
const NASDAQ_BASE = process.env.NASDAQ_API_BASE ?? 'https://api.nasdaq.com';

const BROWSER_UA =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36';

/** US listed equity options are 100 shares to a contract, without exception. */
const CONTRACT_SIZE = 100;

/** Rows per Nasdaq page, and how many pages one crawl will walk. */
const NASDAQ_PAGE = 500;
const NASDAQ_MAX_PAGES = 4;

/* -------------------------------------------------------------- OCC symbols */

const MONTHS = [
  'january', 'february', 'march', 'april', 'may', 'june',
  'july', 'august', 'september', 'october', 'november', 'december',
];

/**
 * Build the OCC option symbol the whole terminal keys on.
 *
 * `AAPL` + 2026-09-18 + call + 300 → `AAPL260918C00300000`. The strike is in
 * thousandths, zero-padded to eight digits, which is what makes a $0.50 strike
 * and a $500 strike sort and compare correctly as text.
 */
export function occSymbol(
  symbol: string,
  isoDate: string,
  type: 'call' | 'put',
  strike: number,
): string | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(isoDate);
  if (!match || !Number.isFinite(strike) || strike <= 0) return null;
  const [, year, month, day] = match;
  const thousandths = String(Math.round(strike * 1000)).padStart(8, '0');
  return `${symbol.toUpperCase().replace(/[^A-Z0-9]/g, '')}${year!.slice(2)}${month}${day}${type === 'call' ? 'C' : 'P'}${thousandths}`;
}

export interface ParsedOcc {
  symbol: string;
  date: string;
  type: 'call' | 'put';
  strike: number;
}

/** The inverse of {@link occSymbol}, for `OPD AAPL260918C00300000`. */
export function parseOcc(contract: string): ParsedOcc | null {
  const match = /^([A-Z]{1,6})(\d{2})(\d{2})(\d{2})([CP])(\d{8})$/.exec(contract.trim().toUpperCase());
  if (!match) return null;
  const [, symbol, yy, mm, dd, leg, strike] = match;
  return {
    symbol: symbol!,
    date: `20${yy}-${mm}-${dd}`,
    type: leg === 'C' ? 'call' : 'put',
    strike: Number(strike) / 1000,
  };
}

/* ------------------------------------------------------------------- yahoo */

interface YahooOptionRow {
  contractSymbol?: string;
  strike?: number;
  lastPrice?: number;
  change?: number;
  bid?: number;
  ask?: number;
  volume?: number;
  openInterest?: number;
  expiration?: number;
}

interface YahooOptionsResponse {
  optionChain?: {
    result?: {
      underlyingSymbol?: string;
      expirationDates?: number[];
      quote?: {
        regularMarketPrice?: number;
        shortName?: string;
        longName?: string;
        currency?: string;
      };
      options?: { expirationDate?: number; calls?: YahooOptionRow[]; puts?: YahooOptionRow[] }[];
    }[];
    error?: { description?: string } | null;
  };
}

async function yahoo<T>(path: string, ttlMs: number): Promise<T> {
  return cache.cached(`yahoo:${path}`, ttlMs, () =>
    fetchJson<T>(`${YAHOO_BASE}${path}`, {
      timeoutMs: 20_000,
      retries: 1,
      headers: { 'User-Agent': BROWSER_UA },
    }),
  );
}

function positive(value: number | undefined | null): number | null {
  return value === undefined || value === null || !Number.isFinite(value) || value <= 0
    ? null
    : value;
}

function finite(value: number | undefined | null): number | null {
  return value === undefined || value === null || !Number.isFinite(value) ? null : value;
}

/**
 * Yahoo's options payload → board quotes.
 *
 * Exported for tests. Yahoo pads a chain with contracts that have never traded
 * and have no book at all; those are kept rather than dropped, because an open
 * strike with no quote is a real feature of a board and a chain with holes
 * punched in it reads as missing data.
 */
export function parseYahooOptions(
  body: YahooOptionsResponse,
  symbol: string,
): { board: OptionBoard; expiries: number[] } {
  const result = body.optionChain?.result?.[0];
  if (!result) {
    throw new UpstreamError(
      body.optionChain?.error?.description ?? `Yahoo Finance returned no option board for ${symbol}`,
      {
        code: 'not_found',
        hint: 'Check the symbol. Not every listed name has options — most ETFs and large caps do.',
      },
    );
  }

  const quote = result.quote ?? {};
  const quotes: BoardQuote[] = [];

  for (const slice of result.options ?? []) {
    const legs: [YahooOptionRow[], 'call' | 'put'][] = [
      [slice.calls ?? [], 'call'],
      [slice.puts ?? [], 'put'],
    ];

    for (const [rows, type] of legs) {
      for (const row of rows) {
        const strike = positive(row.strike);
        const expiration = row.expiration ?? slice.expirationDate;
        if (strike === null || expiration === undefined) continue;

        // Yahoo dates an expiry at UTC midnight; the contract actually stops
        // trading at the close in New York, ~16 hours later. On a same-day
        // chain that difference is the entire remaining life of the option.
        const expiry = expiryInstantFor(expiration);
        const contract =
          row.contractSymbol ??
          occSymbol(symbol, new Date(expiration * 1000).toISOString().slice(0, 10), type, strike);
        if (!contract) continue;

        quotes.push({
          contract,
          type,
          strike,
          expiry,
          bid: positive(row.bid),
          ask: positive(row.ask),
          last: positive(row.lastPrice),
          mark: null,
          change: finite(row.change),
          volume: finite(row.volume),
          openInterest: finite(row.openInterest),
          venueIv: null,
          venueForward: null,
          venueDiscount: null,
        });
      }
    }
  }

  const board: OptionBoard = {
    symbol: (result.underlyingSymbol ?? symbol).toUpperCase(),
    name: quote.longName ?? quote.shortName ?? symbol.toUpperCase(),
    assetClass: 'stock',
    currency: quote.currency ?? 'USD',
    venue: 'OPRA',
    source: 'yahoo',
    contractSize: CONTRACT_SIZE,
    spot: positive(quote.regularMarketPrice),
    quotes,
  };

  return { board, expiries: result.expirationDates ?? [] };
}

/** 16:00 America/New_York on the day a Yahoo expiry timestamp falls on. */
function expiryInstantFor(epochSeconds: number): number {
  const iso = new Date(epochSeconds * 1000).toISOString().slice(0, 10);
  return expiryInstant(iso) ?? epochSeconds;
}

/* ------------------------------------------------------------------ nasdaq */

interface NasdaqChainRow {
  expirygroup?: string | null;
  expiryDate?: string | null;
  strike?: string | null;
  c_Last?: string | null;
  c_Change?: string | null;
  c_Bid?: string | null;
  c_Ask?: string | null;
  c_Volume?: string | null;
  c_Openinterest?: string | null;
  p_Last?: string | null;
  p_Change?: string | null;
  p_Bid?: string | null;
  p_Ask?: string | null;
  p_Volume?: string | null;
  p_Openinterest?: string | null;
}

interface NasdaqChainResponse {
  data?: {
    totalRecord?: number;
    lastTrade?: string | null;
    table?: { rows?: NasdaqChainRow[] | null } | null;
  } | null;
  status?: { rCode?: number };
}

async function nasdaq<T>(path: string, ttlMs: number): Promise<T> {
  return cache.cached(`nasdaq:${path}`, ttlMs, () =>
    fetchJson<T>(`${NASDAQ_BASE}${path}`, {
      timeoutMs: 25_000,
      retries: 1,
      headers: { 'User-Agent': BROWSER_UA },
    }),
  );
}

/** `"September 18, 2026"` → `"2026-09-18"`. */
export function parseExpiryGroup(label: string | null | undefined): string | null {
  if (!label) return null;
  const match = /^([A-Za-z]+)\s+(\d{1,2}),\s*(\d{4})$/.exec(label.trim());
  if (!match) return null;
  const [, month, day, year] = match;
  const index = MONTHS.indexOf(month!.toLowerCase());
  if (index === -1) return null;
  return `${year}-${String(index + 1).padStart(2, '0')}-${day!.padStart(2, '0')}`;
}

/** `"LAST TRADE: $305.93 (AS OF AUG 13, 2026)"` → `305.93`. */
export function parseLastTrade(raw: string | null | undefined): number | null {
  if (!raw) return null;
  const match = /\$\s*([\d,]+\.?\d*)/.exec(raw);
  return match ? parseNasdaqNumber(match[1]) : null;
}

/**
 * Nasdaq's flat table → board quotes.
 *
 * The table interleaves group-header rows (`expirygroup` set, everything else
 * null) with data rows that carry only an abbreviated `expiryDate` like
 * `"Sep 18"` — no year. The header is therefore the *only* place the year
 * exists, so parsing statefully is not a shortcut here, it is the only correct
 * reading. A data row before any header is dropped rather than guessed at.
 */
export function parseNasdaqChain(
  body: NasdaqChainResponse,
  symbol: string,
): { quotes: BoardQuote[]; dates: string[]; spot: number | null } {
  const rows = body.data?.table?.rows ?? [];
  const quotes: BoardQuote[] = [];
  const dates: string[] = [];
  let currentDate: string | null = null;

  for (const row of rows) {
    const group = parseExpiryGroup(row.expirygroup);
    if (group) {
      currentDate = group;
      if (!dates.includes(group)) dates.push(group);
      continue;
    }

    const strike = parseNasdaqNumber(row.strike);
    if (currentDate === null || strike === null || strike <= 0) continue;

    const expiry = expiryInstant(currentDate);
    if (expiry === null) continue;

    const legs: [
      'call' | 'put',
      { last: string | null | undefined; change: string | null | undefined; bid: string | null | undefined; ask: string | null | undefined; volume: string | null | undefined; oi: string | null | undefined },
    ][] = [
      [
        'call',
        { last: row.c_Last, change: row.c_Change, bid: row.c_Bid, ask: row.c_Ask, volume: row.c_Volume, oi: row.c_Openinterest },
      ],
      [
        'put',
        { last: row.p_Last, change: row.p_Change, bid: row.p_Bid, ask: row.p_Ask, volume: row.p_Volume, oi: row.p_Openinterest },
      ],
    ];

    for (const [type, leg] of legs) {
      const contract = occSymbol(symbol, currentDate, type, strike);
      if (!contract) continue;
      quotes.push({
        contract,
        type,
        strike,
        expiry,
        bid: positive(parseNasdaqNumber(leg.bid)),
        ask: positive(parseNasdaqNumber(leg.ask)),
        last: positive(parseNasdaqNumber(leg.last)),
        mark: null,
        change: finite(parseNasdaqNumber(leg.change)),
        volume: finite(parseNasdaqNumber(leg.volume)),
        openInterest: finite(parseNasdaqNumber(leg.oi)),
        venueIv: null,
        venueForward: null,
        venueDiscount: null,
      });
    }
  }

  return { quotes, dates, spot: parseLastTrade(body.data?.lastTrade) };
}

/** Nasdaq needs the right `assetclass`; there is no lookup, so probe. */
async function nasdaqAssetClass(symbol: string): Promise<string> {
  const key = `nasdaq:optclass:${symbol}`;
  const known = cache.get<string>(key);
  if (known) return known;

  for (const assetClass of ['stocks', 'etf', 'index'] as const) {
    const body = await nasdaq<NasdaqChainResponse>(
      chainPath(symbol, assetClass, { limit: 10 }),
      TTL.meta,
    ).catch(() => null);
    if (body?.data?.table?.rows && body.data.table.rows.length > 0) {
      cache.set(key, assetClass, TTL.catalogue);
      return assetClass;
    }
  }

  throw new UpstreamError(`Nasdaq lists no option chain for ${symbol}`, {
    code: 'not_found',
    hint: 'Nasdaq covers US equities, ETFs and its own indices. Not every name has listed options.',
  });
}

function chainPath(
  symbol: string,
  assetClass: string,
  opts: { limit: number; offset?: number; from?: string; to?: string },
): string {
  const range = opts.from && opts.to ? `fromdate=${opts.from}&todate=${opts.to}` : 'fromdate=all';
  return (
    `/api/quote/${encodeURIComponent(symbol)}/option-chain?assetclass=${assetClass}` +
    `&limit=${opts.limit}&offset=${opts.offset ?? 0}&${range}` +
    `&excode=oprac&callput=callput&money=all&type=all`
  );
}

/**
 * Walk Nasdaq's paged table into one board.
 *
 * Nasdaq has no endpoint that lists expiries, so the crawl is how the strip
 * gets built — but it also collects every quote on the way, which means the
 * volatility term structure comes free instead of costing one request per
 * expiry. The page cap is a bound on a very wide name (SPY lists tens of
 * thousands of contracts); when it bites, the response says so rather than
 * presenting a truncated board as the whole one.
 */
async function nasdaqBoard(symbol: string): Promise<OptionBoard> {
  const assetClass = await nasdaqAssetClass(symbol);

  const quotes: BoardQuote[] = [];
  const seen = new Set<string>();
  let spot: number | null = null;
  let truncated = false;
  let total = 0;

  for (let page = 0; page < NASDAQ_MAX_PAGES; page++) {
    const body = await nasdaq<NasdaqChainResponse>(
      chainPath(symbol, assetClass, { limit: NASDAQ_PAGE, offset: page * NASDAQ_PAGE }),
      TTL.quote,
    );

    const parsed = parseNasdaqChain(body, symbol);
    spot ??= parsed.spot;
    total = body.data?.totalRecord ?? total;

    for (const quote of parsed.quotes) {
      if (seen.has(quote.contract)) continue;
      seen.add(quote.contract);
      quotes.push(quote);
    }

    const returned = body.data?.table?.rows?.length ?? 0;
    if (returned < NASDAQ_PAGE) break;
    if (page === NASDAQ_MAX_PAGES - 1 && (page + 1) * NASDAQ_PAGE < total) truncated = true;
  }

  const board: OptionBoard = {
    symbol: symbol.toUpperCase(),
    name: symbol.toUpperCase(),
    assetClass: 'stock',
    currency: 'USD',
    venue: 'OPRA',
    source: 'nasdaq',
    contractSize: CONTRACT_SIZE,
    spot,
    quotes,
  };

  if (truncated) {
    board.note =
      `Nasdaq paginates the chain; this board is the first ` +
      `${NASDAQ_MAX_PAGES * NASDAQ_PAGE} of ${total} rows, so the longest-dated ` +
      `expiries are not shown.`;
  }

  return board;
}

/* ------------------------------------------------------------------ public */

/**
 * 16:00 America/New_York — when a US listed option actually stops trading.
 *
 * Memoised because resolving it goes through `Intl.formatToParts`, and a wide
 * chain asks the same question once per row for a few dozen distinct dates.
 */
const expiryInstants = new Map<string, number | null>();
function expiryInstant(isoDate: string): number | null {
  const hit = expiryInstants.get(isoDate);
  if (hit !== undefined) return hit;
  const value = equityExpiryInstant(isoDate);
  expiryInstants.set(isoDate, value);
  return value;
}

/**
 * The board for one underlying.
 *
 * `expiries` names which expiries to load, as `YYYY-MM-DD`. Yahoo serves one
 * expiry per request, so this bounds the fan-out; Nasdaq's crawl returns
 * everything at once and simply ignores it. An empty list means the front
 * expiry alone.
 */
export async function getBoard(
  symbol: string,
  expiries: string[] = [],
): Promise<{ board: OptionBoard; expiryDates: string[] }> {
  const upper = symbol.trim().toUpperCase();

  try {
    return await yahooBoard(upper, expiries);
  } catch (primaryError) {
    if (primaryError instanceof UpstreamError && primaryError.code === 'not_found') {
      throw primaryError;
    }

    try {
      const board = await nasdaqBoard(upper);
      if (board.spot === null) {
        board.spot = await getQuote(upper)
          .then((q) => q.price)
          .catch(() => null);
      }
      const dates = [...new Set(board.quotes.map((q) => isoOf(q.expiry)))].sort();
      return { board, expiryDates: dates };
    } catch (fallbackError) {
      if (fallbackError instanceof UpstreamError && fallbackError.code === 'not_found') {
        throw fallbackError;
      }
      const status = primaryError instanceof UpstreamError ? primaryError.status : undefined;
      throw new UpstreamError(`No provider could serve an option chain for ${upper}`, {
        code: 'upstream_error',
        hint:
          status === 429
            ? `Yahoo Finance is rate-limiting this IP — its \`v7\` option endpoint is ` +
              `blocked from shared datacentre addresses more aggressively than the price ` +
              `endpoint is — and the Nasdaq fallback did not answer either. The same ` +
              `request usually succeeds from a residential connection.`
            : `Yahoo Finance and Nasdaq both refused an option chain for ${upper}. ` +
              `Check the symbol, and note that not every listed name has options.`,
      });
    }
  }
}

async function yahooBoard(
  symbol: string,
  expiries: string[],
): Promise<{ board: OptionBoard; expiryDates: string[] }> {
  // The undated call returns the front expiry *and* the full expiry list, so it
  // is both the cheapest probe and the one that builds the picker.
  const first = parseYahooOptions(
    await yahoo<YahooOptionsResponse>(
      `/v7/finance/options/${encodeURIComponent(symbol)}`,
      TTL.quote,
    ),
    symbol,
  );

  const expiryDates = first.expiries.map((e) => new Date(e * 1000).toISOString().slice(0, 10));
  const byDate = new Map(first.expiries.map((e) => [new Date(e * 1000).toISOString().slice(0, 10), e]));

  const loaded = new Set(first.board.quotes.map((q) => isoOf(q.expiry)));
  const wanted = expiries.filter((date) => byDate.has(date) && !loaded.has(date));

  if (wanted.length > 0) {
    const extra = await Promise.all(
      wanted.map((date) =>
        yahoo<YahooOptionsResponse>(
          `/v7/finance/options/${encodeURIComponent(symbol)}?date=${byDate.get(date)}`,
          TTL.quote,
        )
          .then((body) => parseYahooOptions(body, symbol).board.quotes)
          .catch(() => [] as BoardQuote[]),
      ),
    );
    for (const quotes of extra) first.board.quotes.push(...quotes);
  }

  if (first.board.spot === null) {
    first.board.spot = await getQuote(symbol)
      .then((q) => q.price)
      .catch(() => null);
  }

  return { board: first.board, expiryDates };
}

function isoOf(expirySeconds: number): string {
  return new Date(expirySeconds * 1000).toISOString().slice(0, 10);
}

/** Equity options exist for far too many names to enumerate; this is a hint list. */
export function listUnderlyings(): OptionUnderlying[] {
  return [
    { symbol: 'SPY', name: 'SPDR S&P 500 ETF', assetClass: 'stock', venue: 'OPRA' },
    { symbol: 'QQQ', name: 'Invesco QQQ Trust', assetClass: 'stock', venue: 'OPRA' },
    { symbol: 'IWM', name: 'iShares Russell 2000 ETF', assetClass: 'stock', venue: 'OPRA' },
    { symbol: 'AAPL', name: 'Apple', assetClass: 'stock', venue: 'OPRA' },
    { symbol: 'NVDA', name: 'NVIDIA', assetClass: 'stock', venue: 'OPRA' },
    { symbol: 'TSLA', name: 'Tesla', assetClass: 'stock', venue: 'OPRA' },
    { symbol: 'MSFT', name: 'Microsoft', assetClass: 'stock', venue: 'OPRA' },
    { symbol: 'AMZN', name: 'Amazon', assetClass: 'stock', venue: 'OPRA' },
  ];
}
