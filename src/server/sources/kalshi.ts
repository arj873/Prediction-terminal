/**
 * Kalshi public trade-api v2 client.
 *
 * The upstream speaks fixed-point decimal strings — `"0.6900"` for prices
 * (suffix `_dollars`) and `"12645.98"` for sizes and volumes (suffix `_fp`).
 * Everything here converts to plain numbers so the rest of the codebase never
 * has to think about it again.
 */

import type {
  Candle,
  CandleInterval,
  CandlesResponse,
  EventsResponse,
  KalshiEvent,
  Market,
  MarketsResponse,
  OrderBook,
  SeriesInfo,
  StrikeType,
  TradesResponse,
} from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';

const BASE = process.env.KALSHI_API_BASE ?? 'https://api.elections.kalshi.com/trade-api/v2';

/* --------------------------------------------------------------- coercion */

/** Parse a fixed-point decimal string. Returns `null` for absent/garbage. */
function num(value: unknown): number | null {
  if (value === null || value === undefined || value === '') return null;
  const n = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(n) ? n : null;
}

/** Same as {@link num} but collapses missing values to 0 — for volumes. */
function num0(value: unknown): number {
  return num(value) ?? 0;
}

/**
 * A price of exactly 0 on a Kalshi book means "no resting order", not "this
 * contract is worthless" — an empty side reports `"0.0000"`. Treat it as absent
 * so the terminal renders `--` rather than a fake 0¢ quote.
 */
function price(value: unknown): number | null {
  const n = num(value);
  return n === null || n === 0 ? null : n;
}

/* ------------------------------------------------------------ raw upstream */

interface RawMarket {
  ticker: string;
  event_ticker?: string;
  title?: string;
  yes_sub_title?: string;
  no_sub_title?: string;
  status?: string;
  market_type?: string;
  yes_bid_dollars?: string;
  yes_ask_dollars?: string;
  no_bid_dollars?: string;
  no_ask_dollars?: string;
  last_price_dollars?: string;
  previous_price_dollars?: string;
  volume_fp?: string;
  volume_24h_fp?: string;
  open_interest_fp?: string;
  liquidity_dollars?: string;
  open_time?: string;
  close_time?: string;
  expiration_time?: string;
  result?: string;
  rules_primary?: string;
  category?: string;
  strike_type?: string;
  floor_strike?: number;
  cap_strike?: number;
}

interface RawEvent {
  event_ticker: string;
  series_ticker?: string;
  title?: string;
  sub_title?: string;
  category?: string;
  mutually_exclusive?: boolean;
  markets?: RawMarket[];
}

export interface RawCandle {
  end_period_ts: number;
  open_interest_fp?: string;
  volume_fp?: string;
  price?: {
    open_dollars?: string;
    high_dollars?: string;
    low_dollars?: string;
    close_dollars?: string;
    mean_dollars?: string;
    previous_dollars?: string;
  };
  yes_bid?: { open_dollars?: string; close_dollars?: string; high_dollars?: string; low_dollars?: string };
  yes_ask?: { open_dollars?: string; close_dollars?: string; high_dollars?: string; low_dollars?: string };
}

/* --------------------------------------------------------- ticker helpers */

/**
 * Derive the series ticker from a market or event ticker.
 *
 * Kalshi tickers are `SERIES-EVENTSUFFIX[-STRIKE]`, and the series segment is
 * the part before the first hyphen — except for the handful of series whose own
 * ticker contains a hyphen (`KXMVECROSSCATEGORY-SHARD1`). The candlesticks
 * endpoint is the only caller that needs this, and it 404s on a wrong guess, so
 * `getCandles` verifies against the market record rather than trusting this.
 */
export function seriesFromTicker(ticker: string): string {
  const first = ticker.indexOf('-');
  return first === -1 ? ticker : ticker.slice(0, first);
}

/* ------------------------------------------------------------ normalisers */

