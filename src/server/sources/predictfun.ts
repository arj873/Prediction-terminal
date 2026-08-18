/**
 * predict.fun client — the BNB-chain order-book exchange.
 *
 * The venue documents a REST API at `api.predict.fun` and gates every path of
 * it, read-only ones included, behind an account-issued `x-api-key`. Its own
 * website never calls it: the browser bundle talks to a single public GraphQL
 * endpoint instead, which answers the whole market-data surface — catalogue,
 * contract, book, tape, price history, taxonomy — with no credential at all.
 * That is the endpoint read here, so nothing this module needs is paywalled.
 *
 * Three things about that endpoint shape the code below.
 *
 *   * It answers a failed query with **HTTP 200** and a top-level `errors`
 *     array, and a missing entity with `data.category === null` and no error at
 *     all. Status alone therefore tells you nothing; {@link gql} reads the body.
 *   * A wrong `Origin` is a hard 403 (`request-origin not allowed`), while no
 *     `Origin` is a plain 200 — so nothing here ever sets one.
 *   * `pagination.first` is clamped to 100 in silence, so a crawl that asks for
 *     more just gets 100 and a wrong idea of how far it got.
 *
 * The venue's unit of grouping is a *category* (32 NFL teams under
 * `big-game-champion-2027`) and its unit of trading is a *market*, one leg of
 * that category with a numbered outcome pair — index 1 is always the YES side,
 * whatever it is called. Categories are named by a slug a trader would type;
 * legs are named by a database integer nobody would. See {@link makeTicker}.
 *
 * Money arrives in three dialects and none of them leaves this file: book and
 * quote prices are plain dollars, the tape is 1e18-scaled decimal strings, and
 * turnover is US *dollars* rather than contracts — which the venue's registry
 * note says out loud, because a dollar volume silently compared against
 * Kalshi's contract volume is a wrong number rather than a missing one.
 */

import type {
  Candle,
  CandleInterval,
  CandlesResponse,
  Market,
  MarketStatus,
  OrderBook,
  SeriesInfo,
  Trade,
  TradesResponse,
  VenueEvent,
} from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';
import {
  rankMarkets,
  searchCorpus,
  type Corpus,
  type MoverSort,
  type SearchResponse,
} from './corpus.js';
import { stripDates } from './slug.js';

const ENDPOINT = process.env.PREDICTFUN_GRAPHQL_URL ?? 'https://graphql.predict.fun/graphql';

const VENUE = 'predictfun' as const;

/**
 * What joins a category slug to a leg id in a contract ticker.
 *
 * A hyphen would be unreadable in both directions: the venue's own slugs end in
 * hyphenated numbers (`btc-updown-15m-1787032800`, `big-game-champion-2027`),
 * so `slug-26952` cannot be split back into its parts without guessing which
 * trailing number is the leg. `~` appears in no slug, so the split is exact.
 */
const TICKER_SEPARATOR = '~';

/* ------------------------------------------------------------- transport */

interface GraphQLBody<T> {
  data?: T | null;
  errors?: { message?: string; extensions?: { code?: string } }[];
}

/**
 * Run one query and hand back its `data`, or throw.
 *
 * Sent as GET with the query in the URL. The endpoint serves both verbs for
 * queries, and GET is the one that goes through this repo's {@link fetchJson} —
 * which brings the timeout, the bounded retries, the response ceiling and the
 * transport-error vocabulary that every other venue module already relies on.
 * A hand-rolled POST would have to restate all four, in a file that is supposed
 * to be about predict.fun.
 *
 * `fetchJson` also covers the nginx failure mode for free: when the endpoint
 * answers an overweight query with an HTML error page, the parse fails and the
 * caller gets an `UpstreamError` rather than a `SyntaxError` from deep inside a
 * normaliser. What is left for this helper is the 200-with-`errors` case, which
 * no HTTP-level check would ever notice.
 */
async function gql<T>(
  key: string,
  ttlMs: number,
  query: string,
  variables: Record<string, unknown> = {},
  timeoutMs = 25_000,
): Promise<T> {
  return cache.cached(`predictfun:${key}`, ttlMs, async () => {
    const url = new URL(ENDPOINT);
    url.searchParams.set('query', query);
    if (Object.keys(variables).length > 0) {
      url.searchParams.set('variables', JSON.stringify(variables));
    }

    const body = await fetchJson<GraphQLBody<T>>(url.toString(), {
      timeoutMs,
      retries: 2,
      // A crawl page of 100 categories with every leg runs to ~750 KB and the
      // 32-leg timeseries to ~160 KB, so the ceiling is only raised against a
      // catalogue that grows an order of magnitude, not against today's.
      maxBytes: 32 * 1024 * 1024,
    });

    const failure = body.errors?.[0];
    if (failure) {
      throw new UpstreamError(`predict.fun rejected the query: ${failure.message ?? 'unknown error'}`, {
        code: 'upstream_error',
        hint:
          'The GraphQL endpoint answers a rejected query with HTTP 200 and an error body. ' +
          `Reported code: ${failure.extensions?.code ?? 'none'}.`,
      });
    }
    if (body.data === null || body.data === undefined) {
      throw new UpstreamError('predict.fun returned no data for the query', {
        code: 'upstream_error',
      });
    }
    return body.data;
  });
}

/* --------------------------------------------------------------- coercion */

/**
 * A published number, or `null` where the venue published nothing.
 *
 * Unlike the Polymarkets, this venue says `null` when it means "no resting
 * order" rather than `0`, so there is no zero to undo here — and a `0` that
 * does arrive is the venue's own statement and survives as one. Nulls are not
 * an edge case: 189 of 1,780 outcomes on the busiest catalogue page carry no
 * bid or ask, and 151 of those sit on live legs of open categories, so every
 * quote read goes through here.
 */
