/**
 * Equity, ETF and cash-index prices.
 *
 * Two providers, tried in order, for the same reason FRED has two:
 *
 * **Yahoo Finance** (`query1.finance.yahoo.com/v8/finance/chart`) is the
 * primary. It is the only free, keyless source here that covers cash indices —
 * `^GSPC`, `^NDX`, `^DJI` — and Kalshi's index ladders settle on the index, not
 * on an ETF tracking it. Quoting SPY against a `KXINX` ladder would be off by a
 * factor of ten, so this matters more than convenience.
 *
 * **Nasdaq** (`api.nasdaq.com`) is the fallback. It answers from datacentre
 * IPs, where Yahoo's API hosts return 429 to a shared address — the same
 * hosted-deployment failure the FRED scrape hits. It covers equities, ETFs and
 * Nasdaq's own indices (`COMP`, `NDX`) but *not* the S&P 500 or the Dow, so it
 * narrows coverage rather than replacing it. The terminal says which provider
 * answered rather than leaving that invisible.
 */

import type {
  CandleInterval,
  SpotCandle,
  SpotCandlesResponse,
  SpotQuote,
  SpotSearchResult,
} from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';
import { firstAnswer, type ChainOptions, type Provider } from '../lib/providers.js';

const YAHOO_BASE = process.env.YAHOO_API_BASE ?? 'https://query1.finance.yahoo.com';
const NASDAQ_BASE = process.env.NASDAQ_API_BASE ?? 'https://api.nasdaq.com';

const BROWSER_UA =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36';

/* --------------------------------------------------------------- intervals */

/** Kalshi's candle periods expressed the way each provider spells them. */
const YAHOO_INTERVAL: Record<CandleInterval, string> = { 1: '1m', 60: '1h', 1440: '1d' };

/**
 * How far back each interval can actually be requested.
 *
 * Yahoo enforces these server-side and answers a too-wide window with an error
 * rather than a truncated series, so the clamp has to happen before the call.
 */
const MAX_LOOKBACK: Record<CandleInterval, number> = {
  1: 7 * 86400,
  60: 729 * 86400,
  1440: 40 * 365 * 86400,
};

/* ------------------------------------------------------------------ yahoo */

interface YahooChartResponse {
  chart?: {
    result?: {
      meta?: {
        currency?: string;
        symbol?: string;
        exchangeName?: string;
        fullExchangeName?: string;
        instrumentType?: string;
        regularMarketPrice?: number;
        previousClose?: number;
        chartPreviousClose?: number;
        regularMarketDayHigh?: number;
        regularMarketDayLow?: number;
        regularMarketVolume?: number;
        regularMarketTime?: number;
        longName?: string;
        shortName?: string;
      };
      timestamp?: number[];
      indicators?: {
        quote?: {
          open?: (number | null)[];
          high?: (number | null)[];
          low?: (number | null)[];
          close?: (number | null)[];
          volume?: (number | null)[];
        }[];
      };
    }[];
    error?: { code?: string; description?: string } | null;
  };
}

async function yahoo<T>(path: string, ttlMs: number): Promise<T> {
  return cache.cached(`yahoo:${path}`, ttlMs, () =>
    fetchJson<T>(`${YAHOO_BASE}${path}`, {
      timeoutMs: 15_000,
      retries: 1,
      headers: { 'User-Agent': BROWSER_UA },
    }),
  );
}

/**
 * Yahoo's chart payload → candles plus a quote, in one call.
 *
 * Exported for tests, and because both {@link getQuote} and
 * {@link getCandles} want different halves of the same response — fetching it
 * once and splitting it is cheaper than two endpoints.
 */
