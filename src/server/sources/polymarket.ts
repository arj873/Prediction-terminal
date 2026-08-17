/**
 * Polymarket International (polymarket.com) client.
 *
 * Three public hosts, because one does not carry everything:
 *
 *   gamma-api    catalogue — events, markets, series, prices, volume
 *   clob         the live book, and the price history behind every chart
 *   data-api     the public print tape
 *
 * The dialect differs from Kalshi's in three ways worth knowing before reading
 * the normalisers. Prices arrive as JSON *numbers* but the arrays beside them
 * (`outcomes`, `outcomePrices`, `clobTokenIds`) arrive as JSON *strings* that
 * have to be parsed a second time. There is no NO book: a Polymarket market is
 * a pair of ERC-1155 tokens and the terminal quotes the first one, deriving the
 * NO side from it. And the CLOB identifies a market by a 77-digit token id,
 * never by the slug a human types — so anything touching the book or the chart
 * resolves the slug through the catalogue first.
 */

import type {
  Candle,
  CandleInterval,
  CandlesResponse,
  Market,
  MarketStatus,
  OrderBook,
  SeriesInfo,
  TradesResponse,
  VenueEvent,
} from '../../shared/types.js';
import { TTL, cache, catalogue } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';
import {
  rankMarkets,
  searchCorpus,
  type Corpus,
  type MoverSort,
  type SearchResponse,
} from './corpus.js';

const GAMMA = process.env.POLYMARKET_GAMMA_BASE ?? 'https://gamma-api.polymarket.com';
const CLOB = process.env.POLYMARKET_CLOB_BASE ?? 'https://clob.polymarket.com';
const DATA = process.env.POLYMARKET_DATA_BASE ?? 'https://data-api.polymarket.com';

const VENUE = 'polymarket' as const;

/* --------------------------------------------------------------- coercion */

function num(value: unknown): number | null {
  if (value === null || value === undefined || value === '') return null;
  const n = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(n) ? n : null;
}

/**
 * A quote of exactly 0 means "nothing rests on this side".
 *
 * Polymarket's minimum tick is 0.001, so a real resting order can never be at
 * zero, and gamma reports an empty side as `0`. A quote of exactly 1 is *not*
 * given the same treatment — it is a legitimate price for a contract the market
 * has already made up its mind about.
 */
function price(value: unknown): number | null {
  const n = num(value);
  return n === null || n === 0 ? null : n;
}

/** Kill binary-float dust from the `1 - p` inversions. */
function round4(n: number): number {
  return Math.round(n * 10_000) / 10_000;
}

/**
 * Parse one of gamma's stringified JSON arrays.
 *
 * `outcomes`, `outcomePrices` and `clobTokenIds` all arrive as strings holding
 * JSON — `"[\"Yes\", \"No\"]"` — rather than as arrays. Occasionally one is
 * already an array; both shapes are accepted so a fixed upstream would not
 * break this.
 */
export function parseStringArray(value: unknown): string[] {
  if (Array.isArray(value)) return value.map(String);
  if (typeof value !== 'string' || value === '') return [];
  try {
    const parsed: unknown = JSON.parse(value);
    return Array.isArray(parsed) ? parsed.map(String) : [];
  } catch {
    return [];
  }
}

/* ------------------------------------------------------------ raw upstream */

export interface RawGammaMarket {
  id?: string;
  slug?: string;
  question?: string;
  conditionId?: string;
  groupItemTitle?: string;
  description?: string;
  outcomes?: string;
  outcomePrices?: string;
  clobTokenIds?: string;
  bestBid?: number;
  bestAsk?: number;
  lastTradePrice?: number;
  oneDayPriceChange?: number;
  volumeNum?: number;
  volume?: string | number;
  volume24hr?: number;
  liquidityNum?: number;
  liquidity?: string | number;
  startDate?: string;
  endDate?: string;
  endDateIso?: string;
  closedTime?: string;
  active?: boolean;
  closed?: boolean;
  archived?: boolean;
  acceptingOrders?: boolean;
  events?: RawGammaEvent[];
}