function normaliseMarket(raw: RawMarket, seriesTicker?: string): Market {
  const yesBid = price(raw.yes_bid_dollars);
  const yesAsk = price(raw.yes_ask_dollars);
  const last = price(raw.last_price_dollars);
  const previous = price(raw.previous_price_dollars);
  const eventTicker = raw.event_ticker ?? '';

  const mid = yesBid !== null && yesAsk !== null ? (yesBid + yesAsk) / 2 : (last ?? yesBid ?? yesAsk);

  return {
    ticker: raw.ticker,
    eventTicker,
    seriesTicker: seriesTicker ?? seriesFromTicker(eventTicker || raw.ticker),
    title: raw.title ?? raw.ticker,
    yesSubTitle: raw.yes_sub_title ?? '',
    noSubTitle: raw.no_sub_title ?? '',
    status: raw.status ?? 'unknown',
    marketType: raw.market_type ?? 'binary',
    yesBid,
    yesAsk,
    noBid: price(raw.no_bid_dollars),
    noAsk: price(raw.no_ask_dollars),
    mid: mid === null ? null : round4(mid),
    lastPrice: last,
    previousPrice: previous,
    change: last !== null && previous !== null ? round4(last - previous) : null,
    volume: num0(raw.volume_fp),
    volume24h: num0(raw.volume_24h_fp),
    openInterest: num0(raw.open_interest_fp),
    liquidity: num0(raw.liquidity_dollars),
    openTime: raw.open_time ?? '',
    closeTime: raw.close_time ?? '',
    expirationTime: raw.expiration_time ?? '',
    result: raw.result ?? '',
    rulesPrimary: raw.rules_primary ?? '',
    ...(raw.category ? { category: raw.category } : {}),
    strikeType: STRIKE_TYPES.has(raw.strike_type ?? '')
      ? (raw.strike_type as StrikeType)
      : null,
    // Unlike prices, strikes arrive as JSON numbers already. They are also the
    // one field where 0 is a legitimate value (a rate or a spread can settle at
    // zero), so `num` is right here and `price` would not be.
    floorStrike: num(raw.floor_strike),
    capStrike: num(raw.cap_strike),
  };
}

/**
 * Strike types that name a numeric price level. Kalshi also uses `structured`
 * and `custom` for contracts whose "strike" is a rule rather than a number;
 * those carry no bound the implied-price maths can use.
 */
const STRIKE_TYPES = new Set<string>([
  'greater',
  'greater_or_equal',
  'less',
  'less_or_equal',
  'between',
]);

function normaliseEvent(raw: RawEvent): KalshiEvent {
  const seriesTicker = raw.series_ticker ?? seriesFromTicker(raw.event_ticker);
  return {
    eventTicker: raw.event_ticker,
    seriesTicker,
    title: raw.title ?? raw.event_ticker,
    subTitle: raw.sub_title ?? '',
    category: raw.category ?? '',
    mutuallyExclusive: raw.mutually_exclusive ?? false,
    markets: (raw.markets ?? []).map((m) => normaliseMarket(m, seriesTicker)),
  };
}

/* ---------------------------------------------------------------- queries */

function qs(params: Record<string, string | number | undefined>): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== '') search.set(key, String(value));
  }
  const s = search.toString();
  return s ? `?${s}` : '';
}

async function get<T>(path: string, ttlMs: number): Promise<T> {
  return cache.cached(`kalshi:${path}`, ttlMs, () =>
    fetchJson<T>(`${BASE}${path}`, { timeoutMs: 20_000, retries: 2 }),
  );
}

export interface ListMarketsParams {
  limit?: number;
  cursor?: string;
  status?: string;
  eventTicker?: string;
  seriesTicker?: string;
  tickers?: string;
}