export function figure(value: number | null | undefined): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

/** A price the venue sent as text, as the last-print field does (`"0.14"`). */
export function decimal(value: string | number | null | undefined): number | null {
  if (typeof value === 'number') return figure(value);
  if (typeof value !== 'string' || value.trim() === '') return null;
  return figure(Number(value));
}

/**
 * Read one of the tape's 1e18-scaled integer strings.
 *
 * Both the filled size and the executed price arrive this way
 * (`"207230000000000000000"` is 207.23 shares, `"70000000000000000"` is 7¢).
 * Anything that is not a run of digits is a shape this module does not
 * understand, and reporting it as `0` would put a free trade on the tape.
 */
export function fromWei(value: string | number | null | undefined): number | null {
  const text = typeof value === 'number' ? String(value) : value;
  if (typeof text !== 'string' || !/^-?\d+$/.test(text.trim())) return null;
  return figure(Number(text) / 1e18);
}

function round(value: number, places: number): number {
  const factor = 10 ** places;
  return Math.round(value * factor) / factor;
}

const round4 = (value: number): number => round(value, 4);

/**
 * The venue's own YES↔NO price mirror.
 *
 * Taken verbatim from predict.fun's order-book note, rounding included: the
 * book holds YES prices only, and the NO ladder is `1 - price` computed at the
 * *market's* `decimalPrecision`. That precision is per leg and genuinely
 * differs from its category's — leg 26952 is quoted to three places inside a
 * category quoted to two — so using the category's would put a NO ladder a
 * tenth of a cent away from the one the exchange itself shows.
 */
export function complement(price: number, decimalPrecision: number | null | undefined): number {
  const places =
    typeof decimalPrecision === 'number' && decimalPrecision >= 1 && decimalPrecision <= 6
      ? Math.round(decimalPrecision)
      : 2;
  const factor = 10 ** places;
  return (factor - Math.round(price * factor)) / factor;
}

/* ------------------------------------------------------------ raw upstream */

export interface RawPfConnection<T> {
  totalCount?: number | null;
  pageInfo?: { hasNextPage?: boolean; endCursor?: string | null };
  edges?: ({ cursor?: string; node?: T | null } | null)[] | null;
}

/** Flatten a Relay connection, which is how every list on this API arrives. */
export function nodes<T>(connection: RawPfConnection<T> | null | undefined): T[] {
  const out: T[] = [];
  for (const edge of connection?.edges ?? []) {
    if (edge?.node) out.push(edge.node);
  }
  return out;
}

export interface RawPfOutcome {
  id?: string;
  /** 1 is the YES side on every market, whatever the venue named it. */
  index?: number;
  name?: string | null;
  status?: string | null;
  chancePercentage?: number | null;
  bidPriceInCurrency?: number | null;
  askPriceInCurrency?: number | null;
  statistics?: { sharesCount?: number | null; positionsValueUsd?: number | null } | null;
}

export interface RawPfMarketStatistics {
  volumeTotalUsd?: number | null;
  volume24hUsd?: number | null;
  /** Turnover against the previous day, not a price move. Unread. */
  volume24hChangeUsd?: number | null;
  /**
   * Published, unsigned, and deliberately unread: it is the 24h *range*.
   *
   * It is named like the daily move and behaves like a spread. It is never
   * negative — 0 of 6,341 stated values in a full crawl of the open catalogue —
   * and where it is non-zero it tracks max − min of the venue's own chance
   * series rather than the distance between its ends: leg 932250 reports 15.5
   * over a day that finished 0.4 lower, leg 1215726 reports 11.0 over a rise of
   * 4, and leg 935920 reports 29.0 over a 29-point *fall*. Across the legs with
   * a non-zero figure it matched the range on 22 of 28 and the signed move on
   * none of the contested ones. Passing it through as `Market.change` would
   * paint every crash on this venue green.
   */
  percentageChanceChange24h?: number | null;
  liquidity3CAskUsd?: number | null;
  /** Present, unscaled and unread — see {@link normaliseMarket}. */
  totalLiquidityUsd?: number | null;
}

export interface RawPfOrderbook {
  marketId?: number;
  /** `[price, size]`, price in dollars and size in shares, cheapest first. */
  asks?: [number, number][] | null;
  /** `[price, size]`, best (highest) first. */
  bids?: [number, number][] | null;
  lastOrderSettled?: {
    /** Whole cents, in YES terms whichever outcome is named — see {@link lastPrintOf}. */
    price?: string | null;
    side?: string | null;
    /** The side the resting order sat on, not the space its price is quoted in. */
    outcome?: string | null;
    kind?: string | null;
  } | null;
}

export interface RawPfMarket {
  id?: string;
  /** The short leg label: `"Los Angeles Rams"`, `">¥42B"`, `"↑ 5.5%"`. */
  title?: string | null;
  /** The full question sentence. */
  question?: string | null;
  description?: string | null;
  status?: string | null;
  marketType?: string | null;
  isTradingEnabled?: boolean | null;
  decimalPrecision?: number | null;
  /**
   * Published, and deliberately unread. See {@link normaliseMarket}.
   */
  chancePercentage?: number | null;
  nearMidpointLiquidityUsd?: number | null;
  resolution?: { index?: number | null; name?: string | null; status?: string | null } | null;
  statistics?: RawPfMarketStatistics | null;
  orderbook?: RawPfOrderbook | null;
  outcomes?: RawPfConnection<RawPfOutcome> | null;
  category?: RawPfCategory | null;
}