export interface RawGammaEvent {
  id?: string;
  ticker?: string;
  slug?: string;
  title?: string;
  description?: string;
  seriesSlug?: string;
  series?: { slug?: string; title?: string }[];
  negRisk?: boolean;
  closed?: boolean;
  active?: boolean;
  volume24hr?: number;
  tags?: { id?: string; slug?: string; label?: string }[];
  markets?: RawGammaMarket[];
}

/* ------------------------------------------------------------ normalisers */

/**
 * The series a Polymarket event belongs to.
 *
 * Gamma states one on most recurring events (`fomc`, `nfl`, `bitcoin-price`),
 * which is the identifier that lines up with a Kalshi series ticker. Where it
 * does not, the event slug's stem is the best available stand-in: Polymarket
 * suffixes repeat listings with a date or a random tail
 * (`fed-decision-in-january-20260729233815502`), and stripping that leaves a
 * label that at least groups the same question together.
 */
export function seriesFromEvent(raw: RawGammaEvent): string {
  const stated = raw.seriesSlug ?? raw.series?.[0]?.slug;
  if (stated) return stated;
  return seriesFromSlug(raw.ticker ?? raw.slug ?? '');
}

/** Strip the disambiguating tail Polymarket appends to a repeated listing. */
export function seriesFromSlug(slug: string): string {
  return (
    slug
      // A long digit run is a creation timestamp, not part of the name.
      .replace(/-\d{6,}$/, '')
      // …and a short one is the "-762" style collision suffix.
      .replace(/-\d{1,5}$/, '')
      .replace(/-+$/, '') || slug
  );
}

function statusOf(raw: RawGammaMarket): MarketStatus {
  if (raw.closed) return 'settled';
  if (raw.archived) return 'closed';
  if (raw.active === false) return 'unopened';
  if (raw.acceptingOrders === false) return 'closed';
  return 'open';
}

/**
 * Which way a settled market resolved.
 *
 * Gamma states no winner field; a resolved market simply prints its outcome
 * prices as `["0", "1"]`. Read only when the market is closed, so a contract
 * trading at 99.5¢ is never reported as already settled.
 */
function resultOf(raw: RawGammaMarket, prices: number[]): string {
  if (!raw.closed) return '';
  if (prices[0] === 1) return 'yes';
  if (prices[1] === 1) return 'no';
  return '';
}

export function normaliseMarket(
  raw: RawGammaMarket,
  parent?: { eventTicker: string; seriesTicker: string },
): Market {
  const event = parent ?? parentOf(raw);
  const outcomes = parseStringArray(raw.outcomes);
  const prices = parseStringArray(raw.outcomePrices)
    .map((p) => num(p))
    .map((p) => p ?? 0);

  const yesBid = price(raw.bestBid);
  const yesAsk = price(raw.bestAsk);
  // `outcomePrices[0]` is gamma's own mark for the YES token — the last print
  // when there is one, the midpoint when there is not.
  const last = price(raw.lastTradePrice) ?? price(prices[0]);
  const change = num(raw.oneDayPriceChange);

  const mid = yesBid !== null && yesAsk !== null ? (yesBid + yesAsk) / 2 : (last ?? yesBid ?? yesAsk);
  const closeTime = raw.endDate ?? raw.endDateIso ?? '';

  return {
    venue: VENUE,
    ticker: raw.slug ?? '',
    eventTicker: event.eventTicker,
    seriesTicker: event.seriesTicker,
    title: raw.question ?? raw.slug ?? '',
    // Within a grouped event the outcome's own label is the strike; a
    // standalone binary market only has "Yes".
    yesSubTitle: raw.groupItemTitle || outcomes[0] || 'Yes',
    noSubTitle: outcomes[1] ?? 'No',
    status: statusOf(raw),
    marketType: 'binary',
    yesBid,
    yesAsk,
    // Polymarket runs one book per token pair and quotes only the first token.
    // An offer to sell YES at `p` is a bid to buy NO at `1 - p`, so the NO side
    // is derived here rather than left blank.
    noBid: yesAsk === null ? null : round4(1 - yesAsk),
    noAsk: yesBid === null ? null : round4(1 - yesBid),
    mid: mid === null ? null : round4(mid),
    lastPrice: last,
    previousPrice: last !== null && change !== null ? round4(last - change) : null,
    change,
    volume: num(raw.volumeNum) ?? num(raw.volume),
    volume24h: num(raw.volume24hr),
    // Gamma reports open interest per *event*, never per market, and the
    // event's figure is not this contract's. Left unstated rather than guessed.
    openInterest: null,
    liquidity: num(raw.liquidityNum) ?? num(raw.liquidity),
    openTime: raw.startDate ?? '',
    closeTime,
    expirationTime: raw.closedTime ?? closeTime,
    result: resultOf(raw, prices),
    rulesPrimary: raw.description ?? '',
    // Polymarket words its strikes into the question ("Will BTC be above
    // $120,000…"), and never states them as numbers. Parsing one out of the
    // prose would feed the implied-price maths a guess, so nothing is claimed.
    strikeType: null,
    floorStrike: null,
    capStrike: null,
  };
}

