/**
 * Gemini prediction markets (gemini.com/predictions) client.
 *
 * The exchange runs its prediction book on the same matching engine as its
 * crypto pairs, and it shows in the API: everything the terminal needs is
 * public, unauthenticated and split across two hosts that speak different
 * dialects.
 *
 *   www.gemini.com/prediction-markets   catalogue and single event. Names,
 *                                       rules, strikes, settlement, turnover.
 *                                       Every figure is a decimal *string*.
 *   api.gemini.com                      book, tape and OHLC bars, keyed on the
 *                                       instrument symbol. Strings on the v1
 *                                       paths, plain numbers on the v2 ones.
 *
 * Two identifiers, both upper-case and both case-insensitive upstream. An event
 * is its ticker (`DEMNOM2028`, `NFL-2608212330-CAR-JAX-M`); a contract is its
 * `instrumentSymbol` (`GEMI-DEMNOM2028-DEMNOM28AOC`), which is the only thing
 * api.gemini.com answers to and the only field that names a leg uniquely.
 *
 * What it costs is one request: `?limit=500&status=active` returns the whole
 * open universe — ~409 events and ~2,865 contracts, 10 MB decoded but 660 KB on
 * the wire — so the corpus crawl is a single call rather than the paginated
 * per-category walk the other brokers need.
 *
 * What it never states is turnover per contract, open interest anywhere, and
 * any resting-depth figure outside the book itself. Those stay `null`, and the
 * two boards that would have ranked on them are empty by declaration in the
 * venue registry rather than by accident here.
 */

import type {
  BookLevel,
  Candle,
  CandleInterval,
  CandlesResponse,
  Market,
  MarketStatus,
  OrderBook,
  SeriesInfo,
  StrikeType,
  Trade,
  TradesResponse,
  VenueEvent,
} from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';
import { searchCorpus, type Corpus, type MoverSort, type SearchResponse } from './corpus.js';

const CATALOGUE = process.env.GEMINI_CATALOGUE_BASE ?? 'https://www.gemini.com';
const API = process.env.GEMINI_API_BASE ?? 'https://api.gemini.com';

const VENUE = 'gemini' as const;

/* --------------------------------------------------------------- coercion */

/** Read a decimal string, or a number that already is one. */
export function num(value: unknown): number | null {
  if (value === null || value === undefined || value === '') return null;
  const n = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(n) ? n : null;
}

/**
 * As {@link num}, but a price of zero is the absence of one.
 *
 * `priceMinimum` is $0.01 on all 2,995 contracts and the tick is a cent, so
 * nothing can rest at zero. A zero in a price field is an empty side wearing a
 * number's clothes, and it has to reach the panel as `--` rather than as a
 * contract someone will sell you for nothing.
 */
function quote(value: unknown): number | null {
  const n = num(value);
  return n === null || n === 0 ? null : n;
}

/** Kill binary-float dust from the `1 - p` inversions. */
function round4(n: number): number {
  return Math.round(n * 10_000) / 10_000;
}

/* ------------------------------------------------------------ raw upstream */

/** How every contract states its rules: a Contentful rich-text document. */
export interface RawGeminiRichText {
  content?: { value?: string }[];
}

export interface RawGeminiPrices {
  /**
   * Present on every contract, and deliberately unread — see
   * {@link normaliseMarket}. Where a book exists these restate it; where none
   * does they hold an indicative mark instead.
   */
  buy?: { yes?: string; no?: string };
  sell?: { yes?: string; no?: string };
  /** Absent, key and all, on 678 of 2,995 contracts. */
  bestBid?: string;
  bestAsk?: string;
  /** Absent on 1,509 of 2,995 — never traded, or settled. */
  lastTradePrice?: string;
}

export interface RawGeminiStrike {
  /** `above` | `under_or_equal` | `below` | `reference`. */
  type?: string;
  /** A decimal string on 429 of 456 strikes, and label debris on the other 27. */
  value?: string;
}

export interface RawGeminiContract {
  id?: string;
  label?: string;
  abbreviatedName?: string;
  ticker?: string;
  /** The authoritative identifier. Never rebuild it from the two tickers. */
  instrumentSymbol?: string;
  description?: RawGeminiRichText;
  prices?: RawGeminiPrices;
  status?: string;
  marketState?: string;
  effectiveDate?: string;
  expiryDate?: string;
  /** A percentage of the older price, not a dollar delta. */
  priceDelta24hPct?: string;
  priceDelta1hPct?: string;
  /** `yes` or `no`, on exactly the 262 settled contracts. */
  resolutionSide?: string;
  strike?: RawGeminiStrike;
  /** The venue's own display order for the leg — see {@link normaliseEvent}. */
  sortOrder?: number;
}

export interface RawGeminiEvent {
  id?: string;
  ticker?: string;
  slug?: string;
  title?: string;
  /** `categorical` or `binary`. Not an exclusivity flag — see {@link normaliseEvent}. */
  type?: string;
  category?: string;
  subcategory?: { slug?: string; name?: string };
  status?: string;
  /** Stated on 64 of 436 events, and authoritative where it is. */
  series?: string;
  tags?: string[];
  /** Contracts traded, ever and in the last 24h. Stated per event only. */
  volume?: string;
  volume24h?: string;
  volumeDelta24hPct?: string;
  effectiveDate?: string;
  expiryDate?: string;
  /** Start of the observed period, not of trading — see {@link normaliseMarket}. */
  startTime?: string;
  resolvedAt?: string;
  contracts?: RawGeminiContract[];
}

export interface RawGeminiCatalogue {
  data?: RawGeminiEvent[];
  pagination?: { limit?: number; offset?: number; total?: number };
}