export interface RawPfCategory {
  id?: string;
  slug?: string | null;
  title?: string | null;
  description?: string | null;
  status?: string | null;
  marketVariant?: string | null;
  /** The venue's own statement that the legs exclude each other. */
  isNegRisk?: boolean | null;
  decimalPrecision?: number | null;
  startsAt?: string | null;
  endsAt?: string | null;
  statistics?: {
    volumeTotalUsd?: number | null;
    volume24hUsd?: number | null;
    liquidity3CAskUsd?: number | null;
    liquidityValueUsd?: number | null;
  } | null;
  tags?: RawPfConnection<{ id?: string; name?: string | null }> | null;
  markets?: RawPfConnection<RawPfMarket> | null;
}

export interface RawPfTrade {
  transactionHash?: string | null;
  /** 1e18-scaled shares. */
  amountFilled?: string | null;
  /** 1e18-scaled dollars, in the named outcome's own price space. */
  priceExecuted?: string | null;
  quoteType?: string | null;
  /** ISO-8601 UTC, one-second granularity. */
  timestamp?: string | null;
  market?: { id?: string; title?: string | null; category?: { slug?: string | null } | null } | null;
  outcome?: { index?: number | null; name?: string | null } | null;
  account?: { address?: string | null; name?: string | null } | null;
}

/** One probability sample: `x` unix seconds, `y` a percentage 0..100. */
export interface RawPfPoint {
  x?: number | null;
  y?: number | null;
}

export interface RawPfSeries {
  dataGranularity?: string | null;
  market?: { id?: string; title?: string | null } | null;
  data?: RawPfConnection<RawPfPoint> | null;
}

export interface RawPfTag {
  id?: string;
  name?: string | null;
  /** Open categories carrying the tag — the count asked for with a filter. */
  open?: number | null;
  parent?: { id?: string; name?: string | null } | null;
  children?: ({ id?: string; name?: string | null } | null)[] | null;
}

/* ------------------------------------------------------------ identifiers */

/**
 * The terminal's name for one leg: `<category slug>~<leg id>`.
 *
 * Neither half stands alone. The slug is what a trader types and what the
 * website URL uses, but 758 of the venue's 1,024 open categories hold more than
 * one leg, so a slug alone does not name a contract. The leg id is exact but is
 * a bare database integer — `26952` says nothing about what it is, and the
 * venue publishes no per-leg page to look it up on. Carrying both makes the
 * ticker readable and reversible, and {@link getMarket} accepts either half on
 * its own for the cases where that is unambiguous.
 */
export function makeTicker(categorySlug: string, marketId: string): string {
  return categorySlug ? `${categorySlug}${TICKER_SEPARATOR}${marketId}` : marketId;
}

/** Split a ticker back up, on the *last* separator. `null` when there is none. */
export function parseTicker(ticker: string): { slug: string; marketId: string } | null {
  const cut = ticker.lastIndexOf(TICKER_SEPARATOR);
  if (cut < 0) return null;
  return { slug: ticker.slice(0, cut), marketId: ticker.slice(cut + 1) };
}

/**
 * The recurring question behind a slug.
 *
 * Dates come off through the shared {@link stripDates}, which is the same rule
 * Polymarket US's slugs need and for the same reason: `fed-decision-in-september`
 * and `fed-decision-in-october` are one question asked twice, and leaving the
 * month on makes them two one-event series, neither of which then lines up
 * against `KXFEDDECISION` at Kalshi — which is the pairing this terminal exists
 * to draw.
 *
 * Two suffixes are this venue's own, and are taken off first because
 * {@link stripDates} reads segments rather than digit runs. A machine-minted
 * stamp — ten digits of unix seconds on the crypto books
 * (`btc-updown-15m-1787032800`, a new slug every fifteen minutes) or seventeen
 * of creation time on the season books
 * (`laliga-2027-champion-20260701200737375`) — is dropped wherever it appears.
 * A short trailing run is dropped only at the end (`fed-decision-in-september-762`),
 * because that position is where this venue puts a disambiguator and nowhere
 * else: a number in the middle of a slug is part of the question, and
 * `nasdaq-100` should not become `nasdaq`.
 */
export function seriesFromSlug(slug: string): string {
  const withoutStamps = slug
    .split('-')
    .filter((part) => !/^\d{6,}$/.test(part))
    .join('-')
    .replace(/-\d{3,5}$/, '');
  return stripDates(withoutStamps) || withoutStamps;
}

/* ------------------------------------------------------------ normalisers */

/**
 * Leg status, read off the leg rather than its category.
 *
 * A settled leg lives happily inside an open category — 19 of the 890 legs on
 * the busiest catalogue page had already resolved — so taking the category's
 * word would show a decided market as tradable. `REGISTERED` is the ordinary
 * live state here, not a pre-open one, and `isTradingEnabled` is the venue's
 * halt switch on top of it.
 */
export function statusOf(raw: RawPfMarket): MarketStatus {
  switch ((raw.status ?? '').toUpperCase()) {
    case 'RESOLVED':
      return 'settled';
    case 'PRICE_PROPOSED':
    case 'PRICE_DISPUTED':
      return 'determined';
    case 'PAUSED':
      return 'closed';
    case 'INITIALIZING':
    case 'INITIALIZED':
    case 'CREATING':
      return 'unopened';
    case 'CREATED':
    case 'REGISTERED':
    case 'UNPAUSED':
      return raw.isTradingEnabled === false ? 'closed' : 'open';
    default:
      return (raw.status ?? '').toLowerCase() || 'open';
  }
}

/**
 * Which side won, by outcome index rather than by name.
 *
 * The names are whatever the question needed — `DNS`, `Down`, a team — so only
 * the index carries the yes/no meaning the terminal settles on.
 */
function resultOf(raw: RawPfMarket): string {
  const index = raw.resolution?.index;
  if (index === 1) return 'yes';
  if (index === 2) return 'no';
  return '';
}