/** A market fetched on its own still carries its parent event inline. */
function parentOf(raw: RawGammaMarket): { eventTicker: string; seriesTicker: string } {
  const event = raw.events?.[0];
  if (!event) return { eventTicker: '', seriesTicker: seriesFromSlug(raw.slug ?? '') };
  return {
    eventTicker: event.ticker ?? event.slug ?? '',
    seriesTicker: seriesFromEvent(event),
  };
}

/**
 * The closest thing gamma has to Kalshi's category.
 *
 * There is no category field — only a flat bag of tags, in no useful order:
 * the FOMC event's first tag is `fomc` and its fifth is `Politics`. But tag ids
 * are issued in sequence, and the top-level ones were created first, so the
 * lowest id in the bag is the broadest label. `Sports` is 1, `Politics` is 2;
 * `fomc` is 100478.
 */
function categoryOf(raw: RawGammaEvent): string {
  let best: { id: number; label: string } | null = null;
  for (const tag of raw.tags ?? []) {
    const label = tag.label ?? tag.slug ?? '';
    const id = Number(tag.id);
    if (!label || !Number.isFinite(id)) continue;
    if (!best || id < best.id) best = { id, label };
  }
  return best?.label ?? '';
}

export function normaliseEvent(raw: RawGammaEvent): VenueEvent {
  const eventTicker = raw.ticker ?? raw.slug ?? '';
  const seriesTicker = seriesFromEvent(raw);
  const parent = { eventTicker, seriesTicker };

  return {
    venue: VENUE,
    eventTicker,
    seriesTicker,
    title: raw.title ?? eventTicker,
    subTitle: raw.series?.[0]?.title ?? '',
    category: categoryOf(raw),
    // `negRisk` is Polymarket's name for a ladder whose legs cannot both win —
    // exactly Kalshi's mutually-exclusive flag.
    mutuallyExclusive: raw.negRisk ?? false,
    markets: (raw.markets ?? []).map((m) => normaliseMarket(m, parent)),
  };
}

/* ---------------------------------------------------------------- queries */

function qs(params: Record<string, string | number | boolean | undefined>): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== '') search.set(key, String(value));
  }
  const s = search.toString();
  return s ? `?${s}` : '';
}

async function get<T>(base: string, path: string, ttlMs: number): Promise<T> {
  return cache.cached(`polymarket:${base}${path}`, ttlMs, () =>
    fetchJson<T>(`${base}${path}`, { timeoutMs: 20_000, retries: 2 }),
  );
}

/* ------------------------------------------------------------------ lookups */

/** The raw catalogue record for a market slug. */
async function rawMarket(slug: string): Promise<RawGammaMarket> {
  const rows = await get<RawGammaMarket[]>(GAMMA, `/markets${qs({ slug })}`, TTL.quote);
  const row = rows?.[0];
  if (!row) {
    throw new UpstreamError(`No Polymarket market with slug ${slug}`, {
      code: 'not_found',
      hint: 'Polymarket identifies markets by slug, as in the tail of its URL. Try `SRCH` to find one.',
    });
  }
  return row;
}

