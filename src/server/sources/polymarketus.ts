/**
 * Polymarket US (polymarket.us) client — the CFTC-regulated book.
 *
 * A different exchange from Polymarket International, not a regional skin of
 * it: different host, different schema, different markets. It splits its API in
 * two, and only one half is usable here:
 *
 *   gateway.polymarket.us   public. events, markets, books, series, search.
 *   api.polymarket.us       API key required. orders, positions, and the only
 *                           historical trade data the exchange publishes.
 *
 * The terminal holds no credentials by design, so it reads the gateway and
 * nothing else. That is a real limit rather than an oversight, and the two
 * places it bites — no chart, no tape — say so in as many words instead of
 * failing obscurely or drawing an empty panel.
 *
 * Prices arrive as `{value, currency}` objects and quantities as decimal
 * strings; both are flattened to plain numbers here.
 */

import type {
  CandleInterval,
  CandlesResponse,
  Market,
  MarketStatus,
  OrderBook,
  SeriesInfo,
  TradesResponse,
  VenueEvent,
} from '../../shared/types.js';
import { isValidIdentifier } from '../../shared/venue.js';
import { TTL, cache, catalogue } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';
import {
  rankMarkets,
  searchCorpus,
  type Corpus,
  type MoverSort,
  type SearchResponse,
} from './corpus.js';

const BASE = process.env.POLYMARKET_US_API_BASE ?? 'https://gateway.polymarket.us';

const VENUE = 'polymarket-us' as const;

/* --------------------------------------------------------------- coercion */

/** Unwrap a `{value, currency}` money object, or a bare decimal string. */
export function money(value: unknown): number | null {
  if (value === null || value === undefined) return null;
  if (typeof value === 'object' && 'value' in (value as Record<string, unknown>)) {
    return money((value as { value: unknown }).value);
  }
  if (value === '') return null;
  const n = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(n) ? n : null;
}

/** As {@link money}, but an empty book side reports `0` and means "nothing". */
function quote(value: unknown): number | null {
  const n = money(value);
  return n === null || n === 0 ? null : n;
}

function round4(n: number): number {
  return Math.round(n * 10_000) / 10_000;
}

/* ------------------------------------------------------------ raw upstream */

export interface RawUsMarket {
  id?: string;
  slug?: string;
  question?: string;
  title?: string;
  titleShort?: string;
  subtitle?: string;
  description?: string;
  category?: string;
  marketType?: string;
  status?: string;
  active?: boolean;
  closed?: boolean;
  archived?: boolean;
  hidden?: boolean;
  startDate?: string;
  endDate?: string;
  gameStartTime?: string;
  bestBidQuote?: { value?: string };
  bestAskQuote?: { value?: string };
  /**
   * Present, and deliberately unread. See {@link normaliseMarket} — the pair is
   * not self-consistent across endpoints.
   */
  outcomes?: string;
  outcomePrices?: string;
  marketSides?: {
    description?: string;
    long?: boolean;
    price?: string;
    quote?: { value?: string };
  }[];
}

export interface RawUsEvent {
  id?: string;
  ticker?: string;
  slug?: string;
  title?: string;
  description?: string;
  category?: string;
  seriesSlug?: string;
  startDate?: string;
  endDate?: string;
  active?: boolean;
  closed?: boolean;
  archived?: boolean;
  hidden?: boolean;
  markets?: RawUsMarket[];
}

export interface RawUsBook {
  marketData?: {
    marketSlug?: string;
    bids?: { px?: { value?: string }; qty?: string }[];
    offers?: { px?: { value?: string }; qty?: string }[];
  };
}

export interface RawUsBbo {
  marketData?: {
    marketSlug?: string;
    currentPx?: { value?: string };
    lastTradePx?: { value?: string };
    settlementPx?: { value?: string };
    sharesTraded?: string;
    openInterest?: string;
    bestBid?: { value?: string };
    bestAsk?: { value?: string };
  };
}

/* ------------------------------------------------------------ normalisers */

/** `MARKET_STATUS_OPEN` → `open`. */
function statusOf(raw: RawUsMarket): MarketStatus {
  const stated = raw.status?.replace(/^MARKET_STATUS_/, '').toLowerCase();
  if (stated) return stated;
  if (raw.closed) return 'closed';
  if (raw.active === false) return 'unopened';
  return 'open';
}