/**
 * The label that identifies a leg on screen and in the search index.
 *
 * The venue splits the label across two fields and which one matters flips by
 * market. On the NFL book the outcomes are the generic `Yes`/`No` pair and the
 * leg title is the team; on the esports book the leg title is `Match Winner`
 * for every leg and the outcomes are `T1`/`DNS`. Preferring a named outcome and
 * falling back to the title gets both right, where either field alone reduces
 * one whole family of markets to identical rows.
 */
function legLabel(title: string, outcomeName: string | null | undefined, side: 'yes' | 'no'): string {
  const name = (outcomeName ?? '').trim();
  const generic = /^(yes|no)$/i.test(name);
  if (name && !generic) return name;
  if (side === 'yes') return title || name;
  return name || 'No';
}

/**
 * One leg, quoted from its own outcome pair.
 *
 * **`chancePercentage` is not read, on purpose.** It looks like a midpoint and
 * is not one: across 692 legs with a live two-sided book it agreed with the
 * book's mid on 39.7% of them, and it disagrees loudly rather than subtly —
 * leg 1459048 quotes 0.10/0.85 and reports a chance of 70. It is also not
 * rounded to anything, returning raw float artifacts like 18.999999999999993.
 * Whatever it is (a smoothed or last-trade figure the venue never documents),
 * printing it in a mid column would put a price on screen that no side of the
 * book supports, so the mid is computed here from the YES outcome's own bid and
 * ask and the last print comes from the book's last settled order.
 *
 * Open interest is the one figure the venue publishes under an unrecognisable
 * name: `sharesCount` on each outcome is shares outstanding, and the YES and NO
 * sides agree to within 1% on 99 of 100 legs — the signature of minted pairs,
 * not of a volume restatement.
 */
export function normaliseMarket(raw: RawPfMarket, category?: RawPfCategory | null): Market {
  const parent = category ?? raw.category ?? null;
  const slug = parent?.slug ?? parent?.id ?? '';
  const id = raw.id ?? '';

  const outcomes = nodes(raw.outcomes);
  const yes = outcomes.find((o) => o.index === 1);
  const no = outcomes.find((o) => o.index === 2);

  const yesBid = figure(yes?.bidPriceInCurrency);
  const yesAsk = figure(yes?.askPriceInCurrency);
  const lastPrice = lastPrintOf(raw);

  // The venue states the NO side itself, and it agrees with the mirror of the
  // YES side to the last decimal. The mirror is the fallback for the leg that
  // is quoted on one side only, so a half-published book still ladders.
  const noBid =
    figure(no?.bidPriceInCurrency) ?? (yesAsk === null ? null : complement(yesAsk, raw.decimalPrecision));
  const noAsk =
    figure(no?.askPriceInCurrency) ?? (yesBid === null ? null : complement(yesBid, raw.decimalPrecision));

  const mid =
    yesBid !== null && yesAsk !== null ? round4((yesBid + yesAsk) / 2) : (lastPrice ?? yesBid ?? yesAsk);

  const tag = nodes(parent?.tags)[0]?.name ?? '';

  return {
    venue: VENUE,
    ticker: makeTicker(slug, id),
    eventTicker: slug,
    seriesTicker: seriesFromSlug(slug),
    title: raw.question ?? raw.title ?? '',
    yesSubTitle: legLabel(raw.title ?? '', yes?.name, 'yes'),
    noSubTitle: legLabel(raw.title ?? '', no?.name, 'no'),
    status: statusOf(raw),
    marketType: raw.marketType ?? parent?.marketVariant ?? '',
    yesBid,
    yesAsk,
    noBid,
    noAsk,
    mid,
    lastPrice,
    // The venue states a 24h *range* and no earlier price at all — see
    // `percentageChanceChange24h` on {@link RawPfMarketStatistics}. A range has
    // no direction, and the terminal colours this column by its sign, so both
    // fields stay unstated rather than painting every fall as a rise.
    previousPrice: null,
    change: null,
    // Dollars, not contracts — the venue meters turnover in collateral and the
    // registry note says so, because the honest number in the wrong unit is
    // more dangerous silently than the missing one is loudly.
    volume: figure(raw.statistics?.volumeTotalUsd),
    volume24h: figure(raw.statistics?.volume24hUsd),
    openInterest: figure(yes?.statistics?.sharesCount),
    // Dollars resting within 3¢ of the ask. `totalLiquidityUsd` sits next to it
    // and is unscaled to the point of being meaningless — 107,890,435 on a leg
    // whose whole book is about $5.5k — so it is never read.
    liquidity: figure(raw.statistics?.liquidity3CAskUsd),
    openTime: parent?.startsAt ?? '',
    // The venue draws no line between the last trade and settlement; one
    // timestamp closes the category and every leg under it.
    closeTime: parent?.endsAt ?? '',
    expirationTime: parent?.endsAt ?? '',
    result: resultOf(raw),
    // The leg's own terms when the leg was read on its own, the category's when
    // it came out of the catalogue — see {@link MARKET_FIELDS}.
    rulesPrimary: raw.description ?? parent?.description ?? '',
    ...(tag ? { category: tag } : {}),
    // Strikes exist here only as prose inside the leg title (`">¥42B"`,
    // `"↑ 5.5%"`). Nothing in the payload asserts a bound, and a regex over a
    // title would be a guess printed in a column the terminal treats as fact.
    strikeType: null,
    floorStrike: null,
    capStrike: null,
  };
}

/**
 * The last print, which is already quoted in YES terms.
 *
 * `lastOrderSettled` names an outcome beside the price, which reads as an
 * invitation to flip a NO print into YES terms — the tape really does work that
 * way, so the two look alike and behave differently. Of 167 live legs whose
 * last settled order names the NO outcome, 163 sit nearer the YES mid as
 * quoted than inverted, and the ones that decide it are unambiguous: leg
 * 1457355 is quoted 0.01 on YES and reports `{price: "0.01", outcome: "No"}`,
 * which inverted would have printed 99¢ on a penny market. `outcome` names the
 * side the resting order was on, not the space its price is in.
 *
 * The price is also the venue's display figure rather than a tick-exact fill:
 * 315 of 315 sampled prints carry exactly two decimals, including on legs
 * quoted to three, so a sub-cent market can report a last of `0.00`. The exact
 * fills are on the tape — see {@link normaliseTrades}.
 */
