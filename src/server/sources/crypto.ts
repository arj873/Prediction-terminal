/**
 * Crypto spot prices from Coinbase Exchange's public market-data API.
 *
 * Chosen over the alternatives for boring reasons that matter here: no key, no
 * account, no geo-fence, documented rate limits, and it answers from datacentre
 * IPs — which the equity feeds mostly do not (see `sources/stocks.ts`). It is
 * also the venue behind Kalshi's crypto settlement family (CF Benchmarks
 * indices track the same USD spot market), so the true price and the implied
 * price are describing the same thing.
 *
 * The one sharp edge is `/candles`: it caps a response at 300 buckets and
 * returns them *newest first*, so any window worth charting has to be walked
 * backwards in pages and reversed.
 */

import type { CandleInterval, SpotCandle, SpotCandlesResponse, SpotQuote } from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';

const BASE = process.env.COINBASE_API_BASE ?? 'https://api.exchange.coinbase.com';

/** Coinbase granularities, in seconds. These are the only accepted values. */
const GRANULARITY: Record<CandleInterval, number> = { 1: 60, 60: 3600, 1440: 86400 };

/** Hard cap Coinbase applies to one `/candles` response. */
const MAX_BUCKETS = 300;

/** Pages per request, so a 1-year daily window cannot fan out unboundedly. */
const MAX_PAGES = 12;

interface RawTicker {
  price?: string;
  bid?: string;
  ask?: string;
  volume?: string;
  time?: string;
}

interface RawStats {
  open?: string;
  high?: string;
  low?: string;
  last?: string;
  volume?: string;
}

interface RawProduct {
  id: string;
  base_currency?: string;
  quote_currency?: string;
  display_name?: string;
  status?: string;
  trading_disabled?: boolean;
}

/** Coinbase returns candles as positional arrays: [time, low, high, open, close, volume]. */
type RawCandle = [number, number, number, number, number, number];

function num(value: unknown): number | null {
  if (value === null || value === undefined || value === '') return null;
  const n = Number(value);
  return Number.isFinite(n) ? n : null;
}

/**
 * Symbol → Coinbase product id.
 *
 * `BTC` and `BTC-USD` both mean the same thing to a person typing into a
 * terminal, and only one of them means anything to Coinbase.
 */
export function productId(symbol: string): string {
  const upper = symbol.trim().toUpperCase();
  if (upper.includes('-')) return upper;
  return `${upper}-USD`;
}

/** The bare asset symbol, i.e. the inverse of {@link productId}. */
export function baseSymbol(symbol: string): string {
  return symbol.trim().toUpperCase().split('-')[0] ?? symbol;
}

async function get<T>(path: string, ttlMs: number): Promise<T> {
  return cache.cached(`coinbase:${path}`, ttlMs, () =>
    // Coinbase 403s a request with no User-Agent, so the browser-shaped header
    // set is not optional here even though this is a JSON API.
    fetchJson<T>(`${BASE}${path}`, {
      timeoutMs: 15_000,
      retries: 2,
      headers: {
        'User-Agent':
          'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36',
      },
    }),
  );
}

/** Tradeable USD-quoted products, for symbol search and validation. */
export async function listProducts(): Promise<RawProduct[]> {
  const products = await get<RawProduct[]>('/products', TTL.catalogue);
  return products.filter(
    (p) => p.status === 'online' && !p.trading_disabled && p.quote_currency === 'USD',
  );
}

export async function getQuote(symbol: string): Promise<SpotQuote> {
  const id = productId(symbol);

  // `/ticker` is the live print; `/stats` carries the 24h open/high/low that
  // makes a change figure meaningful. Neither contains the other.
  const [ticker, stats] = await Promise.all([
    get<RawTicker>(`/products/${encodeURIComponent(id)}/ticker`, TTL.quote).catch(
      (err: unknown) => {
        throw notFound(err, id);
      },
    ),
    get<RawStats>(`/products/${encodeURIComponent(id)}/stats`, TTL.quote).catch(() => ({}) as RawStats),
  ]);

  const price = num(ticker.price) ?? num(stats.last);
  // Crypto has no session close, so the 24h open is the honest comparison.
  const previousClose = num(stats.open);
  const change = price !== null && previousClose !== null ? price - previousClose : null;

  return {
    symbol: baseSymbol(id),
    assetClass: 'crypto',
    name: id.replace('-', ' / '),
    currency: id.split('-')[1] ?? 'USD',
    price,
    previousClose,
    change,
    changePercent:
      change !== null && previousClose !== null && previousClose !== 0
        ? (change / previousClose) * 100
        : null,
    dayOpen: num(stats.open),
    dayHigh: num(stats.high),
    dayLow: num(stats.low),
    volume: num(stats.volume),
    venue: 'Coinbase',
    time: ticker.time ? Math.floor(new Date(ticker.time).getTime() / 1000) : Math.floor(Date.now() / 1000),
    source: 'coinbase',
  };
}