export async function listMarkets(params: ListMarketsParams = {}): Promise<MarketsResponse> {
  const path = `/markets${qs({
    limit: Math.min(Math.max(params.limit ?? 100, 1), 1000),
    cursor: params.cursor,
    status: params.status,
    event_ticker: params.eventTicker,
    series_ticker: params.seriesTicker,
    tickers: params.tickers,
  })}`;
  const raw = await get<{ markets?: RawMarket[]; cursor?: string }>(path, TTL.quote);
  return {
    markets: (raw.markets ?? []).map((m) => normaliseMarket(m)),
    cursor: raw.cursor || null,
  };
}

export async function getMarket(ticker: string): Promise<Market> {
  const raw = await get<{ market?: RawMarket }>(
    `/markets/${encodeURIComponent(ticker)}`,
    TTL.quote,
  );
  if (!raw.market) {
    throw new UpstreamError(`No Kalshi market with ticker ${ticker}`, { code: 'not_found' });
  }
  return normaliseMarket(raw.market);
}

export interface ListEventsParams {
  limit?: number;
  cursor?: string;
  status?: string;
  seriesTicker?: string;
  withNestedMarkets?: boolean;
}

export async function listEvents(params: ListEventsParams = {}): Promise<EventsResponse> {
  const path = `/events${qs({
    limit: Math.min(Math.max(params.limit ?? 100, 1), 200),
    cursor: params.cursor,
    status: params.status,
    series_ticker: params.seriesTicker,
    with_nested_markets: params.withNestedMarkets ? 'true' : undefined,
  })}`;
  const raw = await get<{ events?: RawEvent[]; cursor?: string }>(path, TTL.meta);
  return { events: (raw.events ?? []).map(normaliseEvent), cursor: raw.cursor || null };
}

export async function getEvent(eventTicker: string): Promise<KalshiEvent> {
  const raw = await get<{ event?: RawEvent; markets?: RawMarket[] }>(
    `/events/${encodeURIComponent(eventTicker)}${qs({ with_nested_markets: 'true' })}`,
    TTL.quote,
  );
  if (!raw.event) {
    throw new UpstreamError(`No Kalshi event with ticker ${eventTicker}`, { code: 'not_found' });
  }
  // Some responses nest markets under the event, others return them alongside.
  const event = normaliseEvent(raw.event);
  if (event.markets.length === 0 && raw.markets?.length) {
    event.markets = raw.markets.map((m) => normaliseMarket(m, event.seriesTicker));
  }
  return event;
}

export interface RawOrderBook {
  orderbook_fp?: { yes_dollars?: [string, string][]; no_dollars?: [string, string][] };
}

/**
 * Turn Kalshi's two bid ladders into a conventional book.
 *
 * Kalshi quotes both sides as *bids*: a YES ladder and a NO ladder. There is no
 * ask ladder, because an offer to sell YES at `p` is identical to a bid to buy
 * NO at `1 - p`. Traders expect a bid/ask, so the NO side is inverted into YES
 * ask terms here — once, on the server — rather than in every panel that
 * renders a book.
 */