/**
 * One contract, quoted from its best bid and offer.
 *
 * **`outcomePrices` is not read, on purpose.** The field exists on every market
 * and means different things on different endpoints: in a nested event listing
 * it holds `[bestBid, bestAsk]`, and the same market fetched by slug holds
 * `[askToBuyYes, askToBuyNo]` — for one market, `["0.4600","0.4780"]` in one
 * place and `["0.4780","0.54"]` in the other. Nor does the `outcomes` array
 * order it: the same payload labels one market `["Yes","No"]` and its
 * neighbour `["No","Yes"]` with both price arrays in bid/ask order. Reading
 * either would silently mislabel a bid as an ask on half the book. The
 * `bestBidQuote`/`bestAskQuote` pair is unambiguous everywhere, so it is the
 * only price source used.
 */
export function normaliseMarket(
  raw: RawUsMarket,
  parent?: { eventTicker: string; seriesTicker: string },
): Market {
  const yesBid = quote(raw.bestBidQuote);
  const yesAsk = quote(raw.bestAskQuote);
  const mid = yesBid !== null && yesAsk !== null ? round4((yesBid + yesAsk) / 2) : (yesBid ?? yesAsk);

  const label = raw.title || raw.titleShort || raw.question || raw.slug || '';
  const short = raw.marketSides?.find((s) => s.long === false)?.description;

  return {
    venue: VENUE,
    ticker: raw.slug ?? '',
    eventTicker: parent?.eventTicker ?? '',
    seriesTicker: parent?.seriesTicker ?? '',
    title: raw.question || label,
    yesSubTitle: raw.subtitle ? `${label} · ${raw.subtitle}` : label,
    noSubTitle: short ?? 'No',
    status: statusOf(raw),
    marketType: raw.marketType ?? 'binary',
    yesBid,
    yesAsk,
    // One book per contract, quoted in YES terms; the NO side is its mirror.
    noBid: yesAsk === null ? null : round4(1 - yesAsk),
    noAsk: yesBid === null ? null : round4(1 - yesBid),
    mid,
    // The catalogue carries no prints. `bbo` does, and {@link getMarket} fills
    // these in from it; a market read out of the corpus leaves them unstated.
    lastPrice: null,
    previousPrice: null,
    change: null,
    volume: null,
    volume24h: null,
    openInterest: null,
    liquidity: null,
    openTime: raw.startDate ?? '',
    closeTime: raw.endDate ?? '',
    expirationTime: raw.endDate ?? '',
    result: '',
    rulesPrimary: raw.description ?? '',
    ...(raw.category ? { category: raw.category } : {}),
    strikeType: null,
    floorStrike: null,
    capStrike: null,
  };
}

/**
 * The series an event belongs to.
 *
 * The exchange states one on its sports and weather books (`mlb-2026`,
 * `weather-daily-high-nyc`) but leaves it empty on much of the rest, including
 * the FOMC, CPI and election books that matter most for comparing brokers.
 * Slugs are disciplined enough to fall back on: strip the date and what is left
 * names the recurring question, so `usfed-fomc-2026-10-28` and
 * `usfed-fomc-2026-12-09` are one series without the exchange saying so.
 *
 * The date is not always at the end, and is not always numeric. `uscpi-august-
 * yoy` puts the month in the middle, and stripping only a trailing date leaves
 * every month its own series — twelve one-event series where there should be
 * one twelve-event series, none of which can pair with the monthly CPI market
 * at either other broker. So dates are removed wherever they appear.
 */
export function seriesFromEvent(raw: RawUsEvent): string {
  if (raw.seriesSlug) return raw.seriesSlug;
  const slug = raw.ticker ?? raw.slug ?? '';
  return stripDates(slug) || slug;
}

const MONTH_SEGMENT =
  /^(jan|january|feb|february|mar|march|apr|april|may|jun|june|jul|july|aug|august|sep|sept|september|oct|october|nov|november|dec|december)$/;

const isYear = (part = ''): boolean => /^(19|20)\d{2}$/.test(part);
const isDayOrMonth = (part = ''): boolean => /^\d{1,2}$/.test(part);

