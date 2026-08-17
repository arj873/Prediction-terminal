/**
 * Polygon.io — a licensed consolidated tape, when a deployment has a key.
 *
 * This is not a twelfth thing to browse; it is a better answer to a question
 * the terminal already asks. `STK`, `CRY` and the true-price half of `IMP` go
 * through a provider chain, and Polygon slots in *ahead* of Yahoo and Nasdaq
 * whenever `POLYGON_API_KEY` is set, because it fixes the two failures those
 * two actually have:
 *
 *  - Yahoo returns 429 to shared datacentre addresses, which is exactly where a
 *    hosted deployment of this terminal runs.
 *  - Nasdaq answers from those addresses but has no S&P 500 or Dow, and Kalshi's
 *    index ladders settle on the index rather than on an ETF tracking it.
 *
 * Polygon has neither problem: it is authenticated rather than IP-reputation
 * based, and `I:SPX` is the index itself. Without a key the chain is exactly
 * what it was, so this file changes nothing for a deployment that has none.
 *
 * A caveat the terminal states rather than hides: Polygon's free tier serves
 * end-of-day data with a 15-minute delay and no intraday aggregates. A `1m`
 * chart on a free key therefore returns nothing, and that reads as an empty
 * chart unless it is named — so it is.
 */

import type {
  CandleInterval,
  SpotCandle,
  SpotQuote,
  SpotSearchResult,
} from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';

const BASE = process.env.POLYGON_API_BASE ?? 'https://api.polygon.io';

export const apiKey = (): string | undefined => process.env.POLYGON_API_KEY?.trim() || undefined;

export function hasCredentials(): boolean {
  return apiKey() !== undefined;
}

/* --------------------------------------------------------------- symbols */

/**
 * Cash indices, as this terminal names them and as Polygon does.
 *
 * The rest of the terminal speaks Yahoo's caret convention because Yahoo is the
 * primary equity provider. Polygon uses an `I:` prefix and its own root, so the
 * handful that differ are stated here rather than guessed — a wrong guess would
 * quote an unrelated instrument rather than fail.
 */
const INDEX_SYMBOLS: Record<string, string> = {
  '^GSPC': 'I:SPX',
  SPX: 'I:SPX',
  '^NDX': 'I:NDX',
  NDX: 'I:NDX',
  '^IXIC': 'I:COMP',
  COMP: 'I:COMP',
  '^DJI': 'I:DJI',
  DJI: 'I:DJI',
  '^RUT': 'I:RUT',
  RUT: 'I:RUT',
  '^VIX': 'I:VIX',
  VIX: 'I:VIX',
};

/** Crypto pairs are `X:BTCUSD`; the terminal writes them `BTC-USD`. */
export function polygonSymbol(symbol: string, assetClass: 'stock' | 'crypto' = 'stock'): string {
  const upper = symbol.trim().toUpperCase();

  if (assetClass === 'crypto') {
    if (upper.startsWith('X:')) return upper;
    const [base, quote] = upper.split(/[-/]/);
    return `X:${base}${quote ?? 'USD'}`;
  }

  const index = INDEX_SYMBOLS[upper];
  if (index) return index;
  // An unmapped caret symbol is still an index; Polygon spells it without one.
  return upper.startsWith('^') ? `I:${upper.slice(1)}` : upper;
}

/** Kalshi's candle periods as Polygon's multiplier/timespan pair. */
const TIMESPAN: Record<CandleInterval, { multiplier: number; timespan: string }> = {
  1: { multiplier: 1, timespan: 'minute' },
  60: { multiplier: 1, timespan: 'hour' },
  1440: { multiplier: 1, timespan: 'day' },
};

/* ----------------------------------------------------------------- fetch */

interface Aggregate {
  /** Bar start, unix milliseconds. */
  t?: number;
  o?: number;
  h?: number;
  l?: number;
  c?: number;
  v?: number;
}

interface AggregatesResponse {
  ticker?: string;
  status?: string;
  resultsCount?: number;
  results?: Aggregate[];
  error?: string;
  message?: string;
}

interface TickerDetails {
  results?: {
    ticker?: string;
    name?: string;
    market?: string;
    primary_exchange?: string;
    currency_name?: string;
    locale?: string;
  };
}

interface TickerSearchResponse {
  results?: {
    ticker?: string;
    name?: string;
    market?: string;
    primary_exchange?: string;
  }[];
}

function requireKey(): string {
  const key = apiKey();
  if (!key) {
    throw new UpstreamError('Polygon.io needs an API key, and none is set', {
      code: 'not_configured',
      hint: 'Set POLYGON_API_KEY. Keys are free from https://polygon.io/dashboard/api-keys.',
    });
  }
  return key;
}

async function get<T>(path: string, params: Record<string, string>, ttlMs: number): Promise<T> {
  const url = new URL(`${BASE}${path}`);
  for (const [k, v] of Object.entries(params)) url.searchParams.set(k, v);

  // The key travels as a header, so it never reaches the cache key or a log.
  const cacheKey = `polygon:${url.pathname}${url.search}`;

  return cache.cached(cacheKey, ttlMs, async () => {
    try {
      return await fetchJson<T>(url.toString(), {
        timeoutMs: 25_000,
        retries: 1,
        headers: { Authorization: `Bearer ${requireKey()}` },
      });
    } catch (err) {
      throw annotate(err);
    }
  });
}

/**
 * Say which of Polygon's two refusals this is.
 *
 * A free key answers 403 for anything intraday or real-time and 429 above five
 * requests a minute. Both are plan limits rather than outages, and both are
 * indistinguishable from a broken symbol unless named.
 */