export async function getMarket(slug: string): Promise<Market> {
  return normaliseMarket(await rawMarket(slug));
}

export async function getEvent(slug: string): Promise<VenueEvent> {
  const rows = await get<RawGammaEvent[]>(GAMMA, `/events${qs({ slug })}`, TTL.quote);
  const row = rows?.[0];
  if (!row) {
    throw new UpstreamError(`No Polymarket event with slug ${slug}`, { code: 'not_found' });
  }
  return normaliseEvent(row);
}

export async function listSeries(): Promise<SeriesInfo[]> {
  const rows = await get<
    { slug?: string; ticker?: string; title?: string; recurrence?: string; seriesType?: string }[]
  >(GAMMA, `/series${qs({ limit: 100, closed: false })}`, TTL.catalogue);

  return (rows ?? []).map((s) => ({
    venue: VENUE,
    ticker: s.slug ?? s.ticker ?? '',
    title: s.title ?? s.slug ?? '',
    category: s.seriesType ?? '',
    frequency: s.recurrence ?? '',
    tags: [],
  }));
}

/**
 * The YES token id for a market slug.
 *
 * Everything on the CLOB — the book, the price history — is keyed by token id,
 * and nothing in the terminal's command surface carries one. Held for the
 * metadata TTL because the mapping is immutable once a market is deployed.
 */
async function yesTokenId(slug: string): Promise<string> {
  return cache.cached(`polymarket:token:${slug}`, TTL.meta, async () => {
    const raw = await rawMarket(slug);
    const token = parseStringArray(raw.clobTokenIds)[0];
    if (!token) {
      throw new UpstreamError(`Polymarket market ${slug} has no order book`, {
        code: 'not_found',
        hint: 'This market was never deployed to the CLOB, so it has no book or price history.',
      });
    }
    return token;
  });
}

/* -------------------------------------------------------------------- book */

export interface RawClobBook {
  bids?: { price: string; size: string }[];
  asks?: { price: string; size: string }[];
}

/**
 * Turn the CLOB's two ladders into the terminal's book.
 *
 * Unlike Kalshi, Polymarket quotes a genuine bid and ask on one token, so no
 * inversion is needed to build the ask side. The NO ladder is the one that has
 * to be derived: a resting offer to sell YES at `p` is a bid to buy NO at
 * `1 - p`, and traders comparing this book against a Kalshi one expect to see it.
 */
export function normaliseOrderBook(raw: RawClobBook, ticker: string, depth = 12): OrderBook {
  const rows = (side: { price: string; size: string }[] | undefined): { price: number; size: number }[] =>
    (side ?? [])
      .map((l) => ({ price: num(l.price) ?? 0, size: num(l.size) ?? 0 }))
      .filter((l) => l.price > 0 && l.size > 0);

  const yes = rows(raw.bids).sort((a, b) => b.price - a.price).slice(0, depth);
  const yesAsks = rows(raw.asks).sort((a, b) => a.price - b.price).slice(0, depth);
  const no = yesAsks
    .map((l) => ({ price: round4(1 - l.price), size: l.size }))
    .sort((a, b) => b.price - a.price);

  const bestYesBid = yes[0]?.price ?? null;
  const bestYesAsk = yesAsks[0]?.price ?? null;

  return {
    venue: VENUE,
    ticker,
    yes,
    no,
    yesAsks,
    bestYesBid,
    bestYesAsk,
    spread: bestYesBid !== null && bestYesAsk !== null ? round4(bestYesAsk - bestYesBid) : null,
    mid: bestYesBid !== null && bestYesAsk !== null ? round4((bestYesBid + bestYesAsk) / 2) : null,
  };
}

export async function getOrderBook(slug: string, depth = 12): Promise<OrderBook> {
  const token = await yesTokenId(slug);
  const raw = await get<RawClobBook>(CLOB, `/book${qs({ token_id: token })}`, TTL.quote);
  return normaliseOrderBook(raw, slug, depth);
}