export function normaliseOrderBook(raw: RawOrderBook, ticker: string, depth = 12): OrderBook {
  const book = raw.orderbook_fp ?? {};
  const toLevels = (rows: [string, string][] | undefined): BookRow[] =>
    (rows ?? [])
      .map(([p, s]) => ({ price: num0(p), size: num0(s) }))
      .filter((l) => l.price > 0 && l.size > 0);

  // Kalshi returns both sides ascending by price. Best bid is the highest.
  const yes = toLevels(book.yes_dollars).sort((a, b) => b.price - a.price).slice(0, depth);
  const no = toLevels(book.no_dollars).sort((a, b) => b.price - a.price).slice(0, depth);

  // A resting NO bid at q is an offer to sell YES at 1 - q.
  const yesAsks = no
    .map((l) => ({ price: round4(1 - l.price), size: l.size }))
    .sort((a, b) => a.price - b.price);

  const bestYesBid = yes[0]?.price ?? null;
  const bestYesAsk = yesAsks[0]?.price ?? null;

  return {
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

export async function getOrderBook(ticker: string, depth = 12): Promise<OrderBook> {
  const raw = await get<RawOrderBook>(
    `/markets/${encodeURIComponent(ticker)}/orderbook${qs({ depth })}`,
    TTL.quote,
  );
  return normaliseOrderBook(raw, ticker, depth);
}

interface BookRow {
  price: number;
  size: number;
}

/** Kill binary-float dust from the `1 - q` inversion (0.30000000000000004). */
function round4(n: number): number {
  return Math.round(n * 10_000) / 10_000;
}

export async function getTrades(
  ticker: string,
  limit = 50,
  cursor?: string,
): Promise<TradesResponse> {
  const raw = await get<{
    trades?: {
      trade_id: string;
      ticker: string;
      created_time: string;
      count_fp?: string;
      yes_price_dollars?: string;
      no_price_dollars?: string;
      taker_side?: string;
      is_block_trade?: boolean;
    }[];
    cursor?: string;
  }>(
    `/markets/trades${qs({ ticker, limit: Math.min(Math.max(limit, 1), 1000), cursor })}`,
    TTL.quote,
  );

  return {
    trades: (raw.trades ?? []).map((t) => {
      const yesPrice = num(t.yes_price_dollars);
      const noPrice = num(t.no_price_dollars);
      // Only one side is always present; the pair sums to 1.
      const yes = yesPrice ?? (noPrice !== null ? round4(1 - noPrice) : 0);
      const no = noPrice ?? round4(1 - yes);
      return {
        tradeId: t.trade_id,
        ticker: t.ticker,
        ts: Math.floor(new Date(t.created_time).getTime() / 1000),
        count: num0(t.count_fp),
        yesPrice: yes,
        noPrice: no,
        takerSide: t.taker_side ?? 'yes',
        isBlockTrade: t.is_block_trade ?? false,
      };
    }),
    cursor: raw.cursor || null,
  };
}

export async function listSeries(category?: string): Promise<SeriesInfo[]> {
  const raw = await get<{
    series?: { ticker: string; title?: string; category?: string; frequency?: string; tags?: string[] }[];
  }>(`/series/${qs({ category })}`, TTL.catalogue);
  return (raw.series ?? []).map((s) => ({
    ticker: s.ticker,
    title: s.title ?? s.ticker,
    category: s.category ?? '',
    frequency: s.frequency ?? '',
    tags: s.tags ?? [],
  }));
}

/* ----------------------------------------------------------------- search */

/**
 * The searchable universe, built from *events* rather than markets.
 *
 * This is not a micro-optimisation — it is the only workable route. Kalshi's
 * open-market list is ~99.98% auto-generated multivariate parlay legs
 * (`KXMVE…`, titles like `"yes Boston,yes Chicago WS,yes Miami,…"`): paging
 * `/markets` returned 11,998 of them in the first 12,000 rows, two of which
 * were real. `/events?with_nested_markets=true` excludes them entirely and
 * yields ~2,000 markets per page of real, human-authored contracts.
 *
 * Held for {@link TTL.catalogue} and warmed at boot, so the first search a user
 * runs does not pay for the crawl. Prices inside the snapshot go stale, so
 * anything that renders a live quote re-fetches the individual market.
 */
interface Corpus {
  events: KalshiEvent[];
  markets: Market[];
  builtAt: number;
}

const CORPUS_KEY = 'kalshi:corpus';
const CORPUS_MAX_PAGES = 20;

async function buildCorpus(): Promise<Corpus> {
  const events: KalshiEvent[] = [];
  const markets: Market[] = [];
  let cursor: string | undefined;

  for (let page = 0; page < CORPUS_MAX_PAGES; page++) {
    const path = `/events${qs({
      limit: 200,
      status: 'open',
      with_nested_markets: 'true',
      cursor,
    })}`;

    const raw = await fetchJson<{ events?: RawEvent[]; cursor?: string }>(`${BASE}${path}`, {
      timeoutMs: 30_000,
      retries: 2,
    });

    for (const rawEvent of raw.events ?? []) {
      const event = normaliseEvent(rawEvent);
      events.push(event);
      markets.push(...event.markets);
    }

    cursor = raw.cursor || undefined;
    if (!cursor || !raw.events?.length) break;
  }

  return { events, markets, builtAt: Date.now() };
}

async function corpus(): Promise<Corpus> {
  return cache.cached(CORPUS_KEY, TTL.catalogue, buildCorpus);
}

/**
 * Kick off the crawl without blocking startup.
 *
 * A cold corpus takes ~15s to build. Doing it at boot means the operator waits,
 * not the first user to type `SRCH`.
 */
export function warmCorpus(): void {
  corpus()
    .then((c) => {
      console.log(
        `[kalshi] corpus warm: ${c.events.length} events, ${c.markets.length} markets`,
      );
    })
    .catch((err: unknown) => {
      console.warn('[kalshi] corpus warm failed:', err instanceof Error ? err.message : err);
    });
}

export interface EventSearchHit {
  event: Omit<KalshiEvent, 'markets'>;
  /** Markets in the event, most liquid first. */
  markets: Market[];
  /** Summed 24h volume across the event's markets. */
  volume24h: number;
  score: number;
}

export interface SearchResponse {
  query: string;
  hits: EventSearchHit[];
  /** How many events were searched. */
  scanned: number;
  /** Age of the snapshot in seconds — surfaced so the UI can say so. */
  snapshotAgeSeconds: number;
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * Rank open events against a free-text query.
 *
 * Every whitespace-separated term must appear somewhere in the haystack
 * (event ticker, title, sub-title, category, and its markets' strike labels).
 * Scoring rewards ticker hits, title-prefix hits and word-boundary hits, and a
 * whole-phrase match outranks scattered terms. Ties break on 24h volume so the
 * liquid event wins.
 */
export async function search(query: string, limit = 25): Promise<SearchResponse> {
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
  const snapshot = await corpus();
  const hits: EventSearchHit[] = [];

  for (const event of snapshot.events) {
    const ticker = event.eventTicker.toLowerCase();
    const title = event.title.toLowerCase();
    const strikes = event.markets
      .map((m) => m.yesSubTitle)
      .join(' ')
      .toLowerCase();
    const haystack = `${ticker} ${title} ${event.subTitle.toLowerCase()} ${event.category.toLowerCase()} ${strikes}`;

    let score = 0;
    let matchedAll = true;

    for (const term of terms) {
      if (!haystack.includes(term)) {
        matchedAll = false;
        break;
      }
      score += 10;
      if (ticker.includes(term)) score += 12;
      if (title.startsWith(term)) score += 8;
      if (new RegExp(`\\b${escapeRegExp(term)}`).test(haystack)) score += 6;
    }
    if (!matchedAll) continue;

    if (terms.length > 1 && haystack.includes(terms.join(' '))) score += 25;

    const volume24h = event.markets.reduce((sum, m) => sum + m.volume24h, 0);
    if (volume24h > 0) score += 5;

    const { markets, ...meta } = event;
    hits.push({
      event: meta,
      markets: [...markets].sort((a, b) => b.volume24h - a.volume24h),
      volume24h,
      score,
    });
  }

  hits.sort((a, b) => b.score - a.score || b.volume24h - a.volume24h);

  return {
    query,
    hits: hits.slice(0, limit),
    scanned: snapshot.events.length,
    snapshotAgeSeconds: Math.round((Date.now() - snapshot.builtAt) / 1000),
  };
}

export type MoverSort = 'volume' | 'gainers' | 'losers' | 'open_interest' | 'liquidity';

/** Leaderboard over the snapshot. Powers the `TOP` command. */
export async function topMarkets(sort: MoverSort, limit = 25): Promise<Market[]> {
  const snapshot = await corpus();

  const eligible = snapshot.markets.filter((m) => {
    if (sort === 'gainers' || sort === 'losers') return m.change !== null && m.volume24h > 0;
    return true;
  });

  const compare: Record<MoverSort, (a: Market, b: Market) => number> = {
    volume: (a, b) => b.volume24h - a.volume24h,
    open_interest: (a, b) => b.openInterest - a.openInterest,
    liquidity: (a, b) => b.liquidity - a.liquidity,
    gainers: (a, b) => (b.change ?? 0) - (a.change ?? 0),
    losers: (a, b) => (a.change ?? 0) - (b.change ?? 0),
  };

  return eligible.sort(compare[sort]).slice(0, limit);
}

/**
 * Candlesticks for a market.
 *
 * The upstream path embeds the *series* ticker, which a market ticker only
 * sometimes yields by prefix. We look the market up first (cached, and normally
 * already warm because the chart panel just quoted it) and use its recorded
 * series, falling back to the prefix guess if the lookup fails.
 *
 * Periods with no trades carry only `previous_dollars`. Those are emitted as
 * flat candles at the previous close with `traded: false`, so a chart stays
 * continuous instead of gapping, and the client can style them differently.
 */
export function normaliseCandles(rawCandles: RawCandle[]): Candle[] {
  const candles: Candle[] = [];
  let previousClose: number | null = null;

  for (const c of rawCandles) {
    const p = c.price ?? {};
    const bid = price(c.yes_bid?.close_dollars);
    const ask = price(c.yes_ask?.close_dollars);

    const open = num(p.open_dollars);
    const close = num(p.close_dollars);
    const traded = open !== null && close !== null;

    let o: number, h: number, l: number, cl: number;
    if (traded) {
      o = open;
      cl = close;
      h = num(p.high_dollars) ?? Math.max(o, cl);
      l = num(p.low_dollars) ?? Math.min(o, cl);
    } else {
      // No prints this period. Hold the last close; failing that, use the book
      // mid so the series still starts somewhere sensible.
      const carry =
        num(p.previous_dollars) ??
        previousClose ??
        (bid !== null && ask !== null ? round4((bid + ask) / 2) : (bid ?? ask));
      if (carry === null) continue;
      o = h = l = cl = carry;
    }

    previousClose = cl;
    candles.push({
      time: c.end_period_ts,
      open: o,
      high: h,
      low: l,
      close: cl,
      volume: num0(c.volume_fp),
      openInterest: num0(c.open_interest_fp),
      traded,
      bid,
      ask,
    });
  }

  candles.sort((a, b) => a.time - b.time);
  return candles;
}

export async function getCandles(
  ticker: string,
  interval: CandleInterval,
  startTs: number,
  endTs: number,
  /**
   * Skip the market lookup when the series is already known. The implied-price
   * fan-out reads one ladder's worth of candles at a time and knows the series
   * from the event, so passing it halves that route's upstream request count.
   */
  knownSeriesTicker?: string,
): Promise<CandlesResponse> {
  let seriesTicker = knownSeriesTicker ?? seriesFromTicker(ticker);
  if (!knownSeriesTicker) {
    try {
      const market = await getMarket(ticker);
      if (market.seriesTicker) seriesTicker = market.seriesTicker;
    } catch {
      // Market lookup is an optimisation; the prefix guess is right most of the time.
    }
  }

  const path =
    `/series/${encodeURIComponent(seriesTicker)}/markets/${encodeURIComponent(ticker)}` +
    `/candlesticks${qs({ start_ts: startTs, end_ts: endTs, period_interval: interval })}`;

  const raw = await get<{ candlesticks?: RawCandle[] }>(path, TTL.candles);

  return { ticker, seriesTicker, interval, candles: normaliseCandles(raw.candlesticks ?? []) };
}