function lastPrintOf(raw: RawPfMarket): number | null {
  return decimal(raw.orderbook?.lastOrderSettled?.price);
}

/**
 * One category, with every leg under it.
 *
 * `isNegRisk` is the venue's own statement that the legs exclude each other —
 * it is what makes the site offer NO-to-opposing-YES conversion on the 388
 * categories that carry it — so it maps straight through. The remaining 636 get
 * `false`, which is also what an absent flag gets: `EVT` draws its Σmid
 * arbitrage line only on a stated exclusivity, and a guessed one puts a trade
 * on screen that does not exist.
 */
export function normaliseEvent(raw: RawPfCategory): VenueEvent {
  const slug = raw.slug ?? raw.id ?? '';
  return {
    venue: VENUE,
    eventTicker: slug,
    seriesTicker: seriesFromSlug(slug),
    title: raw.title ?? slug,
    subTitle: '',
    category: nodes(raw.tags)[0]?.name ?? '',
    mutuallyExclusive: raw.isNegRisk === true,
    markets: nodes(raw.markets).map((market) => normaliseMarket(market, raw)),
  };
}

/**
 * The ladder, with the NO side mirrored from it.
 *
 * The exchange keeps one book per leg and stores it in YES prices only, so the
 * NO bids a trader can hit are the YES asks read backwards. Depth is applied
 * here because the venue takes no depth argument at all — it returns the whole
 * book, 57 levels on a busy leg — and the sort is re-applied before slicing so
 * the top of the book cannot be cut off by an upstream that reorders.
 */