/* ------------------------------------------------------------------ trades */

export interface RawDataTrade {
  transactionHash?: string;
  timestamp?: number;
  price?: number;
  size?: number;
  side?: string;
  outcomeIndex?: number;
  outcome?: string;
}

/**
 * Normalise the public print tape.
 *
 * Every trade is reported from the point of view of the outcome token that
 * changed hands, so a `BUY` of the NO token at 26¢ and a `SELL` of the YES
 * token at 74¢ are the same print seen from two sides. Both are restated in
 * YES terms — price and aggressor alike — so the tape reads the same way as
 * Kalshi's beside it.
 */
export function normaliseTrades(raw: RawDataTrade[], ticker: string): TradesResponse {
  const trades = raw.map((t, index) => {
    const isYesToken = (t.outcomeIndex ?? 0) === 0;
    const traded = num(t.price) ?? 0;
    const yesPrice = isYesToken ? traded : round4(1 - traded);
    const boughtYes = (t.side ?? 'BUY').toUpperCase() === 'BUY' ? isYesToken : !isYesToken;

    return {
      venue: VENUE,
      // The hash identifies the transaction, not the fill: one transaction can
      // sweep several levels, so the index keeps sibling prints distinct.
      tradeId: `${t.transactionHash ?? 'trade'}-${index}`,
      ticker,
      ts: Math.floor(num(t.timestamp) ?? 0),
      count: num(t.size) ?? 0,
      yesPrice,
      noPrice: round4(1 - yesPrice),
      takerSide: boughtYes ? 'yes' : 'no',
      isBlockTrade: false,
    };
  });

  return { trades, cursor: null };
}

export async function getTrades(slug: string, limit = 50): Promise<TradesResponse> {
  const raw = await rawMarket(slug);
  if (!raw.conditionId) {
    throw new UpstreamError(`Polymarket market ${slug} has no on-chain condition`, {
      code: 'not_found',
    });
  }
  const rows = await get<RawDataTrade[]>(
    DATA,
    `/trades${qs({ market: raw.conditionId, limit: Math.min(Math.max(limit, 1), 500) })}`,
    TTL.quote,
  );
  return normaliseTrades(rows ?? [], slug);
}

/* ----------------------------------------------------------------- candles */

export interface RawHistoryPoint {
  /** Unix seconds. */
  t: number;
  /** Price of the YES token at that moment, 0..1. */
  p: number;
}

/**
 * The widest window the CLOB will serve against explicit timestamps.
 *
 * Measured, not documented: `startTs`/`endTs` spans of exactly 1,296,000s
 * return data at every fidelity and one second more returns an empty history
 * with a 200. Silently empty is the worst possible failure mode for a chart, so
 * requests are shaped to stay inside it.
 */
const HISTORY_MAX_SPAN = 15 * 86400;

/** Chunks of {@link HISTORY_MAX_SPAN} to stitch before falling back. */
const HISTORY_MAX_CHUNKS = 8;

/** Concurrent history requests. Enough to be quick, not enough to flood. */
const HISTORY_FAN_OUT = 4;

/**
 * Lookback windows the CLOB accepts in place of timestamps, longest last.
 *
 * These are ranges ending *now*, not bucket sizes — `fidelity` still decides
 * the bucket. They are the only way to reach past the timestamp ceiling.
 */
const HISTORY_INTERVALS: { token: string; seconds: number }[] = [
  { token: '1d', seconds: 86400 },
  { token: '1w', seconds: 7 * 86400 },
  { token: '1m', seconds: 31 * 86400 },
  { token: 'max', seconds: Number.POSITIVE_INFINITY },
];

async function historyChunk(
  token: string,
  fidelity: number,
  startTs: number,
  endTs: number,
): Promise<RawHistoryPoint[]> {
  const raw = await get<{ history?: RawHistoryPoint[] }>(
    CLOB,
    `/prices-history${qs({ market: token, startTs, endTs, fidelity })}`,
    TTL.candles,
  );
  return raw.history ?? [];
}