export function parseYahooChart(
  body: YahooChartResponse,
  symbol: string,
  interval: CandleInterval = 1440,
): { quote: SpotQuote; candles: SpotCandle[] } {
  const result = body.chart?.result?.[0];
  if (!result) {
    const description = body.chart?.error?.description;
    throw new UpstreamError(
      description ?? `Yahoo Finance returned no data for ${symbol}`,
      {
        code: 'not_found',
        hint: `Check the symbol. Cash indices need their caret form, e.g. \`^GSPC\` for the S&P 500.`,
      },
    );
  }

  const meta = result.meta ?? {};
  const quoteRows = result.indicators?.quote?.[0] ?? {};
  const times = result.timestamp ?? [];

  const candles: SpotCandle[] = [];
  for (let i = 0; i < times.length; i++) {
    const close = quoteRows.close?.[i];
    const time = times[i];
    // Yahoo pads its arrays with nulls for halted or not-yet-printed buckets.
    // A bar with no close is not a bar.
    if (time === undefined || close === null || close === undefined || !Number.isFinite(close)) {
      continue;
    }
    const open = pick(quoteRows.open?.[i], close);
    const high = pick(quoteRows.high?.[i], Math.max(open, close));
    const low = pick(quoteRows.low?.[i], Math.min(open, close));
    candles.push({
      time,
      open,
      high,
      low,
      close,
      volume: pick(quoteRows.volume?.[i], 0),
    });
  }

  const last = candles[candles.length - 1];
  const price = meta.regularMarketPrice ?? last?.close ?? null;
  const previousClose = previousCloseOf(meta, candles, interval);
  const change = price !== null && previousClose !== null ? price - previousClose : null;

  const quote: SpotQuote = {
    symbol: meta.symbol ?? symbol,
    assetClass: 'stock',
    name: meta.longName ?? meta.shortName ?? meta.symbol ?? symbol,
    currency: meta.currency ?? 'USD',
    price,
    previousClose,
    change,
    changePercent:
      change !== null && previousClose !== null && previousClose !== 0
        ? (change / previousClose) * 100
        : null,
    // The final bar is the current session, so its open is today's open. The
    // *first* bar's open is the start of the requested window, which is only
    // the same thing when the window happens to be one day long.
    dayOpen: last?.open ?? null,
    dayHigh: meta.regularMarketDayHigh ?? null,
    dayLow: meta.regularMarketDayLow ?? null,
    volume: meta.regularMarketVolume ?? null,
    venue: meta.fullExchangeName ?? meta.exchangeName ?? '',
    time: meta.regularMarketTime ?? last?.time ?? Math.floor(Date.now() / 1000),
    source: 'yahoo',
  };

  return { quote, candles: candles.sort((a, b) => a.time - b.time) };
}

function pick(value: number | null | undefined, fallback: number): number {
  return value === null || value === undefined || !Number.isFinite(value) ? fallback : value;
}

/**
 * The prior session's close, for the day-change figure.
 *
 * Yahoo's chart meta usually omits `previousClose` entirely and offers
 * `chartPreviousClose`, which is the close before the *requested window* — over
 * a week-long range that is last week's number. Trusting it printed AAPL as
 * -2.4% on a day it closed +0.2%.
 *
 * On a daily series the answer is sitting in the data: the bar before the last
 * one. That holds whether the market is open (last bar is today, forming) or
 * shut (last bar is the most recent session), which is exactly the ambiguity a
 * meta field would have to resolve anyway.
 */
function previousCloseOf(
  meta: { previousClose?: number; chartPreviousClose?: number },
  candles: SpotCandle[],
  interval: CandleInterval,
): number | null {
  if (meta.previousClose !== undefined && Number.isFinite(meta.previousClose)) {
    return meta.previousClose;
  }
  if (interval === 1440 && candles.length >= 2) {
    return candles[candles.length - 2]!.close;
  }
  return meta.chartPreviousClose ?? null;
}

async function yahooChart(
  symbol: string,
  interval: CandleInterval,
  startTs: number,
  endTs: number,
): Promise<{ quote: SpotQuote; candles: SpotCandle[] }> {
  const from = Math.max(startTs, endTs - MAX_LOOKBACK[interval]);
  const path =
    `/v8/finance/chart/${encodeURIComponent(symbol)}` +
    `?period1=${Math.floor(from)}&period2=${Math.floor(endTs)}` +
    `&interval=${YAHOO_INTERVAL[interval]}&includePrePost=false&events=div%2Csplit`;

  return parseYahooChart(await yahoo<YahooChartResponse>(path, TTL.candles), symbol, interval);
}