/**
 * Drop the segments of a slug that name an occasion rather than a question.
 *
 * Dates are removed as whole groups, never as loose numbers, because a slug's
 * other numbers carry meaning: `ushr-tx-15-2026-11-03` is the Texas 15th
 * district on 3 November 2026, and stripping every short number would file all
 * 38 Texas districts under one series. A year anchors the group — the two
 * segments after it if they are a month and day (`2026-11-03`), otherwise the
 * two before it (`03-14-2027`), otherwise nothing.
 */
export function stripDates(slug: string): string {
  const parts = slug.split('-');
  const drop = new Set<number>();

  parts.forEach((part, i) => {
    if (MONTH_SEGMENT.test(part)) drop.add(i);
    if (!isYear(part)) return;

    drop.add(i);
    if (isDayOrMonth(parts[i + 1]) && isDayOrMonth(parts[i + 2])) {
      drop.add(i + 1);
      drop.add(i + 2);
    } else if (isDayOrMonth(parts[i - 1]) && isDayOrMonth(parts[i - 2])) {
      drop.add(i - 1);
      drop.add(i - 2);
    }
  });

  return parts.filter((part, i) => part !== '' && !drop.has(i)).join('-');
}

export function normaliseEvent(raw: RawUsEvent): VenueEvent {
  const eventTicker = raw.ticker ?? raw.slug ?? '';
  const parent = { eventTicker, seriesTicker: seriesFromEvent(raw) };

  return {
    venue: VENUE,
    eventTicker,
    seriesTicker: parent.seriesTicker,
    title: raw.title ?? eventTicker,
    subTitle: '',
    category: raw.category ?? '',
    // The exchange states no exclusivity flag. A multi-leg event here is
    // normally a winner-take-all field, but "normally" is not a claim worth
    // making: `EVT` only draws the Σmid check when it is told the legs exclude
    // each other, and a wrong flag would put a false arbitrage on screen.
    mutuallyExclusive: false,
    markets: (raw.markets ?? []).map((m) => normaliseMarket(m, parent)),
  };
}

/**
 * Merge a live BBO into a catalogue market.
 *
 * The catalogue carries a bid and an ask; `bbo` adds the last print, the open
 * interest and the shares traded, which is the difference between a row in a
 * list and a quote worth acting on.
 */