export interface RawGeminiLevel {
  price?: string;
  /** Contracts, and fractional on the 1,049 instruments that trade in hundredths. */
  amount?: string;
  /** The snapshot time, repeated on every level rather than a per-level age. */
  timestamp?: string;
}

export interface RawGeminiBook {
  bids?: RawGeminiLevel[];
  asks?: RawGeminiLevel[];
}

export interface RawGeminiTrade {
  /** Unix seconds. `timestampms` is the same instant in milliseconds. */
  timestamp?: number;
  timestampms?: number;
  tid?: number;
  price?: string;
  amount?: string;
  /** `buy` or `sell`. The payload says nothing further about what it means. */
  type?: string;
}

export interface RawGeminiTicker {
  symbol?: string;
  /** The price 24h ago, and the current one. */
  open?: string;
  close?: string;
  high?: string;
  low?: string;
  /** 24 hourly closes of disputed order — see {@link applyTicker}. */
  changes?: string[];
  bid?: string;
  ask?: string;
}

/** `[periodStartMs, open, high, low, close, volume]`, all plain numbers. */
export type RawGeminiKline = number[];

/* ------------------------------------------------------------ normalisers */

/**
 * `active`/`open` → open, `settled`/`closed` → settled.
 *
 * A contract's `status` and `marketState` agreed on all 2,995 rows, so either
 * would serve. Anything the exchange starts saying that is neither — its input
 * validator also names `approved` and `under_review`, and neither appeared —
 * passes through in the venue's own word rather than being folded into whichever
 * of ours looks closest, because a status nobody has seen is worth reading
 * literally.
 */
export function statusOf(raw: RawGeminiContract): MarketStatus {
  const stated = (raw.status ?? raw.marketState ?? '').toLowerCase();
  if (stated === 'active' || stated === 'open') return 'open';
  if (stated === 'settled' || stated === 'closed') return 'settled';
  return stated || 'open';
}

/**
 * The rules text, which arrives as a document rather than a string.
 *
 * All 2,995 contracts state their terms as a Contentful rich-text tree and not
 * one states them as prose, so there is no string spelling to prefer: without
 * flattening, every rules pane in the terminal reads `[object Object]`.
 */
export function flattenRules(raw: RawGeminiRichText | undefined): string {
  return (raw?.content ?? []).map((node) => node.value ?? '').join('');
}

interface Strike {
  strikeType: StrikeType | null;
  floorStrike: number | null;
  capStrike: number | null;
}

const NO_STRIKE: Strike = { strikeType: null, floorStrike: null, capStrike: null };

/**
 * The structured strike, when the number in it is a number.
 *
 * 456 contracts carry a `{type, value}` strike and 27 of them carry debris from
 * whatever generated the label instead of a figure — "Lockheed Martin" arrives
 * as `{type:'below', value:'CKHEE.'}` and "Hike 25bps" as `{value:'KE25'}`. So
 * the value is parsed before the type is believed, and a strike that fails to
 * parse leaves the contract with none: a `NaN` bound would sort to one end of
 * every ladder it appeared in and price a spread against nothing.
 *
 * `above` and `below` are the exchange's shorthand for bounds that include
 * their own edge, which the labels and the rules text agree on: every numeric
 * `above` strike is a "$61,000 or above" rung whose terms read "if the price of
 * Bitcoin is $61,000 or above … this market will resolve to Yes", and every
 * numeric `below` one is a "74°F or below" weather bucket worded the same way.
 * So both are the inclusive bound, the same one `under_or_equal` states for a
 * golf top-10 finish. Reading them as strict would put the settlement price
 * itself on the losing side of a rung that pays on it, and the two halves of a
 * ladder would then leave a gap at every tick. `reference` is the level a
 * five-minute up/down book is quoted against rather than a bound anything
 * settles over, so it is no strike at all.
 */
export function strikeOf(raw: RawGeminiStrike | undefined): Strike {
  const value = num(raw?.value);
  if (value === null) return NO_STRIKE;

  switch (raw?.type) {
    case 'above':
      return { strikeType: 'greater_or_equal', floorStrike: value, capStrike: null };
    case 'under_or_equal':
    case 'below':
      return { strikeType: 'less_or_equal', floorStrike: null, capStrike: value };
    default:
      return NO_STRIKE;
  }
}

/**
 * The price 24h ago, recovered from the percentage the venue states instead.
 *
 * `priceDelta24hPct` is a percentage *of the older price*, so assigning it to
 * `change` would print a 10-dollar move on a 22¢ contract that ticked up two
 * cents. Inverting it gives the older price, and the subtraction gives the move
 * in dollars. 262 contracts state no percentage at all; they get no previous
 * price rather than a flat one, so `change` reads `--` instead of `0.00`.
 */
export function previousFrom(lastPrice: number | null, deltaPct: unknown): number | null {
  const pct = num(deltaPct);
  if (lastPrice === null || pct === null) return null;

  const factor = 1 + pct / 100;
  // A contract that lost all of its value states no older price this can reach.
  if (factor === 0) return null;
  return round4(lastPrice / factor);
}

/** What a leg inherits from the event that owns it. */
export interface GeminiParent {
  eventTicker: string;
  seriesTicker: string;
  title: string;
  category: string;
  marketType: string;
  openTime: string;
  closeTime: string;
  /**
   * The event's turnover, and only when the event is a single contract.
   * {@link normaliseEvent} decides; see the reasoning there.
   */
  volume: number | null;
  volume24h: number | null;
}