/* ----------------------------------------------------------------- nasdaq */

interface NasdaqEnvelope<T> {
  data?: T | null;
  status?: { rCode?: number; bCodeMessage?: { errorMessage?: string }[] | null };
}

interface NasdaqInfo {
  symbol?: string;
  companyName?: string;
  exchange?: string;
  primaryData?: {
    lastSalePrice?: string;
    netChange?: string;
    percentageChange?: string;
    volume?: string;
    lastTradeTimestamp?: string;
  };
  keyStats?: { PreviousClose?: { value?: string }; OpenPrice?: { value?: string } };
}

interface NasdaqHistorical {
  tradesTable?: {
    rows?: { date?: string; close?: string; volume?: string; open?: string; high?: string; low?: string }[];
  };
}

/** Nasdaq requires the right `assetclass`, and answers 400 for a wrong guess. */
const ASSET_CLASSES = ['stocks', 'etf', 'index'] as const;

/**
 * The TTL is part of the key.
 *
 * `cache.cached` keys on the string alone, so whoever writes an entry fixes its
 * lifetime for everyone reading it. Two callers ask for the same Nasdaq URL
 * with different freshness needs — `resolveAssetClass` at TTL.meta (60s) and
 * `getQuote` at TTL.quote (3s) — and because the quote path calls the resolver
 * first, the 60s entry was always written first and the quote then served up to
 * twenty times staler than it asked for.
 */
async function nasdaq<T>(path: string, ttlMs: number): Promise<NasdaqEnvelope<T>> {
  return cache.cached(`nasdaq:${ttlMs}:${path}`, ttlMs, () =>
    fetchJson<NasdaqEnvelope<T>>(`${NASDAQ_BASE}${path}`, {
      timeoutMs: 15_000,
      retries: 1,
      headers: { 'User-Agent': BROWSER_UA },
    }),
  );
}

/** Nasdaq has no concept of a caret-prefixed index symbol. */
function nasdaqSymbol(symbol: string): string {
  return symbol.replace(/^\^/, '').toUpperCase();
}

/**
 * Find which `assetclass` Nasdaq files this symbol under.
 *
 * There is no lookup endpoint for it, so this probes the three in turn. The
 * answer is cached for the session because a symbol does not change class.
 */
async function resolveAssetClass(symbol: string): Promise<string> {
  const key = `nasdaq:class:${symbol}`;
  const known = cache.get<string>(key);
  if (known) return known;

  for (const assetClass of ASSET_CLASSES) {
    const body = await nasdaq<NasdaqInfo>(
      `/api/quote/${encodeURIComponent(symbol)}/info?assetclass=${assetClass}`,
      TTL.meta,
    ).catch(() => null);
    if (body?.status?.rCode === 200 && body.data) {
      cache.set(key, assetClass, TTL.catalogue);
      return assetClass;
    }
  }

  throw new UpstreamError(`Nasdaq does not list ${symbol}`, {
    code: 'not_found',
    hint:
      `Nasdaq covers US equities, ETFs and its own indices (COMP, NDX). It has no ` +
      `S&P 500 or Dow index feed — those need Yahoo, which is the primary source.`,
  });
}

/** `"$305.93"` → `305.93`; `"28,229,611"` → `28229611`; `"N/A"` → `null`. */
export function parseNasdaqNumber(raw: string | undefined | null): number | null {
  if (!raw) return null;
  const cleaned = raw.replace(/[$,%\s,]/g, '').replace(/,/g, '');
  if (!cleaned || cleaned === 'N/A' || cleaned === '--') return null;
  const value = Number(cleaned);
  return Number.isFinite(value) ? value : null;
}

/** Nasdaq dates the rows `MM/DD/YYYY`; charts want unix seconds at UTC midnight. */
export function parseNasdaqDate(raw: string | undefined): number | null {
  if (!raw) return null;
  const match = /^(\d{2})\/(\d{2})\/(\d{4})$/.exec(raw.trim());
  if (!match) return null;
  const [, month, day, year] = match;
  return Math.floor(Date.UTC(Number(year), Number(month) - 1, Number(day)) / 1000);
}