export function normaliseOrderBook(raw: RawPfMarket, ticker: string, depth = 12): OrderBook {
  const levels = (rows: [number, number][] | null | undefined): { price: number; size: number }[] =>
    (rows ?? [])
      .map(([price, size]) => ({ price: figure(price) ?? 0, size: figure(size) ?? 0 }))
      .filter((level) => level.price > 0 && level.size > 0);

  const yes = levels(raw.orderbook?.bids)
    .sort((a, b) => b.price - a.price)
    .slice(0, depth);
  const yesAsks = levels(raw.orderbook?.asks)
    .sort((a, b) => a.price - b.price)
    .slice(0, depth);
  const no = yesAsks
    .map((level) => ({ price: complement(level.price, raw.decimalPrecision), size: level.size }))
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

/**
 * The public tape.
 *
 * Two upstream habits are corrected here. Sizes and prices are 1e18-scaled
 * strings, and a price is quoted in whichever outcome the fill names — a NO
 * fill at 0.951 is the same trade as a YES fill at 0.049 — so every row is
 * re-based to YES before it reaches a chart or a VWAP.
 *
 * The identity is the transaction hash, not the edge cursor. The cursor
 * base64-decodes to `{orderId, createdAt}` and therefore repeats across several
 * fills of one resting order: 50 rows of one leg carried 44 distinct cursors
 * and 50 distinct hashes, so keying on the cursor silently drops an eighth of
 * the tape as duplicates.
 *
 * `takerSide` is left empty because the venue documents no aggressor side and
 * the data does not supply one. Its `quoteType` was read as the taker's
 * direction until the same leg was seen printing `Yes/ASK` and `Yes/BID` at one
 * identical price, which that reading cannot produce; the distribution (38 ASK
 * to 2 BID over 40 fills) fits the resting side better than the aggressing one.
 * An unknown side prints as unknown rather than colouring half the tape wrong.
 */
export function normaliseTrades(raw: RawPfConnection<RawPfTrade>, fallbackTicker = ''): Trade[] {
  const trades: Trade[] = [];

  for (const row of nodes(raw)) {
    const size = fromWei(row.amountFilled);
    const executed = fromWei(row.priceExecuted);
    const ts = Date.parse(row.timestamp ?? '');
    if (size === null || executed === null || Number.isNaN(ts)) continue;

    const yesPrice = row.outcome?.index === 2 ? round4(1 - executed) : round4(executed);
    const slug = row.market?.category?.slug ?? '';
    const ticker = row.market?.id && slug ? makeTicker(slug, row.market.id) : fallbackTicker;

    trades.push({
      venue: VENUE,
      tradeId: row.transactionHash ?? `${ticker}:${ts}:${size}`,
      ticker,
      ts: Math.round(ts / 1000),
      count: round(size, 6),
      yesPrice,
      noPrice: round4(1 - yesPrice),
      takerSide: '',
      isBlockTrade: false,
    });
  }

  return trades;
}

/**
 * Bucket a probability sample series onto the terminal's candle grid.
 *
 * predict.fun publishes no OHLC for its own markets — the history is a series
 * of `{x: unix seconds, y: chance in percent}` samples of the YES leg — so
 * open, high, low and close are the first, highest, lowest and last *sample* in
 * each period, and volume and open interest stay `null` because no size is
 * attached to any of it. Buckets are stamped with their period end, matching
 * Kalshi, so the same question drawn from two venues lands on one x-axis.
 *
 * This is the same shape of arithmetic Polymarket International needs, kept
 * separate rather than shared: that module's samples are `{t, p}` in dollars
 * and belong to another venue's file, and importing across venue modules would
 * make a change to one venue's history format a change to another's chart.
 */
export function bucketSamples(points: RawPfPoint[], interval: CandleInterval): Candle[] {
  const seconds = interval * 60;
  const buckets = new Map<number, { open: number; high: number; low: number; close: number }>();

  for (const point of points) {
    const t = figure(point.x);
    const percent = figure(point.y);
    if (t === null || percent === null) continue;
    const p = round4(percent / 100);

    // A sample on a boundary closes the period it ends, not the one it opens.
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

    // The sample spacing is coarser than the finest bucket the terminal draws
    // (two minutes at the shortest lookback), so quiet periods are bridged with
    // flat carried-forward bars rather than left as holes in the line. Bounded,
    // because a series that stops mid-window must not generate a million bars.
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

/** Flat bars bridge one gap. Beyond this the gap is left as a gap. */
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

/**
 * One tag of the venue's taxonomy, as a series row.
 *
 * The ticker is the numeric tag id and not its name, because the id is the only
 * spelling the venue's own filter accepts — `categories(filter:{tag:"6"})` is
 * the query behind a category-filtered board, and a name would have to be
 * mapped back to it on every call.
 */
export function normaliseSeries(raw: RawPfTag): SeriesInfo {
  const children = (raw.children ?? []).flatMap((child) => (child?.name ? [child.name] : []));
  return {
    venue: VENUE,
    ticker: raw.id ?? '',
    title: raw.name ?? raw.id ?? '',
    // A top-level tag files under itself, so filtering on `sports` returns
    // Sports and the fourteen sub-tags hanging off it rather than only one.
    category: raw.parent?.name ?? raw.name ?? '',
    frequency: '',
    tags: children,
  };
}

/* ---------------------------------------------------------------- queries */

/**
 * The leg fields every view needs.
 *
 * `description` is not among them, and that is the difference between a crawl
 * that weighs 750 KB a page and one that weighs 1.7 MB: the settlement rules
 * run to two kilobytes a leg and account for half of every payload they appear
 * in. Only `DES` renders them, and `DES` reads one leg through
 * {@link MARKET_QUERY}, which asks for them there. A leg read out of the
 * catalogue falls back to its category's rules, which are the same terms
 * written once for the group.
 *
 * The fee, on-chain and reward fields are left unasked because nothing in the
 * terminal reads them.
 */
const MARKET_FIELDS = `
  id title question status marketType isTradingEnabled decimalPrecision
  resolution { index name status }
  statistics { volumeTotalUsd volume24hUsd liquidity3CAskUsd }
  outcomes { edges { node { id index name status bidPriceInCurrency askPriceInCurrency statistics { sharesCount } } } }
`;

const CATEGORY_FIELDS = `
  id slug title description status marketVariant isNegRisk decimalPrecision startsAt endsAt
  statistics { volumeTotalUsd volume24hUsd liquidity3CAskUsd }
  tags(pagination: { first: 5 }) { edges { node { id name } } }
`;

const MARKET_QUERY = `query Market($id: ID!) {
  market(id: $id) {
    ${MARKET_FIELDS}
    description
    orderbook { asks bids lastOrderSettled { price side outcome kind } }
    category { ${CATEGORY_FIELDS} }
  }
}`;

const CATEGORY_QUERY = `query Category($id: ID!) {
  category(id: $id) {
    ${CATEGORY_FIELDS}
    markets(pagination: { first: 100 }) { edges { node { ${MARKET_FIELDS} } } }
  }
}`;

const TRADES_QUERY = `query Trades($marketId: ID!, $first: Int!) {
  market(id: $marketId) { id }
  matchEventLog(filter: { marketId: $marketId }, pagination: { first: $first }) {
    pageInfo { hasNextPage endCursor }
    edges { node {
      transactionHash amountFilled priceExecuted quoteType timestamp
      market { id title category { slug } }
      outcome { index name }
      account { address name }
    } }
  }
}`;

const TIMESERIES_QUERY = `query Timeseries($id: ID!, $interval: TimeseriesInterval!) {
  timeseries(categoryId: $id, filter: { interval: $interval }, pagination: { first: 100 }) {
    edges { node {
      dataGranularity
      market { id title }
      data(pagination: { first: 1000 }) { edges { node { x y } } }
    } }
  }
}`;

const TAGS_QUERY = `query Tags {
  categoryTags(pagination: { first: 100 }) {
    edges { node {
      id name
      open: totalCount(filter: { status: OPEN })
      parent { id name }
      children { id name }
    } }
  }
}`;

const CRAWL_QUERY = `query Crawl($after: String) {
  categories(filter: { status: OPEN }, sort: VOLUME_24H_DESC, pagination: { first: 100, after: $after }) {
    totalCount
    pageInfo { hasNextPage endCursor }
    edges { node {
      ${CATEGORY_FIELDS}
      markets(pagination: { first: 100 }) { edges { node { ${MARKET_FIELDS} } } }
    } }
  }
}`;

/* ----------------------------------------------------------------- lookups */

const IDENTIFIER_HINT =
  'predict.fun names a contract `<category slug>~<leg id>`, as in ' +
  '`pf:big-game-champion-2027~26952`. The category slug alone works when the ' +
  'category has a single leg, and the bare leg id (`pf:26952`) always works.';

async function rawMarket(marketId: string): Promise<RawPfMarket> {
  const data = await gql<{ market?: RawPfMarket | null }>(
    `market:${marketId}`,
    TTL.quote,
    MARKET_QUERY,
    { id: marketId },
  );
  if (!data.market) {
    throw new UpstreamError(`No predict.fun market with id ${marketId}`, {
      code: 'not_found',
      hint: IDENTIFIER_HINT,
    });
  }
  return data.market;
}

async function rawCategory(slug: string): Promise<RawPfCategory> {
  const data = await gql<{ category?: RawPfCategory | null }>(
    `category:${slug}`,
    TTL.quote,
    CATEGORY_QUERY,
    { id: slug },
  );
  if (!data.category) {
    throw new UpstreamError(`No predict.fun category with slug ${slug}`, {
      code: 'not_found',
      hint: IDENTIFIER_HINT,
    });
  }
  return data.category;
}

/**
 * Turn whatever the trader typed into the leg id the API answers to.
 *
 * Three spellings reach here and all three are worth accepting. The full
 * ticker is the canonical one. A bare leg id is what the venue itself prints in
 * its book and websocket topics. A bare category slug is what the website URL
 * shows, and for the 266 single-leg categories it names exactly one contract —
 * for the rest it names an event, which is a different command, so it is
 * refused with the legs listed rather than resolved to an arbitrary one.
 *
 * The slug is never passed to `market(id:)`: that argument takes integers only
 * and answers anything else with an error, not a miss.
 */
async function resolveLeg(reference: string): Promise<string> {
  const parsed = parseTicker(reference);
  const candidate = parsed ? parsed.marketId : reference;
  if (/^\d+$/.test(candidate)) return candidate;

  const slug = parsed ? parsed.slug : reference;
  const category = await rawCategory(slug);
  const legs = nodes(category.markets);
  const only = legs[0];
  if (legs.length === 1 && only?.id) return only.id;

  const sample = legs
    .slice(0, 3)
    .map((leg) => `${makeTicker(slug, leg.id ?? '')} (${leg.title ?? 'unnamed'})`)
    .join(', ');
  throw new UpstreamError(
    `"${slug}" is a predict.fun category with ${legs.length} legs, not a single contract`,
    {
      code: 'not_found',
      hint: `Use \`EVT pf:${slug}\` for the whole event, or name a leg: ${sample}…`,
    },
  );
}

export async function getMarket(reference: string): Promise<Market> {
  const raw = await rawMarket(await resolveLeg(reference));
  return normaliseMarket(raw);
}

export async function getEvent(reference: string): Promise<VenueEvent> {
  // A trader who has a contract ticker in hand and wants its event should not
  // have to trim it by hand; the slug is the part before the separator.
  const slug = parseTicker(reference)?.slug ?? reference;
  return normaliseEvent(await rawCategory(slug));
}

export async function getOrderBook(reference: string, depth = 12): Promise<OrderBook> {
  const marketId = await resolveLeg(reference);
  const raw = await rawMarket(marketId);
  return normaliseOrderBook(raw, makeTicker(raw.category?.slug ?? '', marketId), depth);
}

export async function getTrades(reference: string, limit = 50): Promise<TradesResponse> {
  const marketId = await resolveLeg(reference);
  // The connection clamps `first` to 100 without saying so; asking for more
  // would report a shorter tape than the caller thinks it received.
  const first = Math.min(Math.max(limit, 1), 100);

  const data = await gql<{
    market?: { id?: string } | null;
    matchEventLog?: RawPfConnection<RawPfTrade> | null;
  }>(`trades:${marketId}:${first}`, TTL.quote, TRADES_QUERY, { marketId, first });

  // The tape connection answers an unknown leg with an empty page rather than
  // an error, so the leg is confirmed in the same round trip: a quiet market
  // and a mistyped id must not both read as a market nobody trades.
  if (!data.market) {
    throw new UpstreamError(`No predict.fun market with id ${marketId}`, {
      code: 'not_found',
      hint: IDENTIFIER_HINT,
    });
  }

  const log = data.matchEventLog ?? {};
  return {
    trades: normaliseTrades(log, marketId),
    cursor: log.pageInfo?.hasNextPage ? (log.pageInfo.endCursor ?? null) : null,
  };
}

/**
 * The lookback windows the history query accepts, shortest first.
 *
 * There is no `from`/`to` on this API: the only knob is one of six fixed
 * windows ending at now, each with its own sample spacing (two minutes on the
 * hour windows, ten on the day, an hour on the week, a day beyond that). So a
 * requested range is served by the shortest window that reaches back far enough
 * and then clipped — asking for the widest every time would trade the whole
 * chart's resolution for a range the caller did not want.
 */
const LOOKBACKS: readonly { token: string; seconds: number }[] = [
  { token: '_1H', seconds: 3_600 },
  { token: '_3H', seconds: 10_800 },
  { token: '_1D', seconds: 86_400 },
  { token: '_7D', seconds: 604_800 },
  { token: '_30D', seconds: 2_592_000 },
  { token: 'MAX', seconds: Number.POSITIVE_INFINITY },
];

export async function getCandles(
  reference: string,
  interval: CandleInterval,
  startTs: number,
  endTs: number,
): Promise<CandlesResponse> {
  const marketId = await resolveLeg(reference);
  const raw = await rawMarket(marketId);
  const slug = raw.category?.slug ?? raw.category?.id ?? '';
  const ticker = makeTicker(slug, marketId);

  const span = Math.max(Math.floor(Date.now() / 1000) - startTs, 1);
  const lookback = LOOKBACKS.find((w) => w.seconds >= span) ?? LOOKBACKS[LOOKBACKS.length - 1]!;

  // History is published per category, one series per leg with no way to ask
  // for one — a 32-leg category is 160 KB to chart a single team — so the whole
  // set is cached under the category and every leg's chart shares one read.
  const data = await gql<{ timeseries?: RawPfConnection<RawPfSeries> | null }>(
    `timeseries:${slug}:${lookback.token}`,
    TTL.candles,
    TIMESERIES_QUERY,
    { id: slug, interval: lookback.token },
    30_000,
  );

  const series = nodes(data.timeseries).find((s) => s.market?.id === marketId);
  if (!series) {
    throw new UpstreamError(`predict.fun publishes no price history for ${ticker}`, {
      code: 'not_found',
      hint:
        `timeseries(categoryId: "${slug}") returned no series for leg ${marketId}. ` +
        'History appears once a leg has traded.',
    });
  }

  const points = nodes(series.data).filter((p) => {
    const t = figure(p.x);
    return t !== null && t >= startTs && t <= endTs;
  });

  return {
    venue: VENUE,
    ticker,
    seriesTicker: seriesFromSlug(slug),
    interval,
    candles: bucketSamples(points, interval),
    note:
      'predict.fun publishes a probability sample series rather than OHLC: these bars are its ' +
      `samples bucketed, and carry no volume or open interest. History is served only as fixed ` +
      `lookback windows ending at now — this chart is its "${lookback.token}" window clipped to the ` +
      'range requested, so it may start later, or be sampled more coarsely, than asked for.',
  };
}

/**
 * The venue's tag taxonomy, which is what it has instead of series.
 *
 * There is no series object here — recurring questions are related only by the
 * shape of their slugs, which {@link seriesFromSlug} already reads. What the
 * venue does publish is a two-level tag tree with a live count per tag, and
 * that is the thing a category-filtered board is actually asking for.
 */
export async function listSeries(category?: string): Promise<SeriesInfo[]> {
  const data = await gql<{ categoryTags?: RawPfConnection<RawPfTag> | null }>(
    'tags',
    TTL.catalogue,
    TAGS_QUERY,
  );

  // Only tags with something open behind them: the tree keeps 91 tags but a
  // third of them have no live category, and offering those as filters means
  // offering a filter that returns nothing.
  const series = nodes(data.categoryTags)
    .filter((tag) => (tag.open ?? 0) > 0)
    .map(normaliseSeries);

  if (!category) return series;
  const wanted = category.trim().toLowerCase();
  return series.filter(
    (s) => s.category.toLowerCase() === wanted || s.title.toLowerCase() === wanted,
  );
}

/* ----------------------------------------------------------------- corpus */

const CORPUS_KEY = 'predictfun:corpus';

/**
 * Pages of open categories, ordered by 24h turnover.
 *
 * The page size is fixed at the 100 the API silently enforces — it clamps
 * anything larger without saying so, and a crawl that asked for 500 would
 * quietly walk a fifth of the catalogue while believing it had walked it all.
 *
 * The open universe is about 1,085 categories and 7,800 legs, and each page of
 * 100 with every leg attached is ~750 KB and two to three seconds — so a full
 * crawl is eleven pages and about twelve seconds, once per catalogue TTL. The cap
 * sits just past the whole universe rather than under it, so the ordinary case
 * completes and a venue that doubles overnight truncates instead of running
 * for minutes.
 *
 * Volume ordering is what makes the truncation principled: what a cap drops is
 * the untraded tail, not an arbitrary slice of the middle.
 */
const CORPUS_MAX_PAGES = 14;

async function crawlPage(after: string | null): Promise<RawPfConnection<RawPfCategory>> {
  const data = await gql<{ categories?: RawPfConnection<RawPfCategory> | null }>(
    `crawl:${after ?? 'first'}`,
    TTL.catalogue,
    CRAWL_QUERY,
    { after },
    40_000,
  );
  return data.categories ?? {};
}

async function buildCorpus(): Promise<Corpus> {
  const events: VenueEvent[] = [];
  const markets: Market[] = [];
  const seen = new Set<string>();
  let cursor: string | null = null;
  let truncated = false;

  for (let page = 0; page < CORPUS_MAX_PAGES; page++) {
    const connection: RawPfConnection<RawPfCategory> = await crawlPage(cursor);
    for (const raw of nodes(connection)) {
      const key = raw.slug ?? raw.id ?? '';
      if (!key || seen.has(key)) continue;
      seen.add(key);
      const event = normaliseEvent(raw);
      events.push(event);
      markets.push(...event.markets);
    }

    const info = connection.pageInfo;
    if (!info?.hasNextPage || !info.endCursor) break;
    cursor = info.endCursor;
    // `totalCount` is real on this connection but null on nearly every other,
    // so the flag is driven by where the walk stopped rather than by counts.
    if (page === CORPUS_MAX_PAGES - 1) truncated = true;
  }

  return { venue: VENUE, events, markets, builtAt: Date.now(), truncated };
}

export async function corpusSnapshot(): Promise<Corpus> {
  return cache.cached(CORPUS_KEY, TTL.catalogue, buildCorpus);
}

export function warmCorpus(): void {
  corpusSnapshot()
    .then((c) => {
      console.log(`[predictfun] corpus warm: ${c.events.length} events, ${c.markets.length} markets`);
    })
    .catch((err: unknown) => {
      console.warn('[predictfun] corpus warm failed:', err instanceof Error ? err.message : err);
    });
}

/**
 * Search the snapshot rather than the venue's own `search` query.
 *
 * That query exists and is respectable — it answers `fed` with the Fed rate
 * ladder — but it scores by its own rules and cannot be made to score by
 * anyone else's. Ranking the local snapshot the way every other venue is ranked
 * is what makes a cross-venue result list comparable instead of six lists
 * stapled together.
 */
export async function search(query: string, limit = 25): Promise<SearchResponse> {
  return searchCorpus(await corpusSnapshot(), query, limit);
}

/**
 * Leaderboards over the snapshot.
 *
 * Turnover, resting depth and — under the name `sharesCount` — open interest
 * are all stated per leg in the catalogue, so those three boards rank on the
 * venue's own numbers. The movers do not: the only 24h figure here is a range
 * with no direction (see `percentageChanceChange24h` on
 * {@link RawPfMarketStatistics}), `Market.change` is therefore unstated, and
 * {@link rankMarkets} drops a market rather than ranking an unpublished figure
 * as zero. `TOP gainers` and `TOP losers` come back empty on this venue until
 * the venue publishes a signed move or the crawl can afford the per-category
 * history that would reconstruct one.
 */
export async function topMarkets(sort: MoverSort, limit = 25): Promise<Market[]> {
  return rankMarkets((await corpusSnapshot()).markets, sort, limit);
}