/**
 * One leg of an event, quoted from `bestBid` and `bestAsk`.
 *
 * **`prices.buy` and `prices.sell` are not read, on purpose.** Where a book
 * exists they restate it exactly — `buy.yes` equalled `bestAsk` on all 2,317
 * contracts carrying both, and `sell.no` equalled `1 - bestAsk` to the cent on
 * every one. But on the 142 contracts with nothing resting they hold an
 * indicative mark instead, with `buy.yes === sell.yes === lastTradePrice`.
 * Reading them would put a two-sided quote at a zero spread on a contract
 * nobody is quoting, which is the one thing a book panel must never do.
 * `bestBid`/`bestAsk` are simply absent on those rows, and absent is the truth.
 *
 * The NO side is derived rather than read for the same reason: there is one
 * instrument per leg, quoted in YES terms, and the venue's own `buy.no` and
 * `sell.no` are that mirror already.
 */
export function normaliseMarket(raw: RawGeminiContract, parent: GeminiParent): Market {
  const prices = raw.prices ?? {};
  const yesBid = quote(prices.bestBid);
  const yesAsk = quote(prices.bestAsk);
  const lastPrice = quote(prices.lastTradePrice);
  const previousPrice = previousFrom(lastPrice, raw.priceDelta24hPct);
  const strike = strikeOf(raw.strike);

  return {
    venue: VENUE,
    ticker: raw.instrumentSymbol ?? '',
    eventTicker: parent.eventTicker,
    seriesTicker: parent.seriesTicker,
    title: parent.title,
    yesSubTitle: raw.label ?? raw.abbreviatedName ?? raw.ticker ?? '',
    // The exchange words one side of a leg and never the other, even on the 17
    // one-contract events where the label is literally "Yes".
    noSubTitle: 'No',
    status: statusOf(raw),
    marketType: parent.marketType,
    yesBid,
    yesAsk,
    noBid: yesAsk === null ? null : round4(1 - yesAsk),
    noAsk: yesBid === null ? null : round4(1 - yesBid),
    mid:
      yesBid !== null && yesAsk !== null
        ? round4((yesBid + yesAsk) / 2)
        : (lastPrice ?? yesBid ?? yesAsk),
    lastPrice,
    previousPrice,
    change:
      lastPrice !== null && previousPrice !== null ? round4(lastPrice - previousPrice) : null,
    volume: parent.volume,
    volume24h: parent.volume24h,
    // No endpoint on either host carries open interest — not the event, the
    // contract, the book, the ticker, the tape or a candle row.
    openInterest: null,
    // Not published either, but the book states it for anyone who asks for the
    // book: {@link getMarket} adds it up there rather than across 2,865 crawls.
    liquidity: null,
    // `effectiveDate` is when this leg opened. The event's `startTime` names the
    // start of the period being observed — kickoff, or the first tick of a price
    // window — which on the Carolina–Jacksonville game is ten days after the
    // book opened, so reading it here would say the contract had not started
    // trading for most of the time it traded.
    openTime: raw.effectiveDate ?? parent.openTime,
    closeTime: parent.closeTime,
    expirationTime: raw.expiryDate ?? parent.closeTime,
    // Stated per leg rather than one winner per event, and it has to stay that
    // way: a cumulative ladder settles several legs YES, and one two-leg
    // head-to-head in the snapshot resolved YES on both.
    result: raw.resolutionSide === 'yes' || raw.resolutionSide === 'no' ? raw.resolutionSide : '',
    rulesPrimary: flattenRules(raw.description),
    ...(parent.category ? { category: parent.category } : {}),
    ...strike,
  };
}

/**
 * Strip the settlement date out of an event ticker.
 *
 * Six digits is the load-bearing part of the rule. Tickers embed their date as
 * `YYMMDD` or `YYMMDDHHMM`, so six or more digits in a row is a date and
 * anything shorter is part of the question: a `\d{2,}` threshold would take the
 * state off `SEN26MI` and file all 36 Senate races as one race.
 */
export function stripEventDate(ticker: string): string {
  return ticker
    .replace(/\d{6,}/g, '')
    .replace(/-{2,}/g, '-')
    .replace(/^-|-$/g, '');
}

/**
 * The series a recurring question belongs to.
 *
 * Gemini states one on 64 of 436 events — its crypto, metals, energy and
 * weather ladders — and those are exactly the events where guessing goes wrong,
 * so a stated series is preferred. `BTC2608212100` says `BTC1H` while
 * `BTC2608180600` says `BTC`: an hourly bitcoin ladder and a daily one are
 * different products, and stripping the date from both fuses them into one
 * series that would then be matched against a single market at the next broker.
 *
 * Everywhere else the ticker carries its own date and removing it leaves the
 * question: `FED260917` and `FED260729` are both `FED`, one series with two
 * open expiries.
 *
 * The two disagree in one direction that the stated name loses. Gemini files
 * all seventeen cities' daily highs under the single product `WXHIGH`, but a
 * series here is a recurring *question*, and "the high in Chicago tomorrow" is
 * not the same question as "the high in Miami tomorrow" — grouping them makes
 * sixteen of the seventeen unreachable from `XV`, since one event has to stand
 * for the family. So where the stripped ticker *extends* the stated name, the
 * longer one wins: `WXHIGH-CHI` over `WXHIGH`. Where the stated name extends
 * the stripped one it still wins, which is what keeps `BTC1H` apart from `BTC`.
 * Anything else is a name the ticker does not resemble, and the venue's own is
 * the safer of the two.
 */
export function seriesFromEvent(raw: RawGeminiEvent): string {
  const ticker = raw.ticker ?? '';
  const stripped = stripEventDate(ticker) || ticker;
  const stated = raw.series;
  if (!stated) return stripped;
  return stripped.startsWith(`${stated}-`) ? stripped : stated;
}