async function historyWindow(
  token: string,
  fidelity: number,
  startTs: number,
  endTs: number,
): Promise<{ points: RawHistoryPoint[]; note?: string }> {
  const span = endTs - startTs;
  const chunks = Math.ceil(span / HISTORY_MAX_SPAN);

  if (chunks <= HISTORY_MAX_CHUNKS) {
    const windows: [number, number][] = [];
    for (let from = startTs; from < endTs; from += HISTORY_MAX_SPAN) {
      windows.push([from, Math.min(from + HISTORY_MAX_SPAN, endTs)]);
    }

    const points: RawHistoryPoint[] = [];
    for (let i = 0; i < windows.length; i += HISTORY_FAN_OUT) {
      const batch = await Promise.all(
        windows
          .slice(i, i + HISTORY_FAN_OUT)
          .map(([from, to]) => historyChunk(token, fidelity, from, to)),
      );
      for (const chunk of batch) points.push(...chunk);
    }
    return { points };
  }

  // Too wide to request by timestamp. Ask for the nearest lookback window that
  // covers it and clip — and say so, because the upstream also caps how many
  // points a lookback returns, so a long hourly chart really can come back short.
  const lookback =
    HISTORY_INTERVALS.find((i) => i.seconds >= span) ??
    HISTORY_INTERVALS[HISTORY_INTERVALS.length - 1]!;

  const raw = await get<{ history?: RawHistoryPoint[] }>(
    CLOB,
    `/prices-history${qs({ market: token, interval: lookback.token, fidelity })}`,
    TTL.candles,
  );

  const points = (raw.history ?? []).filter((p) => p.t >= startTs && p.t <= endTs);
  return {
    points,
    note:
      `Polymarket serves windows wider than 15 days only as a "${lookback.token}" lookback ` +
      `from now, and caps how much it returns — this chart may start later than requested.`,
  };
}

/**
 * Bucket a price sample series into candles on the terminal's grid.
 *
 * Polymarket publishes prices, not OHLC: each point is the YES token's price at
 * a moment. Open, high, low and close are therefore the first, highest, lowest
 * and last *sample* in the bucket, which is what a candle means when the
 * underlying is a quote series — and volume stays `null`, because no size is
 * attached to any of it.
 *
 * Buckets are stamped with their period *end*, matching Kalshi, so both venues'
 * candles for the same question land on the same x-axis.
 */
export function bucketHistory(points: RawHistoryPoint[], interval: CandleInterval): Candle[] {
  const seconds = interval * 60;
  const buckets = new Map<number, { open: number; high: number; low: number; close: number }>();

  for (const point of points) {
    const p = num(point.p);
    const t = num(point.t);
    if (p === null || t === null) continue;

    // A sample at exactly a boundary closes the period it ends, not the next.
    const end = Math.ceil(t / seconds) * seconds || t;
    const bucket = buckets.get(end);
    if (!bucket) {
      buckets.set(end, { open: p, high: p, low: p, close: p });
      continue;
    }
    bucket.high = Math.max(bucket.high, p);
    bucket.low = Math.min(bucket.low, p);
    bucket.close = p;
  }

  const times = [...buckets.keys()].sort((a, b) => a - b);
  const candles: Candle[] = [];
  let previousClose: number | null = null;

  for (let i = 0; i < times.length; i++) {
    const time = times[i]!;
    const bucket = buckets.get(time)!;

    // Fill the gap to the next observed bucket with flat carried-forward bars,
    // so a quiet market draws a continuous line rather than a jump. Bounded so a
    // market that stopped printing months ago cannot generate a million bars.
    if (previousClose !== null) {
      const previousTime = times[i - 1]!;
      const missing = Math.min((time - previousTime) / seconds - 1, MAX_CARRY_FORWARD);
      for (let step = 1; step <= missing; step++) {
        candles.push(flatCandle(previousTime + step * seconds, previousClose));
      }
    }

    candles.push({
      time,
      open: bucket.open,
      high: bucket.high,
      low: bucket.low,
      close: bucket.close,
      volume: null,
      openInterest: null,
      traded: true,
      bid: null,
      ask: null,
    });
    previousClose = bucket.close;
  }

  return candles;
}