/** A 404 from `/ticker` means the pair is not listed — say which pair. */
function notFound(err: unknown, id: string): unknown {
  if (err instanceof UpstreamError && (err.code === 'not_found' || err.status === 404)) {
    return new UpstreamError(`Coinbase does not list the pair ${id}`, {
      code: 'not_found',
      hint: `Try the base symbol on its own, e.g. \`CRY BTC\`, or a listed pair such as ${id.split('-')[0]}-USDC.`,
    });
  }
  return err;
}

export function normaliseCandles(raw: RawCandle[]): SpotCandle[] {
  const candles: SpotCandle[] = [];
  for (const row of raw) {
    if (!Array.isArray(row) || row.length < 6) continue;
    const [time, low, high, open, close, volume] = row;
    if (!Number.isFinite(time) || !Number.isFinite(close)) continue;
    candles.push({ time, open, high, low, close, volume: Number.isFinite(volume) ? volume : 0 });
  }
  return candles;
}

/**
 * Candles for `[startTs, endTs]`, walking backwards in 300-bucket pages.
 *
 * Coinbase silently truncates rather than paginating, so asking for a window
 * wider than 300 buckets and trusting the answer would quietly lose the older
 * end of every chart — the part a historical overlay is entirely about.
 */
export async function getCandles(
  symbol: string,
  interval: CandleInterval,
  startTs: number,
  endTs: number,
): Promise<SpotCandlesResponse> {
  const id = productId(symbol);
  const granularity = GRANULARITY[interval];
  const span = granularity * MAX_BUCKETS;

  const collected = new Map<number, SpotCandle>();
  let windowEnd = endTs;

  for (let page = 0; page < MAX_PAGES && windowEnd > startTs; page++) {
    const windowStart = Math.max(startTs, windowEnd - span);
    const path =
      `/products/${encodeURIComponent(id)}/candles` +
      `?granularity=${granularity}` +
      `&start=${new Date(windowStart * 1000).toISOString()}` +
      `&end=${new Date(windowEnd * 1000).toISOString()}`;

    const raw = await get<RawCandle[]>(path, TTL.candles).catch((err: unknown) => {
      throw notFound(err, id);
    });
    const page_ = normaliseCandles(Array.isArray(raw) ? raw : []);
    for (const candle of page_) collected.set(candle.time, candle);

    if (page_.length === 0) break;
    // Step to the bucket before the oldest one returned. Using the response's
    // own oldest timestamp rather than the requested start keeps the walk
    // correct across market gaps.
    const oldest = Math.min(...page_.map((c) => c.time));
    if (oldest <= startTs) break;
    windowEnd = oldest - granularity;
  }

  return {
    symbol: baseSymbol(id),
    assetClass: 'crypto',
    name: id.replace('-', ' / '),
    currency: id.split('-')[1] ?? 'USD',
    interval,
    candles: [...collected.values()].sort((a, b) => a.time - b.time),
    source: 'coinbase',
  };
}

/** Substring search over listed USD pairs. */
export async function search(query: string, limit = 20): Promise<RawProduct[]> {
  const needle = query.trim().toUpperCase();
  if (!needle) return [];
  const products = await listProducts();

  const scored = products
    .map((p) => {
      const base = p.base_currency ?? p.id.split('-')[0] ?? '';
      let score = 0;
      if (base === needle) score = 100;
      else if (base.startsWith(needle)) score = 60;
      else if (base.includes(needle)) score = 30;
      else if ((p.display_name ?? '').toUpperCase().includes(needle)) score = 10;
      return { product: p, score };
    })
    .filter((s) => s.score > 0)
    .sort((a, b) => b.score - a.score || a.product.id.localeCompare(b.product.id));

  return scored.slice(0, limit).map((s) => s.product);
}