function annotate(err: unknown): unknown {
  if (!(err instanceof UpstreamError)) return err;

  if (err.status === 403) {
    return new UpstreamError('Polygon.io declined this request on the current plan', {
      code: 'not_configured',
      status: 403,
      hint:
        'A free Polygon key covers end-of-day aggregates only. Intraday bars (`1m`, `1h`) ' +
        'and real-time quotes need a paid plan; daily bars (`1d`) work on every plan.',
    });
  }

  if (err.status === 429) {
    return new UpstreamError('Polygon.io is rate-limiting this key', {
      code: 'rate_limited',
      status: 429,
      hint: 'The free plan allows five requests a minute. Wait a moment, or upgrade the key.',
    });
  }

  return err;
}

/* --------------------------------------------------------------- candles */

const iso = (ts: number): string => new Date(ts * 1000).toISOString().slice(0, 10);

export async function getCandles(
  symbol: string,
  interval: CandleInterval,
  startTs: number,
  endTs: number,
  assetClass: 'stock' | 'crypto' = 'stock',
): Promise<{ candles: SpotCandle[]; name: string; currency: string }> {
  const ticker = polygonSymbol(symbol, assetClass);
  const { multiplier, timespan } = TIMESPAN[interval];

  const [bars, details] = await Promise.all([
    get<AggregatesResponse>(
      `/v2/aggs/ticker/${encodeURIComponent(ticker)}/range/${multiplier}/${timespan}/${iso(startTs)}/${iso(endTs)}`,
      { adjusted: 'true', sort: 'asc', limit: '50000' },
      interval === 1440 ? TTL.candles : TTL.quote,
    ),
    describe(ticker).catch(() => null),
  ]);

  const candles: SpotCandle[] = [];
  for (const bar of bars.results ?? []) {
    const close = bar.c;
    if (bar.t === undefined || close === undefined || !Number.isFinite(close)) continue;
    const open = bar.o ?? close;
    candles.push({
      // Polygon timestamps a bar at its start, in milliseconds; the terminal's
      // spot candles are unix seconds at the bar start, so this is a divide and
      // not an offset.
      time: Math.floor(bar.t / 1000),
      open,
      high: bar.h ?? Math.max(open, close),
      low: bar.l ?? Math.min(open, close),
      close,
      volume: bar.v ?? 0,
    });
  }

  if (candles.length === 0) {
    throw new UpstreamError(`Polygon.io returned no ${timespan} bars for ${ticker}`, {
      code: 'empty_upstream',
      hint:
        interval === 1440
          ? `Check the symbol — indices need their Polygon form, e.g. I:SPX.`
          : `A free Polygon key serves no intraday history. Retry with \`1d\`.`,
    });
  }

  return {
    candles,
    name: details?.name ?? symbol.toUpperCase(),
    currency: details?.currency ?? 'USD',
  };
}

async function describe(ticker: string): Promise<{ name: string; currency: string; venue: string }> {
  const payload = await get<TickerDetails>(
    `/v3/reference/tickers/${encodeURIComponent(ticker)}`,
    {},
    TTL.catalogue,
  );
  const r = payload.results ?? {};
  return {
    name: r.name ?? ticker,
    currency: (r.currency_name ?? 'usd').toUpperCase(),
    venue: r.primary_exchange ?? r.market ?? 'Polygon',
  };
}

/* ----------------------------------------------------------------- quote */

/**
 * A quote, built from the last two daily bars.
 *
 * Polygon's snapshot endpoint is the natural source and is not on the free
 * plan, so this derives the same figures from aggregates, which every plan
 * serves. The previous close is the bar before the last one — the same rule the
 * Yahoo provider settled on, and for the same reason: it is right whether or
 * not the session is still open.
 */
export async function getQuote(
  symbol: string,
  assetClass: 'stock' | 'crypto' = 'stock',
): Promise<SpotQuote> {
  const now = Math.floor(Date.now() / 1000);
  const [{ candles }, details] = await Promise.all([
    getCandles(symbol, 1440, now - 14 * 86400, now, assetClass),
    describe(polygonSymbol(symbol, assetClass)).catch(() => null),
  ]);

  const last = candles.at(-1);
  const previous = candles.at(-2);
  const price = last?.close ?? null;
  const previousClose = previous?.close ?? null;
  const change = price !== null && previousClose !== null ? price - previousClose : null;

  return {
    symbol: symbol.toUpperCase(),
    assetClass,
    name: details?.name ?? symbol.toUpperCase(),
    currency: details?.currency ?? 'USD',
    price,
    previousClose,
    change,
    changePercent:
      change !== null && previousClose !== null && previousClose !== 0
        ? (change / previousClose) * 100
        : null,
    dayOpen: last?.open ?? null,
    dayHigh: last?.high ?? null,
    dayLow: last?.low ?? null,
    volume: last?.volume ?? null,
    venue: details?.venue ?? 'Polygon',
    time: last?.time ?? now,
    source: 'polygon',
  };
}

/* ---------------------------------------------------------------- search */

export async function search(query: string, limit = 20): Promise<SpotSearchResult[]> {
  const payload = await get<TickerSearchResponse>(
    '/v3/reference/tickers',
    { search: query, active: 'true', limit: String(Math.min(Math.max(limit, 1), 100)) },
    TTL.meta,
  );

  return (payload.results ?? [])
    .filter((r) => r.ticker)
    .map((r) => ({
      symbol: r.ticker!,
      name: r.name ?? r.ticker!,
      assetClass: (r.market === 'crypto' ? 'crypto' : 'stock') as 'stock' | 'crypto',
      venue: r.primary_exchange ?? r.market ?? 'Polygon',
      hasImplied: false,
    }));
}