export function parseNasdaqHistorical(body: NasdaqEnvelope<NasdaqHistorical>): SpotCandle[] {
  const rows = body.data?.tradesTable?.rows ?? [];
  const candles: SpotCandle[] = [];

  for (const row of rows) {
    const time = parseNasdaqDate(row.date);
    const close = parseNasdaqNumber(row.close);
    if (time === null || close === null) continue;
    const open = parseNasdaqNumber(row.open) ?? close;
    candles.push({
      time,
      open,
      high: parseNasdaqNumber(row.high) ?? Math.max(open, close),
      low: parseNasdaqNumber(row.low) ?? Math.min(open, close),
      close,
      volume: parseNasdaqNumber(row.volume) ?? 0,
    });
  }

  // Nasdaq returns newest first.
  return candles.sort((a, b) => a.time - b.time);
}

function nasdaqQuote(body: NasdaqEnvelope<NasdaqInfo>, symbol: string): SpotQuote {
  const data = body.data ?? {};
  const primary = data.primaryData ?? {};
  const price = parseNasdaqNumber(primary.lastSalePrice);
  const previousClose = parseNasdaqNumber(data.keyStats?.PreviousClose?.value);
  const change = parseNasdaqNumber(primary.netChange);

  return {
    symbol: data.symbol ?? symbol,
    assetClass: 'stock',
    name: data.companyName ?? symbol,
    currency: 'USD',
    price,
    previousClose: previousClose ?? (price !== null && change !== null ? price - change : null),
    change,
    changePercent: parseNasdaqNumber(primary.percentageChange),
    dayOpen: parseNasdaqNumber(data.keyStats?.OpenPrice?.value),
    dayHigh: null,
    dayLow: null,
    volume: parseNasdaqNumber(primary.volume),
    venue: data.exchange ?? 'Nasdaq',
    time: Math.floor(Date.now() / 1000),
    source: 'nasdaq',
  };
}

async function nasdaqCandles(
  symbol: string,
  interval: CandleInterval,
  startTs: number,
  endTs: number,
): Promise<SpotCandle[]> {
  const plain = nasdaqSymbol(symbol);
  const assetClass = await resolveAssetClass(plain);

  // Nasdaq only publishes daily bars historically; its intraday endpoint covers
  // the current session alone. Serving daily bars for an intraday request would
  // silently answer a different question, so this reports the gap instead.
  if (interval !== 1440) {
    throw new UpstreamError(
      `Nasdaq has no ${interval === 1 ? '1-minute' : 'hourly'} history for ${plain}`,
      {
        code: 'unsupported',
        hint:
          `The Nasdaq fallback only carries daily bars. Retry with \`1d\`, or run the ` +
          `terminal from a network Yahoo Finance answers, which has intraday history.`,
      },
    );
  }

  const iso = (ts: number): string => new Date(ts * 1000).toISOString().slice(0, 10);
  const body = await nasdaq<NasdaqHistorical>(
    `/api/quote/${encodeURIComponent(plain)}/historical` +
      `?assetclass=${assetClass}&fromdate=${iso(startTs)}&todate=${iso(endTs)}&limit=9999`,
    TTL.candles,
  );
  return parseNasdaqHistorical(body);
}

/* ---------------------------------------------------------------- public */

/**
 * Try Yahoo, fall back to Nasdaq, and make the failure legible if both fail.
 *
 * A 429 from Yahoo is the signature of a shared datacentre IP rather than a bad
 * symbol, and it is worth naming: it is the difference between "your symbol is
 * wrong" and "this host is blocking your network".
 */
/**
 * Yahoo first, Nasdaq behind it.
 *
 * A 429 from Yahoo is the signature of a shared datacentre IP rather than a bad
 * symbol, and it is worth naming: it is the difference between "your symbol is
 * wrong" and "this host is blocking your network".
 */