export function applyBbo(market: Market, raw: RawUsBbo): Market {
  const data = raw.marketData;
  if (!data) return market;

  const yesBid = quote(data.bestBid) ?? market.yesBid;
  const yesAsk = quote(data.bestAsk) ?? market.yesAsk;
  const last = quote(data.lastTradePx) ?? quote(data.currentPx);

  return {
    ...market,
    yesBid,
    yesAsk,
    noBid: yesAsk === null ? null : round4(1 - yesAsk),
    noAsk: yesBid === null ? null : round4(1 - yesBid),
    mid: yesBid !== null && yesAsk !== null ? round4((yesBid + yesAsk) / 2) : (last ?? yesBid ?? yesAsk),
    lastPrice: last,
    // `sharesTraded` is a lifetime figure, so it belongs in `volume`. Nothing
    // in the public API breaks it down by day, so `volume24h` stays unstated.
    volume: money(data.sharesTraded),
    openInterest: money(data.openInterest),
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

async function get<T>(path: string, ttlMs: number): Promise<T> {
  return cache.cached(`polymarket-us:${path}`, ttlMs, () =>
    fetchJson<T>(`${BASE}${path}`, { timeoutMs: 20_000, retries: 2 }),
  );
}

/* ----------------------------------------------------------------- lookups */

/**
 * Reject a slug that would not survive being a URL path segment.
 *
 * Same reasoning as Kalshi's ticker check: `encodeURIComponent` leaves `.`
 * alone, so `..` reaches the gateway as a dot-segment and URL normalisation
 * quietly resolves it to a different endpoint. Polymarket International is not
 * affected — it passes its slug as a query parameter, never as a path segment.
 */
export function assertSlug(slug: string): string {
  if (!isValidIdentifier(slug)) {
    throw new UpstreamError(`"${slug}" is not a valid Polymarket US slug`, {
      code: 'bad_request',
      hint: 'Slugs look like `tec-mlb-champ-2026-09-27-lad`. Try `SRCH` to find one.',
    });
  }
  return slug;
}

export async function getMarket(slug: string): Promise<Market> {
  assertSlug(slug);
  // Named `listing` rather than `catalogue` so it does not shadow the catalogue
  // cache this module now writes its corpus into.
  const [listing, bbo] = await Promise.all([
    get<{ market?: RawUsMarket }>(`/v1/market/slug/${encodeURIComponent(slug)}`, TTL.meta),
    get<RawUsBbo>(`/v1/markets/${encodeURIComponent(slug)}/bbo`, TTL.quote).catch(() => ({})),
  ]);

  if (!listing.market) {
    throw new UpstreamError(`No Polymarket US market with slug ${slug}`, {
      code: 'not_found',
      hint: 'Polymarket US identifies markets by slug, as in `tec-mlb-champ-2026-09-27-lad`.',
    });
  }

  // The by-slug record has no parent, so recover the event from the corpus,
  // which is already warm. A miss only costs the two ticker fields.
  const known = (await corpusSnapshot()).markets.find((m) => m.ticker === slug);
  const market = normaliseMarket(listing.market, {
    eventTicker: known?.eventTicker ?? '',
    seriesTicker: known?.seriesTicker ?? '',
  });

  return applyBbo(market, bbo);
}

export async function getEvent(slug: string): Promise<VenueEvent> {
  assertSlug(slug);
  const raw = await get<{ event?: RawUsEvent }>(
    `/v1/events/slug/${encodeURIComponent(slug)}`,
    TTL.quote,
  );
  if (!raw.event) {
    throw new UpstreamError(`No Polymarket US event with slug ${slug}`, { code: 'not_found' });
  }
  return normaliseEvent(raw.event);
}

export async function listSeries(): Promise<SeriesInfo[]> {
  const raw = await get<{ series?: { slug?: string; title?: string; active?: boolean }[] }>(
    `/v1/series${qs({ limit: 200 })}`,
    TTL.catalogue,
  );
  return (raw.series ?? []).map((s) => ({
    venue: VENUE,
    ticker: s.slug ?? '',
    title: s.title ?? s.slug ?? '',
    category: '',
    frequency: '',
    tags: [],
  }));
}

/* -------------------------------------------------------------------- book */

export function normaliseOrderBook(raw: RawUsBook, ticker: string, depth = 12): OrderBook {
  const rows = (
    side: { px?: { value?: string }; qty?: string }[] | undefined,
  ): { price: number; size: number }[] =>
    (side ?? [])
      .map((l) => ({ price: money(l.px) ?? 0, size: money(l.qty) ?? 0 }))
      .filter((l) => l.price > 0 && l.size > 0);

  const yes = rows(raw.marketData?.bids).sort((a, b) => b.price - a.price).slice(0, depth);
  const yesAsks = rows(raw.marketData?.offers).sort((a, b) => a.price - b.price).slice(0, depth);
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
  assertSlug(slug);
  const raw = await get<RawUsBook>(`/v1/markets/${encodeURIComponent(slug)}/book`, TTL.quote);
  return normaliseOrderBook(raw, slug, depth);
}

/* ------------------------------------------------- what the gateway lacks */

/**
 * Polymarket US publishes no public trade history and no public candles.
 *
 * Both live behind `api.polymarket.us`, which requires an account, identity
 * verification and an API key — `POST /v1beta1/report/trades/search` for prints
 * and `POST /v1beta1/report/trades/stats` for OHLC. The terminal holds no
 * credentials, so it says which endpoint exists and what it costs, rather than
 * returning an empty tape that reads as a market nobody trades.
 */
function unavailable(what: string, endpoint: string): never {
  throw new UpstreamError(`Polymarket US publishes no public ${what}`, {
    code: 'unsupported',
    hint:
      `${what[0]!.toUpperCase()}${what.slice(1)} for this venue is served by ` +
      `${endpoint} on api.polymarket.us, which requires a Polymarket US account and an API key. ` +
      `The terminal reads only public endpoints. Quote and book (DES, OB) work.`,
  });
}

export function getTrades(_slug: string, _limit = 50): Promise<TradesResponse> {
  unavailable('trade tape', 'POST /v1beta1/report/trades/search');
}

export function getCandles(
  _slug: string,
  _interval: CandleInterval,
  _startTs: number,
  _endTs: number,
): Promise<CandlesResponse> {
  unavailable('price history', 'POST /v1beta1/report/trades/stats');
}

/* ----------------------------------------------------------------- corpus */

const CORPUS_KEY = 'polymarket-us:corpus';

/**
 * The crawl is partitioned by category, and the reason is bandwidth.
 *
 * Polymarket US embeds a full team record — logos, colours, three providers'
 * ids — on both sides of every sports market, so 500 sports events weigh 25 MB
 * against 3 MB for 500 political ones. The open universe is ~2,300 events and
 * ~90% of them are sports, most of them table-tennis and ITF fixtures with one
 * market each.
 *
 * A flat page cap would therefore be paid almost entirely in table tennis, and
 * because the gateway returns categories interleaved rather than grouped, the
 * truncation would fall across every category at once — which is exactly how
 * the Kalshi crawl once hid half the entertainment book. Asking per category
 * instead means the small ones always complete, and only the whale is capped.
 */
const CORPUS_PAGE = 100;

/** Pages per category, and the tighter cap sports is held to. */
const CORPUS_MAX_PAGES = 8;
const CORPUS_MAX_SPORTS_PAGES = 4;

/**
 * Categories known to exist, seeded so a quiet one is never missed.
 *
 * The live set is discovered too — a category invented next week is crawled
 * without a code change — but discovery reads one page, and a category with no
 * event on that page would otherwise go unasked-for entirely.
 */
const SEED_CATEGORIES = [
  'politics',
  'macro',
  'crypto',
  'finance',
  'technology',
  'science',
  'geopolitics',
  'culture',
  'sports',
];

async function eventPage(
  category: string,
  offset: number,
): Promise<RawUsEvent[]> {
  const path = `/v1/events${qs({
    limit: CORPUS_PAGE,
    offset,
    active: true,
    closed: false,
    categories: category || undefined,
  })}`;
  const raw = await fetchJson<{ events?: RawUsEvent[] }>(`${BASE}${path}`, {
    timeoutMs: 30_000,
    retries: 2,
    // A page of sports events runs to ~5 MB, well past the scraper default.
    maxBytes: 48 * 1024 * 1024,
  });
  return raw.events ?? [];
}

async function buildCorpus(): Promise<Corpus> {
  const events: VenueEvent[] = [];
  const markets: Market[] = [];
  const seen = new Set<string>();
  let truncated = false;

  const take = (rows: RawUsEvent[]): void => {
    for (const raw of rows) {
      const key = raw.slug ?? raw.ticker ?? '';
      if (!key || raw.hidden || seen.has(key)) continue;
      seen.add(key);
      const event = normaliseEvent(raw);
      events.push(event);
      markets.push(...event.markets);
    }
  };

  const probe = await eventPage('', 0);
  take(probe);

  const categories = new Set(SEED_CATEGORIES);
  for (const raw of probe) if (raw.category) categories.add(raw.category);

  for (const category of categories) {
    const cap = category === 'sports' ? CORPUS_MAX_SPORTS_PAGES : CORPUS_MAX_PAGES;
    for (let page = 0; page < cap; page++) {
      const rows = await eventPage(category, page * CORPUS_PAGE);
      take(rows);
      if (rows.length < CORPUS_PAGE) break;
      if (page === cap - 1) truncated = true;
    }
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
        `[polymarket-us] corpus warm: ${c.events.length} events, ${c.markets.length} markets`,
      );
    })
    .catch((err: unknown) => {
      console.warn('[polymarket-us] corpus warm failed:', err instanceof Error ? err.message : err);
    });
}

/**
 * Search the local snapshot rather than the gateway's own `/v1/search`.
 *
 * That endpoint exists, and it answers `fed` with "Seeman Jan vs Dufek Jakub
 * Jr" — a tennis fixture — while its own `fed-decision` series goes unmentioned.
 * Ranking the snapshot the same way as the other two venues gives results that
 * are both usable and comparable across brokers.
 */
export async function search(query: string, limit = 25): Promise<SearchResponse> {
  return searchCorpus(await corpusSnapshot(), query, limit);
}

/**
 * Leaderboards over the snapshot.
 *
 * Every sort but the movers comes back empty here: the public catalogue states
 * no volume, open interest or resting depth, and {@link rankMarkets} drops a
 * market rather than ranking an unpublished figure as zero.
 */
export async function topMarkets(sort: MoverSort, limit = 25): Promise<Market[]> {
  return rankMarkets((await corpusSnapshot()).markets, sort, limit);
}