/**
 * One event with its legs.
 *
 * The single-event endpoint and a catalogue row carry the same event shape —
 * the single one adds a `featuredImageUrl` the catalogue omits and nothing
 * else that is read here — so one normaliser serves both paths.
 */
export function normaliseEvent(raw: RawGeminiEvent): VenueEvent {
  const contracts = raw.contracts ?? [];
  const eventTicker = raw.ticker ?? '';

  /*
   * Turnover is stated once per event, in contracts, and never per leg. Copying
   * the event's figure onto each of DEMNOM2028's 12 legs would have
   * `sumOrNull` report 12× the true turnover in the search panel and `TOP` show
   * twelve identical rows, so a leg claims it only when it *is* the event —
   * the 17 one-contract books, where the two figures are the same figure.
   */
  const sole = contracts.length === 1;

  /*
   * The array order is not the exchange's order. 148 of the 239 events that
   * number every leg disagree with their own numbering, and the disagreement
   * shows: `WXHIGH-LA` arrives as 79-80°F, 81-82°F, 77-78°F, 83°F or above,
   * 74°F or below, 75-76°F, where its `sortOrder` reads the ladder from cold to
   * hot. A strike ladder out of sequence is unreadable, so an event that
   * numbers all of its legs is read in that order — and one that numbers only
   * some is left alone rather than half-sorted around the gaps.
   */
  const ordered = contracts.every((c) => typeof c.sortOrder === 'number')
    ? [...contracts].sort((a, b) => (a.sortOrder ?? 0) - (b.sortOrder ?? 0))
    : contracts;

  const parent: GeminiParent = {
    eventTicker,
    seriesTicker: seriesFromEvent(raw),
    title: raw.title ?? eventTicker,
    category: raw.category ?? '',
    marketType: raw.type ?? 'binary',
    openTime: raw.effectiveDate ?? '',
    closeTime: raw.expiryDate ?? '',
    volume: sole ? num(raw.volume) : null,
    volume24h: sole ? num(raw.volume24h) : null,
  };

  return {
    venue: VENUE,
    eventTicker,
    seriesTicker: parent.seriesTicker,
    title: parent.title,
    // The event carries a `shortSummary`, and it is written commentary that is
    // regenerated through the day — a subtitle that changes under the reader.
    subTitle: '',
    category: parent.category,
    /*
     * Never true, and `type` is the trap that makes it look derivable.
     * `categorical` covers both DEMNOM2028, whose 12 legs are a genuine
     * winner-take-all field summing to $0.98, and BTC2608212100, whose legs are
     * "$61,000 or above", "$61,500 or above", "$62,000 or above" — a nested
     * ladder summing to $6.80, where every leg above the settlement price pays.
     * The prose marker is no better: 159 of 436 events say "This event is
     * mutually exclusive" in their rules and the two-leg Senate control book
     * and every NFL head-to-head are not among them. `EVT` only draws its Σmid
     * arbitrage check when told the legs exclude each other, so a guessed flag
     * puts a false arbitrage on screen.
     */
    mutuallyExclusive: false,
    markets: ordered.map((contract) => normaliseMarket(contract, parent)),
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

/** The catalogue host: names, rules and prices, all as strings. */
async function catalogue<T>(path: string, ttlMs: number): Promise<T> {
  return cache.cached(`gemini:${path}`, ttlMs, () =>
    fetchJson<T>(`${CATALOGUE}/prediction-markets${path}`, { timeoutMs: 30_000, retries: 2 }),
  );
}

/** The trading host: book, tape and bars, keyed on the instrument symbol. */
async function api<T>(path: string, ttlMs: number): Promise<T> {
  return cache.cached(`gemini:api:${path}`, ttlMs, () =>
    fetchJson<T>(`${API}${path}`, { timeoutMs: 20_000, retries: 2 }),
  );
}

/**
 * The 404 both lookups answer with, worded so the reader can retype it.
 *
 * api.gemini.com says no to an unknown symbol with a 400 rather than a 404
 * ("is not a valid symbol"), which would otherwise reach the panel as a
 * transport failure instead of a typo.
 */
function missing(id: string, what: 'event' | 'contract'): UpstreamError {
  return new UpstreamError(`No Gemini ${what} ${id}`, {
    code: 'not_found',
    hint:
      what === 'event'
        ? 'Gemini identifies an event by its ticker, as in `gem:DEMNOM2028` or ' +
          '`gem:NFL-2608212330-CAR-JAX-M`. The slug in the web address is not a lookup key, ' +
          'and is not unique — three different events share `btc-price-today-at-2am-edt`.'
        : 'Gemini identifies a contract by its instrument symbol, as in ' +
          '`gem:GEMI-DEMNOM2028-DEMNOM28AOC`. `EVT` on the parent event lists the symbols ' +
          'of its legs.',
  });
}

/**
 * The trading host said no to a symbol, and the catalogue says which no it was.
 *
 * api.gemini.com answers `is not a valid symbol` both for a mistyped symbol and
 * for a contract it has simply never opened an instrument for — around a third
 * of the legs that have never been quoted and never traded, every leg of
 * `HORMUZNORMAL` among them, all of which the catalogue lists and `EVT` prints.
 * Both arrive as a 400, so the snapshot is consulted before the reader is told
 * to check the spelling of a symbol the terminal handed them a moment ago.
 */
async function rejected(symbol: string): Promise<UpstreamError> {
  const listed = await corpusSnapshot()
    .then((c) => c.markets.some((m) => m.ticker === symbol))
    .catch(() => false);
  if (!listed) return missing(symbol, 'contract');

  return new UpstreamError(`Gemini has opened no instrument for ${symbol}`, {
    code: 'not_found',
    hint:
      `${symbol} is listed in the Gemini catalogue, but api.gemini.com answers ` +
      '`is not a valid symbol` for it, which is what it says about a contract that has ' +
      'never been quoted and never traded. `DES` shows what the catalogue states; the ' +
      'book, tape and bars have nothing to read until the leg opens.',
  });
}

/** Ask api.gemini.com about one instrument, reading its rejection as a typo. */
async function instrument<T>(symbol: string, path: string, ttlMs: number): Promise<T> {
  try {
    return await api<T>(path, ttlMs);
  } catch (err) {
    if (err instanceof UpstreamError && (err.status === 400 || err.status === 404)) {
      throw await rejected(symbol);
    }
    throw err;
  }
}

/* ----------------------------------------------------------------- lookups */

export async function getEvent(ticker: string): Promise<VenueEvent> {
  let raw: RawGeminiEvent;
  try {
    raw = await catalogue<RawGeminiEvent>(`/${encodeURIComponent(ticker)}`, TTL.quote);
  } catch (err) {
    if (err instanceof UpstreamError && err.code === 'not_found') throw missing(ticker, 'event');
    throw err;
  }
  if (!raw.ticker) throw missing(ticker, 'event');
  return normaliseEvent(raw);
}

/**
 * Which event owns an instrument symbol.
 *
 * The open snapshot answers instantly, and answers nearly every lookup. It
 * cannot answer for a leg of a settled event — the crawl reads `status=active`
 * — and `EVT` on a settled event hands the reader exactly those symbols, so
 * refusing them would make the terminal reject identifiers it had just printed.
 * The symbol's own prefixes are tried against the single-event endpoint
 * instead, longest first: `GEMI-BTC2608171800-HI63700` asks for
 * `BTC2608171800-HI63700`, is told there is no such event, then asks for
 * `BTC2608171800` and is given it. Only the event half of the symbol is ever
 * used, which is why the four legs whose symbol does not contain their own
 * contract ticker are found as readily as the rest, and why a hyphenated event
 * ticker costs one wasted request rather than a wrong answer.
 */
async function eventTickerFor(symbol: string): Promise<string> {
  const known = (await corpusSnapshot()).markets.find((m) => m.ticker === symbol);
  if (known) return known.eventTicker;

  const parts = symbol.replace(/^GEMI-/i, '').split('-');
  for (let take = parts.length - 1; take >= 1; take--) {
    const candidate = parts.slice(0, take).join('-');
    try {
      const raw = await catalogue<RawGeminiEvent>(`/${encodeURIComponent(candidate)}`, TTL.quote);
      if ((raw.contracts ?? []).some((c) => c.instrumentSymbol === symbol)) return candidate;
    } catch {
      // Naming no event is the expected answer for every candidate but one.
    }
  }
  throw missing(symbol, 'contract');
}

/**
 * One contract, recovered through the event that owns it.
 *
 * An instrument symbol is not fetchable on its own: api.gemini.com quotes it
 * but publishes no name, no rules and no parent. So {@link eventTickerFor}
 * finds the owning event, the event is read for live prices, and the leg is
 * picked out of it — the recovery `polymarketus.getMarket` does for a parent,
 * one step longer because there is no by-symbol endpoint to start from.
 *
 * The two api.gemini.com sidecars are best-effort. The book adds the resting
 * depth nothing in the catalogue states; `/v2/ticker` adds the only 24h open
 * published per contract. `/v2/ticker` 404s on an instrument that has never
 * traded — a fact about the contract, not a failure — so neither is allowed to
 * take the quote down with it.
 */
export async function getMarket(symbol: string): Promise<Market> {
  const event = await getEvent(await eventTickerFor(symbol));
  const leg = event.markets.find((m) => m.ticker === symbol);
  if (!leg) throw missing(symbol, 'contract');

  const [book, ticker] = await Promise.all([
    api<RawGeminiBook>(`/v1/book/${encodeURIComponent(symbol)}`, TTL.quote).catch(() => null),
    api<RawGeminiTicker>(`/v2/ticker/${encodeURIComponent(symbol)}`, TTL.quote).catch(() => null),
  ]);

  return applyTicker(applyBook(leg, book), ticker);
}

/**
 * Add the resting depth the exchange never states.
 *
 * No endpoint publishes a liquidity figure, but the book is public and weighs a
 * few hundred bytes, so the dollars committed to it are added up here: a bid is
 * worth price × size, and an offer is backed by (1 - price) × size, because
 * whoever offers YES at 20¢ has 80¢ of collateral behind every contract. Both
 * halves count, so the number means "dollars resting on this leg" rather than
 * "dollars on one side of it".
 *
 * An empty book gives zero, and that is a genuine zero rather than a silence:
 * the exchange was asked and answered that nothing rests. Turnover is the
 * opposite case — it is never asked and never answered, so it stays `null`.
 */
export function applyBook(market: Market, raw: RawGeminiBook | null): Market {
  if (!raw) return market;

  const { yes, yesAsks } = ladders(raw);
  const depth =
    yes.reduce((sum, level) => sum + level.price * level.size, 0) +
    yesAsks.reduce((sum, level) => sum + (1 - level.price) * level.size, 0);

  const yesBid = yes[0]?.price ?? market.yesBid;
  const yesAsk = yesAsks[0]?.price ?? market.yesAsk;

  return {
    ...market,
    yesBid,
    yesAsk,
    noBid: yesAsk === null ? null : round4(1 - yesAsk),
    noAsk: yesBid === null ? null : round4(1 - yesBid),
    mid:
      yesBid !== null && yesAsk !== null
        ? round4((yesBid + yesAsk) / 2)
        : (market.lastPrice ?? yesBid ?? yesAsk),
    liquidity: round4(depth),
  };
}

/**
 * Merge the instrument's own 24h summary.
 *
 * `/v2/ticker` restates the top of book and adds an `open` — the price 24h ago
 * — which is the only one the exchange states per contract. It is what gives a
 * move to the 1,247 legs whose catalogue row carries a 24h percentage but no
 * last print for it to apply to: a percentage of a price nobody states is not a
 * figure, and those legs would otherwise show `--` in the change column while
 * the exchange's own page shows a number.
 *
 * Its `changes` array of 24 hourly closes is **not** read. The documented order
 * is newest first and the payload's is not: on both instruments sampled
 * `changes[0]` equalled `open` and `changes[23]` equalled `close`. A series
 * read the wrong way round draws the day's move backwards, and `open` and
 * `close` state the same two prices without the ambiguity.
 */
export function applyTicker(market: Market, raw: RawGeminiTicker | null): Market {
  if (!raw) return market;

  const yesBid = quote(raw.bid) ?? market.yesBid;
  const yesAsk = quote(raw.ask) ?? market.yesAsk;
  const lastPrice = quote(raw.close) ?? market.lastPrice;
  // The catalogue's own percentage is the exchange's arithmetic, so it wins;
  // this only fills the gap where there was none.
  const previousPrice = market.previousPrice ?? quote(raw.open);

  return {
    ...market,
    yesBid,
    yesAsk,
    noBid: yesAsk === null ? null : round4(1 - yesAsk),
    noAsk: yesBid === null ? null : round4(1 - yesBid),
    mid:
      yesBid !== null && yesAsk !== null
        ? round4((yesBid + yesAsk) / 2)
        : (lastPrice ?? yesBid ?? yesAsk),
    lastPrice,
    previousPrice,
    change:
      lastPrice !== null && previousPrice !== null ? round4(lastPrice - previousPrice) : null,
  };
}

/* -------------------------------------------------------------------- book */

/**
 * Both ladders, parsed and sorted.
 *
 * The exchange already returns bids descending and asks ascending; sorting
 * anyway costs nothing on a book this size and means a change of habit upstream
 * cannot quietly invert a ladder that the whole panel reads top-down.
 */
function ladders(raw: RawGeminiBook): { yes: BookLevel[]; yesAsks: BookLevel[] } {
  const rows = (side: RawGeminiLevel[] | undefined): BookLevel[] =>
    (side ?? [])
      .map((level) => ({ price: num(level.price) ?? 0, size: num(level.amount) ?? 0 }))
      .filter((level) => level.price > 0 && level.size > 0);

  return {
    yes: rows(raw.bids).sort((a, b) => b.price - a.price),
    yesAsks: rows(raw.asks).sort((a, b) => a.price - b.price),
  };
}

/**
 * One book per leg, quoted in YES terms.
 *
 * There is no separate NO instrument to fetch, so the NO ladder is the ask side
 * inverted — a resting offer of YES at 20¢ is a bid for NO at 80¢, the same
 * order seen from the other end. A settled instrument answers with both sides
 * empty, which reaches the panel as a book with no top rather than as an error.
 */
export function normaliseOrderBook(raw: RawGeminiBook, ticker: string, depth = 12): OrderBook {
  const all = ladders(raw);
  const yes = all.yes.slice(0, depth);
  const yesAsks = all.yesAsks.slice(0, depth);
  const no = yesAsks
    .map((level) => ({ price: round4(1 - level.price), size: level.size }))
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
 * The whole ladder, capped here rather than upstream.
 *
 * `/v1/book` takes `limit_bids` and `limit_asks`, and they are deliberately not
 * sent: one cached snapshot then serves a quote panel asking for 5 levels and a
 * ladder asking for 50 without a second request, and {@link getMarket} needs
 * every level to add the resting depth up.
 */
export async function getOrderBook(symbol: string, depth = 12): Promise<OrderBook> {
  const raw = await instrument<RawGeminiBook>(
    symbol,
    `/v1/book/${encodeURIComponent(symbol)}`,
    TTL.quote,
  );
  return normaliseOrderBook(raw, symbol, depth);
}

/* -------------------------------------------------------------------- tape */

/** The exchange's own ceiling. `limit_trades=1000` is a 400, not a truncation. */
const MAX_TRADES = 500;

/**
 * The public print tape, newest first.
 *
 * Two things this tape is not, both of which matter more than what it is.
 *
 * It is **not a count of anything**. Twenty identical requests for one
 * instrument returned 278 rows thirteen times and 348-349 rows the rest, and the
 * two variants are not a prefix of each other — each replica held prints the
 * other lacked, and their summed sizes differed by 11%. So nothing here derives
 * a volume or a trade count, and nothing paginates on `tid`: a walk backwards
 * through the ids would skip prints depending on which replica answered.
 *
 * It is **not the instrument's lifetime**. It reaches back about 98 days; the
 * daily bars from `/v2/candles` on the same instrument start three months
 * earlier and carry 2,441 contracts that never appear on the tape at all. A
 * backfill has to come from the bars.
 *
 * `type` is `buy` or `sell` and the payload states nothing further, so it maps
 * to the aggressor's side as a reading of the field name rather than as a fact
 * the exchange asserted.
 */
export function normaliseTrades(
  rows: RawGeminiTrade[],
  ticker: string,
  limit: number,
): TradesResponse {
  const trades: Trade[] = [];

  for (const row of rows) {
    if (trades.length >= limit) break;

    const yesPrice = num(row.price);
    const count = num(row.amount);
    const ms = num(row.timestampms);
    const seconds = num(row.timestamp) ?? (ms === null ? null : ms / 1000);
    // A print with no price, no size or no clock is not a print the tape can
    // show. Defaulting any of the three would put a free trade, an empty one or
    // one stamped 1970 in a column the reader scans for outliers.
    if (yesPrice === null || count === null || seconds === null) continue;

    trades.push({
      venue: VENUE,
      // A 16-digit integer: an identity, never an amount, so it travels as text.
      tradeId: String(row.tid ?? ''),
      ticker,
      ts: Math.floor(seconds),
      count,
      yesPrice,
      noPrice: round4(1 - yesPrice),
      takerSide: row.type === 'sell' ? 'no' : 'yes',
      // No block or cross flag exists on this venue.
      isBlockTrade: false,
    });
  }

  // No cursor: see above — a `since_tid` walk over an inconsistent tape can
  // silently drop prints, so the panel gets one honest page instead.
  return { trades, cursor: null };
}

export async function getTrades(symbol: string, limit = 50): Promise<TradesResponse> {
  const want = Math.min(Math.max(limit, 1), MAX_TRADES);
  const rows = await instrument<RawGeminiTrade[]>(
    symbol,
    `/v1/trades/${encodeURIComponent(symbol)}${qs({ limit_trades: want })}`,
    TTL.quote,
  );
  return normaliseTrades(Array.isArray(rows) ? rows : [], symbol, want);
}

/* ----------------------------------------------------------------- candles */

/**
 * The exchange's interval names for the terminal's three periods.
 *
 * It serves `1m, 5m, 15m, 30m, 1hr, 6hr, 1day` and rejects everything else with
 * a 400, so the terminal's grid maps onto it exactly and nothing has to be
 * aggregated. `1h` and `1min` are both rejections, not synonyms.
 */
const INTERVALS: Record<CandleInterval, string> = { 1: '1m', 60: '1hr', 1440: '1day' };

/**
 * Bars from the matching engine, oldest first.
 *
 * Two conversions, both of which are silent bugs if skipped. The feed returns
 * newest first, and a chart drawn in that order runs backwards. And its
 * timestamp is the period *start* in milliseconds, where a {@link Candle} is
 * stamped with its period *end* in seconds — half a day's error on a daily bar.
 *
 * A zero-volume bar is the exchange stating that nothing printed in the period
 * and carrying the previous close forward, so it is marked untraded and keeps
 * its zero: unlike turnover, this is a figure the venue does answer.
 */
export function normaliseCandles(rows: RawGeminiKline[], interval: CandleInterval): Candle[] {
  const period = interval * 60;
  const candles: Candle[] = [];

  for (const row of rows) {
    if (!Array.isArray(row) || row.length < 6) continue;
    const startMs = num(row[0]);
    const open = num(row[1]);
    const high = num(row[2]);
    const low = num(row[3]);
    const close = num(row[4]);
    if (startMs === null || open === null || high === null || low === null || close === null) {
      continue;
    }

    const volume = num(row[5]);
    candles.push({
      time: Math.floor(startMs / 1000) + period,
      open,
      high,
      low,
      close,
      volume,
      openInterest: null,
      traded: (volume ?? 0) > 0,
      bid: null,
      ask: null,
    });
  }

  return candles.sort((a, b) => a.time - b.time);
}

/**
 * Price history over a window.
 *
 * `/v2/klines` rather than `/v2/candles`, which serves the same rows in the
 * same shape but only ever the maximum window — the range is the whole point of
 * this call. No `note` on the response: these are true OHLC bars off the
 * matching engine, not a price sample series bucketed into bars, so there is
 * nothing to disclaim.
 */
export async function getCandles(
  symbol: string,
  interval: CandleInterval,
  startTs: number,
  endTs: number,
): Promise<CandlesResponse> {
  const rows = await instrument<RawGeminiKline[]>(
    symbol,
    `/v2/klines${qs({
      symbol,
      interval: INTERVALS[interval],
      startTime: Math.floor(startTs) * 1000,
      endTime: Math.floor(endTs) * 1000,
    })}`,
    TTL.candles,
  );

  const known = (await corpusSnapshot()).markets.find((m) => m.ticker === symbol);

  return {
    venue: VENUE,
    ticker: symbol,
    seriesTicker: known?.seriesTicker ?? '',
    interval,
    candles: normaliseCandles(Array.isArray(rows) ? rows : [], interval),
  };
}

/* ----------------------------------------------------------------- corpus */

const CORPUS_KEY = 'gemini:corpus';

/**
 * The whole open universe in one request.
 *
 * `limit` has no cap — 500, 1,000 and 2,000 all returned every row with the
 * limit echoed back — so there is nothing to paginate and no truncation to
 * apportion between categories, which is the problem every other venue's crawl
 * spends its complexity on. The decoded body is ~10 MB against `fetchJson`'s
 * 16 MiB ceiling, which fits today and would not fit twice over, so the ceiling
 * is raised to leave the universe room to grow.
 *
 * Two corners of the catalogue are deliberately outside it. `status=settled` is
 * hard-capped at 1,000 rows however it is paged, and `status=under_review`
 * holds ~20 events that no other status returns. Neither is open interest to a
 * trader reading a live board, and both would cost a second 10 MB request.
 */
const CORPUS_LIMIT = 500;
const CORPUS_MAX_BYTES = 32 * 1024 * 1024;

/** What the event states that a {@link Market} has nowhere to carry. */
interface EventFacts {
  volume24h: number | null;
  tags: string[];
}

interface GeminiSnapshot {
  corpus: Corpus;
  /** Keyed by event ticker. */
  facts: Map<string, EventFacts>;
}

async function buildSnapshot(): Promise<GeminiSnapshot> {
  const raw = await fetchJson<RawGeminiCatalogue>(
    `${CATALOGUE}/prediction-markets${qs({ limit: CORPUS_LIMIT, status: 'active' })}`,
    { timeoutMs: 45_000, retries: 2, maxBytes: CORPUS_MAX_BYTES },
  );

  const rows = raw.data ?? [];
  const events: VenueEvent[] = [];
  const markets: Market[] = [];
  const facts = new Map<string, EventFacts>();

  for (const row of rows) {
    if (!row.ticker) continue;
    const event = normaliseEvent(row);
    events.push(event);
    markets.push(...event.markets);
    facts.set(event.eventTicker, {
      volume24h: num(row.volume24h),
      tags: [...(row.tags ?? []), ...(row.subcategory?.slug ? [row.subcategory.slug] : [])],
    });
  }

  const total = raw.pagination?.total ?? rows.length;

  return {
    corpus: {
      venue: VENUE,
      events,
      markets,
      builtAt: Date.now(),
      truncated: rows.length < total,
    },
    facts,
  };
}

async function snapshot(): Promise<GeminiSnapshot> {
  return cache.cached(CORPUS_KEY, TTL.catalogue, buildSnapshot);
}

export async function corpusSnapshot(): Promise<Corpus> {
  return (await snapshot()).corpus;
}

export function warmCorpus(): void {
  corpusSnapshot()
    .then((c) => {
      console.log(`[gemini] corpus warm: ${c.events.length} events, ${c.markets.length} markets`);
    })
    .catch((err: unknown) => {
      console.warn('[gemini] corpus warm failed:', err instanceof Error ? err.message : err);
    });
}

/**
 * The series in the open snapshot, derived rather than listed.
 *
 * There is no series endpoint and no series list to read, so the families come
 * from the events themselves — {@link seriesFromEvent} on each, grouped. The
 * category filter runs over the snapshot rather than over the catalogue's own
 * repeatable `?category=` param, so what this lists and what `SRCH` and `TOP`
 * see are the same universe read at the same moment; a second request would
 * make them disagree for no gain.
 *
 * `frequency` stays empty. The exchange states none, and a series named `BTC1H`
 * is not a licence to write "hourly" into a field the reader will take as the
 * venue's own word.
 */
export async function listSeries(category?: string): Promise<SeriesInfo[]> {
  const { corpus, facts } = await snapshot();
  const want = category?.trim().toLowerCase();

  const families = new Map<string, { title: string; category: string; tags: Set<string> }>();

  for (const event of corpus.events) {
    if (want && event.category.toLowerCase() !== want) continue;
    if (!event.seriesTicker) continue;

    const tags = facts.get(event.eventTicker)?.tags ?? [];
    const family = families.get(event.seriesTicker);
    if (family) {
      for (const tag of tags) family.tags.add(tag);
      continue;
    }
    families.set(event.seriesTicker, {
      // The first event's title stands for the family: they are the same
      // question at different expiries, so any of them names it.
      title: event.title,
      category: event.category,
      tags: new Set(tags),
    });
  }

  return [...families]
    .map(([ticker, family]) => ({
      venue: VENUE,
      ticker,
      title: family.title,
      category: family.category,
      frequency: '',
      tags: [...family.tags],
    }))
    .sort((a, b) => a.ticker.localeCompare(b.ticker));
}

/**
 * Search the local snapshot rather than the catalogue's own `?search=`.
 *
 * That parameter is honoured and semantic, which is worse than absent: it
 * answers `fed` with the September FOMC market and then five baseball games,
 * and answers `zzzznotathing` with two events rather than none. Ranking the
 * snapshot the way every other venue is ranked gives results a trader can
 * compare across brokers.
 */
export async function search(query: string, limit = 25): Promise<SearchResponse> {
  return searchCorpus(await corpusSnapshot(), query, limit);
}

/**
 * Movers, ranked here rather than by `rankMarkets`.
 *
 * The shared ranker drops a market whose sorted figure is `null` and gates the
 * movers on `volume24h > 0`, which between them exclude every Gemini contract
 * but the 17 that are the whole of their event — turnover is stated per event,
 * so a leg never carries one. Ranking the same board against the *event's*
 * turnover keeps what that gate is for, that a move with nothing traded behind
 * it is a stale print rather than a move, without claiming each of a 12-leg
 * field's legs did the field's volume.
 *
 * The three boards the venue registry does not declare return empty rather than
 * ranking on a substitute: there is no open interest anywhere in this API, no
 * resting-depth figure outside the per-contract book, and no per-contract
 * turnover at all. `TOP` reads that declaration and does not offer them.
 */
export async function topMarkets(sort: MoverSort, limit = 25): Promise<Market[]> {
  if (sort !== 'gainers' && sort !== 'losers') return [];

  const { corpus, facts } = await snapshot();
  // Biggest rise first on `gainers`, biggest fall first on `losers`. `TOP` puts
  // several venues on one board and orders the merge itself, so what matters
  // here is that the `limit` rows handed over are the venue's real movers and
  // not the quiet end of its book.
  const direction = sort === 'losers' ? 1 : -1;

  return corpus.markets
    .filter((m) => m.change !== null && (facts.get(m.eventTicker)?.volume24h ?? 0) > 0)
    .sort((a, b) => direction * ((a.change ?? 0) - (b.change ?? 0)))
    .slice(0, limit);
}