function priceProviders<T>(
  symbol: string,
  viaYahoo: () => Promise<T>,
  viaNasdaq: () => Promise<T>,
): { providers: Provider<T>[]; options: ChainOptions } {
  return {
    providers: [
      { id: 'yahoo', label: 'Yahoo Finance', run: viaYahoo },
      { id: 'nasdaq', label: 'Nasdaq', run: viaNasdaq },
    ],
    options: {
      what: `a price for ${symbol}`,
      logPrefix: 'stocks',
      onExhausted: (failures) => {
        const primary = failures.find((f) => f.id === 'yahoo')?.error;
        const status = primary instanceof UpstreamError ? primary.status : undefined;
        return new UpstreamError(`No price source could quote ${symbol}`, {
          code: 'upstream_error',
          hint:
            status === 429
              ? `Yahoo Finance is rate-limiting this IP — it does that to shared ` +
                `datacentre addresses — and the Nasdaq fallback does not cover ${symbol}. ` +
                `The same request usually succeeds from a residential connection.`
              : `Yahoo Finance and Nasdaq both refused ${symbol}. Check the symbol: ` +
                `cash indices need a caret, e.g. \`^GSPC\`.`,
        });
      },
    },
  };
}

export async function getQuote(symbol: string): Promise<SpotQuote> {
  const upper = symbol.trim().toUpperCase();
  const now = Math.floor(Date.now() / 1000);

  const { providers, options } = priceProviders<SpotQuote>(
    upper,
    async () => (await yahooChart(upper, 1440, now - 7 * 86400, now)).quote,
    async () => {
      const plain = nasdaqSymbol(upper);
      const assetClass = await resolveAssetClass(plain);
      return nasdaqQuote(
        await nasdaq<NasdaqInfo>(
          `/api/quote/${encodeURIComponent(plain)}/info?assetclass=${assetClass}`,
          TTL.quote,
        ),
        plain,
      );
    },
  );

  return (await firstAnswer(providers, options)).value;
}

export async function getCandles(
  symbol: string,
  interval: CandleInterval,
  startTs: number,
  endTs: number,
): Promise<SpotCandlesResponse> {
  const upper = symbol.trim().toUpperCase();

  const { providers, options } = priceProviders(
    upper,
    async () => {
      const chart = await yahooChart(upper, interval, startTs, endTs);
      return {
        candles: chart.candles,
        name: chart.quote.name,
        currency: chart.quote.currency,
      };
    },
    async () => ({
      candles: await nasdaqCandles(upper, interval, startTs, endTs),
      name: upper,
      currency: 'USD',
    }),
  );

  // `source` comes from whichever provider answered rather than from a literal
  // written beside each branch, so it cannot disagree with what actually ran.
  const { value: { candles, name, currency }, source } = await firstAnswer(providers, options);

  return {
    symbol: upper,
    assetClass: 'stock',
    name,
    currency,
    interval,
    candles: candles.filter((c) => c.time >= startTs && c.time <= endTs),
    source,
  };
}

interface YahooSearchResponse {
  quotes?: {
    symbol?: string;
    shortname?: string;
    longname?: string;
    exchDisp?: string;
    quoteType?: string;
    isYahooFinance?: boolean;
  }[];
}

export async function search(query: string, limit = 20): Promise<SpotSearchResult[]> {
  const body = await yahoo<YahooSearchResponse>(
    `/v1/finance/search?q=${encodeURIComponent(query)}&quotesCount=${limit}&newsCount=0`,
    TTL.meta,
  );

  return (body.quotes ?? [])
    .filter((q) => q.symbol && q.isYahooFinance !== false)
    .filter((q) => ['EQUITY', 'ETF', 'INDEX', 'MUTUALFUND'].includes(q.quoteType ?? 'EQUITY'))
    .slice(0, limit)
    .map((q) => ({
      symbol: q.symbol!,
      name: q.longname ?? q.shortname ?? q.symbol!,
      assetClass: 'stock' as const,
      venue: q.exchDisp ?? '',
      hasImplied: false,
    }));
}