/** Flat bars to bridge one gap. Beyond this the gap is left as a gap. */
const MAX_CARRY_FORWARD = 512;

function flatCandle(time: number, close: number): Candle {
  return {
    time,
    open: close,
    high: close,
    low: close,
    close,
    volume: null,
    openInterest: null,
    traded: false,
    bid: null,
    ask: null,
  };
}

export async function getCandles(
  slug: string,
  interval: CandleInterval,
  startTs: number,
  endTs: number,
): Promise<CandlesResponse> {
  const [token, market] = await Promise.all([yesTokenId(slug), rawMarket(slug)]);
  const { points, note } = await historyWindow(token, interval, startTs, endTs);

  return {
    venue: VENUE,
    ticker: slug,
    seriesTicker: parentOf(market).seriesTicker,
    interval,
    candles: bucketHistory(points, interval),
    note:
      note ??
      'Polymarket publishes a price series rather than OHLC: these bars are its samples bucketed, and carry no volume.',
  };
}

/* ----------------------------------------------------------------- corpus */

const CORPUS_KEY = 'polymarket:corpus';

/**
 * Pages of 100, ordered by 24h volume.
 *
 * Gamma refuses an offset past 2,500 ("use /events/keyset for deeper
 * pagination"), so the whole open universe is not reachable by offset at all
 * and the crawl has to be bounded. Ordering by volume first makes the
 * truncation principled rather than arbitrary — it drops the dead tail, not a
 * random slice — which is the failure the Kalshi crawl had to be fixed for.
 * The snapshot still reports {@link Corpus.truncated} so a panel can say so.
 */
const CORPUS_PAGE = 100;
const CORPUS_MAX_PAGES = 24;

async function buildCorpus(): Promise<Corpus> {
  const events: VenueEvent[] = [];
  const markets: Market[] = [];
  let truncated = false;

  for (let page = 0; page < CORPUS_MAX_PAGES; page++) {
    const path = `/events${qs({
      limit: CORPUS_PAGE,
      offset: page * CORPUS_PAGE,
      closed: false,
      active: true,
      order: 'volume24hr',
      ascending: false,
    })}`;

    let rows: RawGammaEvent[];
    try {
      const raw = await fetchJson<RawGammaEvent[]>(`${GAMMA}${path}`, {
        timeoutMs: 30_000,
        retries: 2,
      });
      rows = Array.isArray(raw) ? raw : [];
    } catch (err) {
      // Gamma answers 422 once the offset passes its ceiling, which it words as
      // "use /events/keyset for deeper pagination". That is the catalogue
      // ending as far as offsets can reach, not a failure — the pages already
      // collected are the whole liquid universe. Only a first-page failure is
      // a real one.
      if (page > 0 && err instanceof UpstreamError && err.status === 422) {
        truncated = true;
        break;
      }
      throw err;
    }

    for (const rawEvent of rows) {
      const event = normaliseEvent(rawEvent);
      events.push(event);
      markets.push(...event.markets);
    }

    if (rows.length < CORPUS_PAGE) break;
    truncated = page === CORPUS_MAX_PAGES - 1;
  }

  return { venue: VENUE, events, markets, builtAt: Date.now(), truncated };
}

export async function corpusSnapshot(): Promise<Corpus> {
  return catalogue.cached(CORPUS_KEY, TTL.catalogue, buildCorpus);
}

export function warmCorpus(): void {
  corpusSnapshot()
    .then((c) => {
      console.log(
        `[polymarket] corpus warm: ${c.events.length} events, ${c.markets.length} markets`,
      );
    })
    .catch((err: unknown) => {
      console.warn('[polymarket] corpus warm failed:', err instanceof Error ? err.message : err);
    });
}

export async function search(query: string, limit = 25): Promise<SearchResponse> {
  return searchCorpus(await corpusSnapshot(), query, limit);
}

export async function topMarkets(sort: MoverSort, limit = 25): Promise<Market[]> {
  return rankMarkets((await corpusSnapshot()).markets, sort, limit);
}
