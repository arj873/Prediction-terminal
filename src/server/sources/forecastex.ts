/**
 * ForecastEx (forecastex.com) client — the CFTC-regulated exchange behind
 * Interactive Brokers' ForecastTrader.
 *
 * Two public surfaces, both unauthenticated, and the terminal reads both:
 *
 *   forecastex.com/api/*        catalogue, products and price samples. Real
 *                               JSON, but undocumented and Lambda-backed, and
 *                               every response is double-wrapped — `body.data`
 *                               is a JSON *string* that has to be parsed again.
 *   forecastex-public-data.s3   two years of daily CSVs: the whole-exchange
 *                               print tape and the end-of-session archive.
 *
 * What it never publishes is a book. ForecastEx matches by *pairing* — a print
 * creates one YES holder and one NO holder at the same moment — so there is no
 * bid, no ask and no ladder anywhere in public, and {@link getOrderBook} says
 * that rather than dressing the last print up as a one-level ladder. For the
 * same reason `last_yes_price` and `last_no_price` are two independent prints
 * (they sum to 1.00 on 3,977 contracts and 1.01 on another 832), so neither is
 * ever derived from the other.
 *
 * Turnover and the daily move are not in the live catalogue at all: they come
 * from the end-of-session archive, which is published after 16:30 CT. Every
 * figure this module takes from there is therefore a session behind the tape,
 * and each place it lands says so.
 *
 * The identifier trap is the expensive one. `/api/contracts` and
 * `/api/products` are case-insensitive, but `/api/prices` is case-sensitive and
 * fails *silently* — an upper-cased id answers HTTP 200 with an empty series,
 * which draws a blank chart instead of raising anything. The registry folds ids
 * to upper before this module is called, so the canonical spelling is restored
 * from the warm catalogue, or from one case-insensitive contract lookup, before
 * any price call goes out.
 */

import type {
  Candle,
  CandleInterval,
  CandlesResponse,
  Market,
  OrderBook,
  SeriesInfo,
  StrikeType,
  Trade,
  TradesResponse,
  VenueEvent,
} from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson, fetchText } from '../lib/http.js';
import {
  rankMarkets,
  searchCorpus,
  type Corpus,
  type MoverSort,
  type SearchResponse,
} from './corpus.js';

const API_BASE = process.env.FORECASTEX_API_BASE ?? 'https://forecastex.com';

/**
 * The archive is read straight from the bucket rather than through
 * `/api/download`, which is only a proxy in front of it. S3 states
 * `Last-Modified` and honours `Range`; the proxy does neither, so the same
 * bytes cost more to keep fresh.
 */
const ARCHIVE_BASE =
  process.env.FORECASTEX_ARCHIVE_BASE ?? 'https://forecastex-public-data.s3.amazonaws.com';

const VENUE = 'forecastex' as const;

/* --------------------------------------------------------------- coercion */

/**
 * Read a figure that arrives as a JSON number, a CSV string, or not at all.
 *
 * Everything that is not a stated figure has to come back `null`, and `Number`
 * is unhelpfully generous about what it calls zero: `Number('')`, `Number(' ')`
 * and `Number(false)` are all `0`. A blank `settlement_price` cell — 52.6% of
 * the archive — would therefore settle every one of those contracts at zero,
 * which is the difference between "the exchange says nothing" and "the exchange
 * says worthless". Only a number or a non-blank numeric string counts.
 */
export function num(value: unknown): number | null {
  if (typeof value === 'number') return Number.isFinite(value) ? value : null;
  if (typeof value !== 'string') return null;
  const text = value.trim();
  if (text === '') return null;
  const n = Number(text);
  return Number.isFinite(n) ? n : null;
}

function round4(n: number): number {
  return Math.round(n * 10_000) / 10_000;
}

/* ------------------------------------------------------------- US Central */

/**
 * ForecastEx keeps its clock in Chicago and mostly forgets to say so.
 *
 * `expiration_date`, `last_trade_date` and the archive's daily `date` all
 * arrive as bare wall-clock strings with no zone, while the CSVs give the very
 * same instants an explicit `-05:00`/`-06:00`. Left bare they would be read as
 * whatever zone the reader's browser happens to be in — a five-hour error on
 * every close time in London — so the offset is computed and stamped on here.
 */
const CENTRAL = 'America/Chicago';

const CENTRAL_PARTS = new Intl.DateTimeFormat('en-US', {
  timeZone: CENTRAL,
  hourCycle: 'h23',
  year: 'numeric',
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
});

interface WallClock {
  year: number;
  month: number;
  day: number;
  hour: number;
  minute: number;
  second: number;
}

function centralClock(atMs: number): WallClock {
  const read: Record<string, number> = {};
  for (const part of CENTRAL_PARTS.formatToParts(new Date(atMs))) {
    if (part.type !== 'literal') read[part.type] = Number(part.value);
  }
  return {
    year: read.year ?? 1970,
    month: read.month ?? 1,
    day: read.day ?? 1,
    hour: read.hour ?? 0,
    minute: read.minute ?? 0,
    second: read.second ?? 0,
  };
}

/** Milliseconds Central is ahead of UTC at an instant — negative all year. */
function centralOffsetMs(atMs: number): number {
  const c = centralClock(atMs);
  return (
    Date.UTC(c.year, c.month - 1, c.day, c.hour, c.minute, c.second) -
    Math.floor(atMs / 1000) * 1000
  );
}

const WALL_CLOCK = /^(\d{4})-(\d{2})-(\d{2})(?:[T ](\d{2}):(\d{2})(?::(\d{2}))?)?/;

/**
 * A Central wall-clock string as a unix instant.
 *
 * Two passes, because the offset depends on the instant and the instant depends
 * on the offset: the first pass picks the right side of a DST boundary, the
 * second reads the offset that actually applies there.
 */
export function centralInstant(local: string): number | null {
  const m = WALL_CLOCK.exec(local.trim());
  if (!m) return null;
  const naive = Date.UTC(
    Number(m[1]),
    Number(m[2]) - 1,
    Number(m[3]),
    Number(m[4] ?? '0'),
    Number(m[5] ?? '0'),
    Number(m[6] ?? '0'),
  );
  const first = naive - centralOffsetMs(naive);
  return Math.floor((naive - centralOffsetMs(first)) / 1000);
}

/** The same wall clock with its offset spelled out, so the instant is unambiguous. */
export function centralIso(local: string): string {
  const seconds = centralInstant(local);
  if (seconds === null) return local;
  const offset = centralOffsetMs(seconds * 1000) / 60_000;
  const sign = offset <= 0 ? '-' : '+';
  const size = Math.abs(offset);
  const hh = String(Math.floor(size / 60)).padStart(2, '0');
  const mm = String(size % 60).padStart(2, '0');
  return `${local}${sign}${hh}:${mm}`;
}

/**
 * The session day an instant falls in, as the archive names its files.
 *
 * The exchange's day runs 16:15 CT to 16:14 CT, so a session is filed under the
 * date it *ends* on: prints made on Monday evening belong to Tuesday's tape.
 * Asking for the calendar date instead fetches an empty file every evening.
 */
export function sessionDate(atMs: number = Date.now()): string {
  const c = centralClock(atMs);
  const rolled = c.hour > 16 || (c.hour === 16 && c.minute >= 15) ? 1 : 0;
  const day = new Date(Date.UTC(c.year, c.month - 1, c.day + rolled));
  return day.toISOString().slice(0, 10).replace(/-/g, '');
}

/** The session before `date` (`YYYYMMDD`), for walking back to a published file. */
function previousSession(date: string): string {
  const year = Number(date.slice(0, 4));
  const month = Number(date.slice(4, 6));
  const day = Number(date.slice(6, 8));
  const back = new Date(Date.UTC(year, month - 1, day - 1));
  return back.toISOString().slice(0, 10).replace(/-/g, '');
}

/* ------------------------------------------------------------ raw upstream */

/**
 * The Lambda-proxy envelope every `/api/*` response arrives in.
 *
 * Two oddities worth knowing before reading {@link unwrap}. `body.data` is a
 * JSON *string*, not an array — the payload is serialised twice. And a rejected
 * request comes back as HTTP 200 with `statusCode: 400` and `body` collapsed to
 * a string holding `{"error": …}`, so the HTTP status is not the one that
 * matters.
 */
export interface RawFexEnvelope {
  statusCode?: number;
  body?: RawFexBody | string;
  /** Present instead of the envelope when the Lambda itself failed. */
  errorMessage?: string;
  errorType?: string;
}

export interface RawFexBody {
  /** A JSON string on every live response. Typed loosely for the day it isn't. */
  data?: string | unknown[];
  next_page?: number | null;
  total_pages?: number;
  page_size?: number;
  /** Full available depth of a price series, regardless of the window asked for. */
  total_days?: number;
}

/** A catalogue row. These twelve keys are the whole record — there are no others. */
export interface RawFexContract {
  contract_id?: string;
  product_id?: string;
  category?: string;
  event_display_name?: string;
  question?: string;
  expiration_date?: string;
  last_trade_date?: string;
  payout_date?: string;
  open_interest?: number | null;
  exchange_spec_url?: string;
  /** Independent last prints. `null` means never traded — 58% of the universe. */
  last_yes_price?: number | null;
  last_no_price?: number | null;
}

/** A product row — what every other venue calls a series. */
export interface RawFexProduct {
  product_id?: string;
  name?: string;
  description?: string;
  source_agency?: string;
  source_agency_url?: string;
  category?: string;
  calculation_method?: string;
  question_template?: string;
  /** `I` `M` `D` `Q` `W` `H`. */
  frequency_type?: string;
  /** The IBKR/Reuters symbol, e.g. `HORC=MAN`. */
  external_symbol?: string;
  /** `String` `Decimal` `Percentage` `Integer` `Date` — how the strike segment reads. */
  strike_unit?: string;
  position_accountability_value?: string;
  open_interest?: number | null;
}

/** One point of `/api/prices`. A close and a size, never an OHLC bar. */
export interface RawFexPricePoint {
  instrument_id?: string;
  yes_price?: number | null;
  no_price?: number | null;
  /** ISO with `Z` on the hourly feed; a bare Central session date on the daily one. */
  interval_date?: string;
  volume?: number | null;
}

/** One print from `pairs/pairs_YYYYMMDD.csv`, fields as the CSV spells them. */
export interface RawFexPair {
  pair_id: string;
  event_contract: string;
  expiration_date: string;
  quantity: string;
  yes_price: string;
  no_price: string;
  /** US-Central offset with *nanosecond* fractional seconds. */
  pair_time: string;
}

/**
 * One row of `prices/daily_prices_YYYYMMDD.csv`, the end-of-session archive.
 *
 * `high_price`, `low_price` and `vwap` are carried here and deliberately never
 * read: on an untraded row the file writes all three as `0.00`, which is not a
 * mark, and ~92% of rows are untraded. `settlement_price` is likewise unread —
 * it is an empty string on 52.6% of rows, so parsing it unconditionally throws
 * away half the file, and every contract the terminal can reach is unsettled
 * anyway (see {@link normaliseMarket} on `result`).
 */
export interface RawFexArchiveRow {
  event_contract: string;
  subtype: string;
  expiration_date: string;
  date: string;
  start_price: string;
  high_price: string;
  low_price: string;
  end_price: string;
  settlement_price: string;
  pair_quantity: string;
  open_interest: string;
  vwap: string;
}

/**
 * Unwrap the double-wrapped envelope.
 *
 * The inner `JSON.parse` is the whole point: the exchange serialises its
 * payload, then serialises the wrapper around it. That is unusual enough that
 * it could be "fixed" in a deploy without notice, so an array arriving where a
 * string is expected is accepted rather than treated as corruption — the shape
 * that breaks is the one nobody would ship on purpose.
 */
export function unwrap<T>(envelope: RawFexEnvelope): { rows: T[]; nextPage: number | null } {
  if (envelope.errorMessage) {
    throw new UpstreamError(`ForecastEx failed the request: ${envelope.errorMessage}`, {
      code: 'upstream_error',
      hint:
        'The Lambda behind /api caps its response at 6 MB. Ask for a smaller pageSize — ' +
        '5000 is the largest that reliably answers.',
    });
  }

  const status = envelope.statusCode ?? 200;
  const body = envelope.body;

  // A rejection collapses `body` to a string holding `{"error": …}` while the
  // HTTP status stays 200, so the envelope's own status is the one to read.
  if (typeof body === 'string' || status >= 400) {
    throw new UpstreamError(`ForecastEx rejected the request: ${statedError(body)}`, {
      code: status === 404 ? 'not_found' : 'upstream_status',
      status,
    });
  }

  const data = body?.data;
  let rows: unknown = data;
  if (typeof data === 'string') {
    try {
      rows = JSON.parse(data);
    } catch {
      throw new UpstreamError('ForecastEx returned a data string that is not JSON', {
        code: 'bad_upstream_body',
      });
    }
  }
  if (!Array.isArray(rows)) {
    throw new UpstreamError('ForecastEx returned an envelope with no data array', {
      code: 'bad_upstream_body',
    });
  }

  return { rows: rows as T[], nextPage: body?.next_page ?? null };
}

function statedError(body: RawFexBody | string | undefined): string {
  if (typeof body !== 'string') return 'no reason given';
  try {
    const parsed: unknown = JSON.parse(body);
    if (parsed && typeof parsed === 'object' && 'error' in parsed) {
      return String((parsed as { error: unknown }).error);
    }
  } catch {
    /* the body was not JSON — report it as it came */
  }
  return body.slice(0, 200);
}

/* ------------------------------------------------------------ identifiers */

/**
 * The event a contract belongs to: the first two segments of its id.
 *
 * Verified against a full crawl — grouping on this prefix and grouping on
 * `(product_id, event_display_name)` both give 1,579 groups, with no prefix
 * spanning two display names. The exchange publishes no event endpoint and no
 * event id, so this prefix *is* the event.
 */
export function eventTickerOf(contractId: string): string {
  return contractId.toUpperCase().split('_').slice(0, 2).join('_');
}

/**
 * The strike: the *last* segment, not the third.
 *
 * Nine Conditional contracts carry four segments —
 * `ZFFCP_091626_E0X0926_3.4` is the Fed-scenario code and then the CPI
 * threshold — and reading index 2 there yields the literal string `E0X0926`,
 * which renders as a strike label and parses as `NaN`. The last segment is
 * identical to index 2 on every three-segment id, so this is right in both
 * shapes.
 */
export function strikeOf(contractId: string): string {
  const parts = contractId.split('_');
  return parts.length >= 3 ? (parts[parts.length - 1] ?? '') : '';
}

/* ------------------------------------------------------------ normalisers */
/**
 * A readable name for a leg whose strike is a code rather than a word.
 *
 * Half this exchange's ladders are numeric and label themselves — `Above 80` —
 * but the other half carry codes: the September FOMC book's legs are `E0`,
 * `E25`, `E-25`, `H50` and `M-50`, and a Senate race's are `AH` and `JT`.
 * Printed as they arrive, an event's rungs read as five unrelated tokens, and
 * no cross-venue matcher can pair `E0` with `No change` at the other five
 * brokers — the FOMC ladder, the most compared question here, lined up at every
 * venue except this one.
 *
 * The meaning is in the question, which the exchange writes out in full and
 * varies only where the legs differ: *"Will the Fed **leave the rate
 * unchanged** in September 2026?"* against *"Will the Fed **raise the rate
 * 25bps** in September 2026?"*. Removing the words every leg shares, from both
 * ends, leaves exactly the clause that distinguishes it.
 *
 * Returns `null` when that leaves any leg with nothing — a single-leg event has
 * no sibling to differ from, and two legs the exchange worded identically (it
 * lists a few races twice, once by party and once by candidate) reduce to a
 * pair of blanks. In both cases the code is a worse label than the question but
 * a better one than nothing.
 */
export function distinguishingClauses(questions: readonly string[]): string[] | null {
  if (questions.length < 2) return null;
  const words = questions.map((q) => q.trim().split(/\s+/).filter(Boolean));
  if (words.some((w) => w.length === 0)) return null;

  const shortest = Math.min(...words.map((w) => w.length));
  const same = (i: (w: string[]) => number): boolean => {
    const first = words[0]![i(words[0]!)]!.toLowerCase();
    return words.every((w) => w[i(w)]?.toLowerCase() === first);
  };

  let head = 0;
  while (head < shortest && same(() => head)) head++;

  let tail = 0;
  while (head + tail < shortest && same((w) => w.length - 1 - tail)) tail++;

  const clauses = words.map((w) =>
    w
      .slice(head, w.length - tail)
      .join(' ')
      .replace(/[\s?.,;:]+$/, ''),
  );
  return clauses.every(Boolean) ? clauses : null;
}


/** The strike as a reader would see it written, e.g. `3.625%`, `91`, `Republican`. */
function strikeLabel(strike: string, strikeUnit: string | undefined): string {
  if (!strike) return '';
  return strikeUnit === 'Percentage' && num(strike) !== null ? `${strike}%` : strike;
}

/**
 * Which way a ladder runs, taken from the question the exchange itself wrote.
 *
 * The identifier cannot say. `UHBKF_081826_91` and `ULATL_081826_73` are the
 * same grammar with opposite meanings — one settles YES *above* its strike, the
 * other *below* it — so a rule that reads only the id has to guess, and the
 * brief's guess was that every numeric ladder is a one-sided "exceed". It is
 * not: over a full crawl the wording partitions the numeric universe into 9,185
 * upward contracts, **370 downward ones** (`ULATL_081826_73`, "will the lowest
 * temperature in Atlanta be below 73 F"), and none that say both. Calling those
 * 370 `greater` would draw their ladders inverted and put the YES region on the
 * wrong side of the strike.
 *
 * The 53 that state no direction at all — "will exactly one FOMC member
 * dissent", "will Canada's economy enter a recession" — are not ladders, and
 * every strike field stays unstated for them rather than reporting a bound the
 * contract does not have.
 */
function strikeDirection(question: string): StrikeType | null {
  if (/\bat least\b/i.test(question)) return 'greater_or_equal';
  if (/\b(?:exceeds?|above|greater than|more than|higher than)\b/i.test(question)) return 'greater';
  if (/\bat most\b/i.test(question)) return 'less_or_equal';
  if (/\b(?:below|under|less than|lower than|fewer than)\b/i.test(question)) return 'less';
  return null;
}

/** YES and NO leg labels for a ladder rung, in the direction it actually runs. */
const LEG_LABELS: Record<StrikeType, (label: string) => [string, string]> = {
  greater: (l) => [`Above ${l}`, `${l} or below`],
  greater_or_equal: (l) => [`${l} or above`, `Below ${l}`],
  less: (l) => [`Below ${l}`, `${l} or above`],
  less_or_equal: (l) => [`${l} or below`, `Above ${l}`],
  // No `between` ladder exists here — every numeric event found is one-sided —
  // so this arm is unreachable and labels the bound it was given.
  between: (l) => [l, 'No'],
};

/**
 * One contract, quoted from the only two figures the exchange states: the last
 * YES print and the open interest behind it.
 *
 * The four book fields are `null` and always will be — there is no book, and a
 * synthesised level would put a ladder on screen that nobody can trade against.
 * `mid` carries the last print instead, which is honest only because the panel
 * labels it as a print; it is not a midpoint of anything.
 *
 * The strike fields are filled in only where the last segment parses as a
 * number *and* {@link strikeDirection} finds the direction stated in the
 * question. A strike level with no direction behind it is worse than none: it
 * decides which side of the line the YES region sits on, and this venue lists
 * ladders running both ways.
 */
export function normaliseMarket(raw: RawFexContract, product?: RawFexProduct): Market {
  const contractId = raw.contract_id ?? '';
  // The registry folds ids to upper, so a ticker that round-trips through
  // `getMarket` has to be in that case too. The canonical spelling survives in
  // the strike label below, and is restored from the catalogue before any
  // case-sensitive price call.
  const ticker = contractId.toUpperCase();

  const strike = strikeOf(contractId);
  const label = strikeLabel(strike, product?.strike_unit);
  const level = strike === '' ? null : num(strike);
  const direction = level === null ? null : strikeDirection(raw.question ?? '');
  const upward = direction === 'greater' || direction === 'greater_or_equal';
  const [yesLeg, noLeg] = direction ? LEG_LABELS[direction](label) : [label || 'Yes', 'No'];

  const last = num(raw.last_yes_price);
  const expiry = raw.expiration_date ?? '';
  const expires = centralInstant(expiry);

  return {
    venue: VENUE,
    ticker,
    eventTicker: eventTickerOf(contractId),
    seriesTicker: raw.product_id ?? '',
    title: raw.question ?? raw.event_display_name ?? ticker,
    yesSubTitle: yesLeg,
    noSubTitle: noLeg,
    // Not published. The catalogue holds open contracts only — a settled one
    // leaves it entirely — so this reads `open` on everything reachable, and
    // the expiry check is here for the minutes either side of a close.
    status: expires !== null && expires <= Date.now() / 1000 ? 'closed' : 'open',
    marketType: 'binary',
    yesBid: null,
    yesAsk: null,
    noBid: null,
    noAsk: null,
    mid: last,
    lastPrice: last,
    // Both come from the end-of-session archive, a session behind the tape.
    // {@link applySession} fills them in; a market read without it says nothing
    // rather than saying zero.
    previousPrice: null,
    change: null,
    // Lifetime turnover is published nowhere. The CSV archive starts
    // 2024-08-01, so summing it would state "since August 2024" under a column
    // header that says "ever".
    volume: null,
    volume24h: null,
    // The one ranking figure the live catalogue does state. A `0` here is the
    // exchange saying zero — 6,909 contracts have no open position at all.
    openInterest: num(raw.open_interest),
    // Resting depth needs a book, and there is none.
    liquidity: null,
    // No listing timestamp exists anywhere in the API.
    openTime: '',
    closeTime: raw.last_trade_date ? centralIso(raw.last_trade_date) : '',
    expirationTime: expiry ? centralIso(expiry) : '',
    // Settled contracts drop out of the catalogue on expiry, so nothing the
    // terminal can fetch has a result yet. The two-year CSV archive does carry
    // settlements, but only for contracts that can no longer be looked up.
    result: '',
    // A link to the contract's terms PDF; the catalogue carries no rules text.
    rulesPrimary: raw.exchange_spec_url ?? '',
    ...(raw.category ? { category: raw.category } : {}),
    strikeType: direction,
    floorStrike: upward ? level : null,
    // Only one bound is ever stated: every numeric event here is a one-sided
    // ladder, so a rung bounds the YES region below or above it, never both.
    capStrike: direction === 'less' || direction === 'less_or_equal' ? level : null,
  };
}

/**
 * Merge the end-of-session archive into a live market.
 *
 * This is where turnover and the daily move come from, and both are **a session
 * behind**: the file for the current session is not published until after
 * 16:30 CT, so intraday the previous close is what a reader sees. That lag is
 * the price of having the figures at all — the live catalogue states neither.
 *
 * `pair_quantity` is contracts and a genuine `0` — the exchange is saying
 * nothing traded that session, which is true of ~92% of the universe. The close
 * is not so simple: on 15,446 rows the session never traded *and* `start_price`
 * and `end_price` are both `0.00`, which is the file having no mark for that
 * contract rather than a contract worth nothing. Those keep a null previous
 * price, so `change` stays unstated instead of manufacturing a full-dollar move.
 */
export function applySession(market: Market, row: RawFexArchiveRow): Market {
  const traded = num(row.pair_quantity);
  const close = num(row.end_price);
  const marked = close !== null && (close > 0 || (traded ?? 0) > 0);
  const previous = marked ? close : null;

  return {
    ...market,
    previousPrice: previous,
    change:
      market.lastPrice !== null && previous !== null ? round4(market.lastPrice - previous) : null,
    volume24h: traded,
    // Open interest deliberately stays as the catalogue stated it: the archive's
    // column is the figure at that session's close, and the live one is now.
  };
}

/**
 * The legs of one event, and whether they exclude each other.
 *
 * Exclusivity is claimed only where the venue's own structure states it: a
 * `String` strike unit means the legs are named outcomes of a single field
 * (`{Republican, Democratic}` sums to 1.00), where a numeric unit means a
 * monotone "exceed N" ladder whose legs are all true at once below the mark.
 *
 * The Conditional guard is load-bearing and not a nicety. `ZFFCP_091626` has a
 * `String` strike unit and nine legs, and is *not* one winner-take-all field:
 * it is three Fed scenarios crossed with three cumulative CPI thresholds, so
 * within a scenario it is exactly the monotone ladder that must not be flagged.
 * Marking it exclusive would put a fictional Σmid arbitrage on screen across
 * the whole Conditional category.
 */
export function normaliseEvent(
  rows: RawFexContract[],
  product?: RawFexProduct,
  sessions?: ReadonlyMap<string, RawFexArchiveRow>,
): VenueEvent {
  const first = rows[0] ?? {};
  const eventTicker = eventTickerOf(first.contract_id ?? '');
  const category = first.category ?? '';

  const markets = rows.map((row) => {
    const market = normaliseMarket(row, product);
    const session = sessions?.get((row.contract_id ?? '').toUpperCase());
    return session ? applySession(market, session) : market;
  });

  // A numeric ladder already names its own rungs; a coded one does not, and
  // only the sibling questions can say what its codes mean.
  const coded = rows.every((row) => num(strikeOf(row.contract_id ?? '')) === null);
  const clauses = coded ? distinguishingClauses(rows.map((row) => row.question ?? '')) : null;
  if (clauses) {
    clauses.forEach((clause, i) => {
      markets[i] = { ...markets[i]!, yesSubTitle: clause };
    });
  }

  return {
    venue: VENUE,
    eventTicker,
    seriesTicker: first.product_id ?? '',
    title: first.event_display_name ?? eventTicker,
    subTitle: product?.name ?? '',
    category,
    mutuallyExclusive:
      product?.strike_unit === 'String' && rows.length > 1 && category !== 'Conditional',
    markets,
  };
}

/** How often the exchange lists a product. `I` is a one-off, not a cadence. */
const FREQUENCIES: Record<string, string> = {
  H: 'hourly',
  D: 'daily',
  W: 'weekly',
  M: 'monthly',
  Q: 'quarterly',
  I: 'irregular',
};

export function normaliseSeries(raw: RawFexProduct): SeriesInfo {
  const ticker = raw.product_id ?? '';
  const frequency = raw.frequency_type ?? '';
  return {
    venue: VENUE,
    ticker,
    title: raw.name ?? ticker,
    category: raw.category ?? '',
    frequency: FREQUENCIES[frequency] ?? frequency,
    // The strike unit is the useful one to carry: it is what says whether a
    // contract id's last segment is a number or the name of an outcome.
    tags: [raw.strike_unit, raw.external_symbol, raw.source_agency].filter(
      (tag): tag is string => Boolean(tag),
    ),
  };
}

/* ------------------------------------------------------------------- CSVs */

/**
 * Split one CSV line, honouring the quoting the archive actually uses.
 *
 * Eight live contracts have a comma inside their identifier — the conditional
 * books, `FEDRO_1128_Senate-D,House-D,President-D` and its siblings — and the
 * exchange quotes those cells, so sixteen lines of every daily archive carry
 * fourteen commas against a twelve-column header. Splitting on every comma
 * shifts their fields two to the left, which puts `House-D` in the `subtype`
 * column, fails the `YES` filter, and drops the contract from the archive
 * entirely: its published turnover and previous close then reach the terminal
 * as `null`, which is the terminal's way of saying the exchange never stated
 * them. It did.
 */
function splitRow(line: string): string[] {
  const cells: string[] = [];
  let cell = '';
  let quoted = false;

  for (let i = 0; i < line.length; i++) {
    const char = line[i];
    if (quoted) {
      // A doubled quote inside a quoted cell is one literal quote.
      if (char === '"' && line[i + 1] === '"') {
        cell += '"';
        i++;
      } else if (char === '"') quoted = false;
      else cell += char;
    } else if (char === '"') quoted = true;
    else if (char === ',') {
      cells.push(cell);
      cell = '';
    } else cell += char;
  }

  cells.push(cell);
  return cells;
}

/**
 * Split a CSV into records keyed by its own header.
 *
 * Reading by header rather than by position: the archive gained a `vwap` column
 * during its two years on the bucket, and a positional parser would have
 * silently shifted every field after it.
 */
function parseCsv(text: string): Record<string, string>[] {
  const lines = text.split(/\r?\n/);
  const header = splitRow(lines[0] ?? '').map((name) => name.trim());
  const rows: Record<string, string>[] = [];

  for (let i = 1; i < lines.length; i++) {
    const line = lines[i];
    if (!line) continue;
    const cells = splitRow(line);
    if (cells.length < header.length) continue;
    const row: Record<string, string> = {};
    header.forEach((name, col) => {
      row[name] = (cells[col] ?? '').trim();
    });
    rows.push(row);
  }

  return rows;
}

function asPair(row: Record<string, string>): RawFexPair {
  return {
    pair_id: row.pair_id ?? '',
    event_contract: row.event_contract ?? '',
    expiration_date: row.expiration_date ?? '',
    quantity: row.quantity ?? '',
    yes_price: row.yes_price ?? '',
    no_price: row.no_price ?? '',
    pair_time: row.pair_time ?? '',
  };
}

export function parsePairsCsv(text: string): RawFexPair[] {
  return parseCsv(text)
    .filter((row) => Boolean(row.pair_id))
    .map(asPair);
}

/**
 * The archive, indexed by contract.
 *
 * Only the YES rows are kept. Each contract appears twice, YES and NO, and the
 * NO row is the same session with the prices mirrored — `pair_quantity` and
 * `open_interest` are identical on both — so keeping it would double the map
 * for nothing. The key is upper-cased because that is the case the terminal's
 * tickers arrive in.
 */
export function parseArchiveCsv(text: string): Map<string, RawFexArchiveRow> {
  const rows = new Map<string, RawFexArchiveRow>();

  for (const row of parseCsv(text)) {
    const contract = row.event_contract ?? '';
    if (!contract || row.subtype !== 'YES') continue;
    rows.set(contract.toUpperCase(), {
      event_contract: contract,
      subtype: row.subtype,
      expiration_date: row.expiration_date ?? '',
      date: row.date ?? '',
      start_price: row.start_price ?? '',
      high_price: row.high_price ?? '',
      low_price: row.low_price ?? '',
      end_price: row.end_price ?? '',
      settlement_price: row.settlement_price ?? '',
      pair_quantity: row.pair_quantity ?? '',
      open_interest: row.open_interest ?? '',
      vwap: row.vwap ?? '',
    });
  }

  return rows;
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

async function api<T>(path: string, ttlMs: number): Promise<{ rows: T[]; nextPage: number | null }> {
  return cache.cached(`forecastex:${path}`, ttlMs, async () => {
    const envelope = await fetchJson<RawFexEnvelope>(`${API_BASE}${path}`, {
      // A 5,000-row catalogue page is ~2.9 MB and takes about 1.4 s to build.
      timeoutMs: 40_000,
      retries: 2,
    });
    return unwrap<T>(envelope);
  });
}

async function contractRow(id: string): Promise<RawFexContract> {
  const { rows } = await api<RawFexContract>(
    `/api/contracts${qs({ page: 1, pageSize: 1, contractId: id })}`,
    TTL.quote,
  );
  const row = rows[0];
  if (!row?.contract_id) {
    throw new UpstreamError(`No ForecastEx contract ${id}`, {
      code: 'not_found',
      hint:
        'ForecastEx names a contract PRODUCT_PERIOD_STRIKE, as in `fx:HORC_1126_REPUBLICAN` ' +
        'or `fx:FF_091726_3.625`. Settled contracts leave the catalogue on expiry.',
    });
  }
  return row;
}

/** Every contract of one product, which is the only way to reach an event. */
async function productContracts(productId: string): Promise<RawFexContract[]> {
  const rows: RawFexContract[] = [];

  // The largest product lists ~132 contracts, so one page is almost always the
  // lot; the loop is here for the day a daily product runs long.
  for (let page = 1; page <= 3; page++) {
    const result = await api<RawFexContract>(
      `/api/contracts${qs({
        page,
        pageSize: 1000,
        productId,
        sortBy: 'contract_id',
        sortOrder: 'asc',
      })}`,
      TTL.quote,
    );
    rows.push(...result.rows);
    if (result.nextPage === null) break;
  }

  return rows;
}

/**
 * The product registry, keyed by product id.
 *
 * All 1,004 products arrive in a single 929 KB request, and they carry the
 * `strike_unit` that decides how a contract id's last segment reads and the
 * `category` that `listSeries` filters on — so this is fetched once per
 * catalogue TTL and shared by every path that needs either.
 */
async function productIndex(): Promise<Map<string, RawFexProduct>> {
  return cache.cached('forecastex:products', TTL.catalogue, async () => {
    const { rows } = await api<RawFexProduct>(
      `/api/products${qs({ page: 1, pageSize: 2000 })}`,
      TTL.catalogue,
    );
    return new Map(rows.filter((row) => row.product_id).map((row) => [row.product_id ?? '', row]));
  });
}

/* ------------------------------------------------------- session archive */

interface SessionArchive {
  /** The session the figures belong to, `YYYYMMDD`. */
  date: string;
  rows: Map<string, RawFexArchiveRow>;
}

/** How far back to look for a published archive before giving up on the figures. */
const ARCHIVE_LOOKBACK = 5;

/**
 * The newest published end-of-session archive.
 *
 * The current session's file 404s until after 16:30 CT, and a holiday leaves a
 * gap of several days, so the newest available one is found by walking back a
 * bounded number of sessions rather than assumed. Which session answered is
 * carried on the result, because every figure taken from it is as old as that.
 *
 * A failure here is not fatal: turnover, the previous close and the daily move
 * simply stay unstated, which is what they were before the file existed.
 */
async function sessionArchive(): Promise<SessionArchive | null> {
  return cache.cached('forecastex:archive', TTL.catalogue, async () => {
    let date = sessionDate();

    for (let back = 0; back < ARCHIVE_LOOKBACK; back++) {
      try {
        const csv = await fetchText(`${ARCHIVE_BASE}/prices/daily_prices_${date}.csv`, {
          timeoutMs: 40_000,
          retries: 1,
          browserHeaders: false,
          headers: { Accept: 'text/csv,*/*' },
        });
        return { date, rows: parseArchiveCsv(csv) };
      } catch (err) {
        // 404 is the file not being published yet, which is the expected answer
        // for the session in progress. Anything else is a real failure and the
        // corpus goes without its ranking figures rather than hanging on it.
        if (!(err instanceof UpstreamError) || err.code !== 'not_found') return null;
        date = previousSession(date);
      }
    }

    return null;
  });
}

/* ----------------------------------------------------------------- lookups */

export async function getMarket(id: string): Promise<Market> {
  const raw = await contractRow(id);
  const products = await productIndex().catch(() => new Map<string, RawFexProduct>());
  const market = normaliseMarket(raw, products.get(raw.product_id ?? ''));

  const archive = await sessionArchive();
  const session = archive?.rows.get((raw.contract_id ?? '').toUpperCase());
  return session ? applySession(market, session) : market;
}

export async function getEvent(id: string): Promise<VenueEvent> {
  const eventTicker = eventTickerOf(id);
  const productId = eventTicker.split('_')[0] ?? '';

  const [rows, products, archive] = await Promise.all([
    productContracts(productId),
    productIndex().catch(() => new Map<string, RawFexProduct>()),
    sessionArchive(),
  ]);

  const legs = rows.filter((row) => eventTickerOf(row.contract_id ?? '') === eventTicker);
  if (legs.length === 0) {
    throw new UpstreamError(`No ForecastEx event ${id}`, {
      code: 'not_found',
      hint:
        'A ForecastEx event is the first two segments of a contract id — the product and ' +
        'its period — as in `fx:HORC_1126` or `fx:UHBKF_081726`.',
    });
  }

  return normaliseEvent(legs, products.get(productId), archive?.rows);
}

/**
 * Products as series, filtered on the category they state.
 *
 * Honest here in a way it is not at either Polymarket: every product row
 * carries its own category, so a filtered list is the real subset rather than
 * whatever the crawl happened to reach.
 */
export async function listSeries(category?: string): Promise<SeriesInfo[]> {
  const products = await productIndex();
  const want = category?.trim().toLowerCase();

  return [...products.values()]
    .filter((row) => !want || (row.category ?? '').toLowerCase() === want)
    .map(normaliseSeries)
    .sort((a, b) => a.ticker.localeCompare(b.ticker));
}

/* -------------------------------------------------- what the exchange lacks */

/**
 * There is no order book, and there is no endpoint that would serve one.
 *
 * ForecastEx runs a paired auction: a print matches a YES buyer against a NO
 * buyer and creates both positions at once, so what exists afterwards is a
 * print and an open-interest figure, not a bid and an ask. Nothing in the
 * public API — catalogue, prices, tape or archive — carries depth, and the
 * exchange's own contract page renders only "Yes:", "No:" and "Open Interest:".
 * Live quotes exist, but inside IBKR ForecastTrader behind its SSO.
 *
 * Standing a one-level book up from the last YES and NO prints would be worse
 * than refusing: those two prints are independent and routinely sum to 1.01, so
 * the ladder would show a crossed market that no one can trade.
 */
export function getOrderBook(_id: string, _depth = 12): Promise<OrderBook> {
  throw new UpstreamError('ForecastEx publishes no order book', {
    code: 'unsupported',
    hint:
      'The exchange matches by pairing a YES buyer with a NO buyer, so a print is all there ' +
      'is — /api/contracts states last_yes_price, last_no_price and open_interest, and no ' +
      'public endpoint carries depth. Live quotes are behind IBKR ForecastTrader’s SSO at ' +
      '/portal.proxy/v1/ft, which needs an Interactive Brokers login. DES, TAS and GP work.',
  });
}

/* ------------------------------------------------------------------- tape */

/**
 * The tape, filtered out of the whole-exchange session file.
 *
 * **There is no taker side, and that is structural rather than missing.** Every
 * print creates one YES holder and one NO holder simultaneously, so neither
 * side lifted the other and there is no aggressor to name. The field is emitted
 * empty; filling it in with `yes` would assert an initiative that the exchange's
 * own matching model rules out.
 *
 * `pair_time` carries a Central offset and *nanosecond* fractional seconds.
 * `Date.parse` reads the offset but not the nanos, so the fraction is truncated
 * to milliseconds first — left alone, some engines return `NaN` and the whole
 * tape falls to the epoch.
 */
export function normaliseTrades(rows: RawFexPair[], ticker: string, limit: number): TradesResponse {
  const want = ticker.toUpperCase();

  const trades: Trade[] = rows
    .filter((row) => row.event_contract.toUpperCase() === want)
    .flatMap((row) => {
      const ms = Date.parse(row.pair_time.replace(/(\.\d{3})\d+/, '$1'));
      const count = num(row.quantity);
      const yesPrice = num(row.yes_price);
      const noPrice = num(row.no_price);

      // A row the file truncated — S3 rewrites the tape every ten minutes and a
      // reader can arrive mid-write — is dropped rather than published. A Trade
      // carries plain numbers, so the alternative is a 1,000-lot at $0.00 dated
      // 1 January 1970 sitting at the top of the tape, which reads as a print.
      if (!Number.isFinite(ms) || count === null || yesPrice === null || noPrice === null) {
        return [];
      }

      return [
        {
          venue: VENUE,
          // An opaque base-32-looking string: an identity, never an amount.
          tradeId: row.pair_id,
          ticker: want,
          ts: Math.floor(ms / 1000),
          count,
          yesPrice,
          noPrice,
          takerSide: '',
          // No block or cross flag is published.
          isBlockTrade: false,
        },
      ];
    })
    // The file is roughly but not strictly time-ordered, so it is sorted rather
    // than reversed.
    .sort((a, b) => b.ts - a.ts)
    .slice(0, limit);

  // No cursor: the tape is one file per session, so a page is the file. Paging
  // deeper means the previous session's file, not an offset into this one.
  return { trades, cursor: null };
}

/**
 * One session's prints for the whole exchange, ~1.4 MB.
 *
 * Held for a minute rather than for the three seconds a quote gets: the file is
 * rewritten about every ten minutes, so a shorter TTL would re-download a
 * megabyte to learn nothing, and N panels on one contract share the one copy.
 */
async function pairsFile(date: string): Promise<RawFexPair[]> {
  return cache.cached(`forecastex:pairs:${date}`, TTL.meta, async () => {
    const csv = await fetchText(`${ARCHIVE_BASE}/pairs/pairs_${date}.csv`, {
      timeoutMs: 40_000,
      retries: 1,
      browserHeaders: false,
      headers: { Accept: 'text/csv,*/*' },
    });
    return parsePairsCsv(csv);
  });
}

/**
 * The tape for one contract, out of the two newest session files.
 *
 * Most contracts do not print every session — 982 of 11,466 traded on a typical
 * day — so an empty answer from the current file is the norm rather than a
 * fault, and the panel is better served by the last session that did trade.
 *
 * What must not happen is the two being confused. An empty tape means "nobody
 * traded this contract"; a missing file means "the exchange has not published
 * the tape yet", which is what the minutes after the 16:15 CT roll look like.
 * If neither session file exists, that is said rather than shown as a market
 * nobody trades.
 */
export async function getTrades(id: string, limit = 50): Promise<TradesResponse> {
  const today = sessionDate();
  let answered = false;
  let result: TradesResponse = { trades: [], cursor: null };

  for (const date of [today, previousSession(today)]) {
    const rows = await pairsFile(date).catch((err: unknown) => {
      // A 404 is the file not existing yet, or a holiday. Anything else is a
      // real failure and is worth reporting rather than reading as a quiet tape.
      if (err instanceof UpstreamError && err.code === 'not_found') return null;
      throw err;
    });
    if (rows === null) continue;

    answered = true;
    result = normaliseTrades(rows, id, limit);
    if (result.trades.length > 0) break;
  }

  if (!answered) {
    throw new UpstreamError('ForecastEx has not published a tape for this session yet', {
      code: 'not_found',
      hint:
        `The print tape is one whole-exchange file per session at ` +
        `${ARCHIVE_BASE}/pairs/pairs_YYYYMMDD.csv, and neither ${today} nor the session ` +
        `before it is there. Today’s file appears a few minutes after the 16:15 CT roll.`,
    });
  }

  return result;
}

/* ---------------------------------------------------------------- candles */

/** The two buckets `/api/prices` serves. Both are case-sensitive: `H` is a 400. */
const INTERVALS: Record<number, 'h' | 'd'> = { 60: 'h', 1440: 'd' };

/** The archive is two years deep, so nothing older can be asked for. */
const MAX_DAYS_BACK = 760;

/**
 * When a sample's period ends. `Candle.time` is a period *end* everywhere in
 * the terminal, and `/api/prices` labels its buckets by where they *begin*.
 *
 * Both feeds label, and neither says so. Reconciling the buckets against the
 * print tape settles it: for `UHLAX_081826_78` the bucket stamped
 * `2026-08-17T22:00:00Z` carries volume 158, and the tape's prints between
 * 22:00Z and 23:00Z are 21+10+30+94+3 = 158, with the bucket's price equal to
 * the last of them. The stamp opens the hour; the bar closes an hour later.
 *
 * The daily feed is the same shape one rung up, and it is a **UTC** calendar
 * day rather than the Central session day the exchange files its archive under.
 * Same contract: the daily point for `2026-08-17` reports volume 992, which is
 * exactly the sum of the hourly buckets stamped 8/17 in UTC, while the archive
 * row for the 8/17 *session* — 16:15 CT the previous day to 16:14 CT — reports
 * a different figure entirely (70,985 against 58,034 on `UHLAX_081726_77`).
 * Closing a daily bar at Central midnight, as the session boundary would
 * suggest, therefore stamps every one of them five hours late.
 */
export function sampleEnd(stamp: string | undefined, interval: CandleInterval): number | null {
  if (!stamp) return null;

  if (/^\d{4}-\d{2}-\d{2}$/.test(stamp)) {
    const year = Number(stamp.slice(0, 4));
    const month = Number(stamp.slice(5, 7));
    const day = Number(stamp.slice(8, 10));
    return Math.floor(Date.UTC(year, month - 1, day + 1) / 1000);
  }

  // A zoneless wall clock never appears on the hourly feed, but reading one as
  // local time would move the whole series by the server's own offset.
  const ms = Date.parse(/(?:Z|[+-]\d{2}:\d{2})$/.test(stamp) ? stamp : `${stamp}Z`);
  return Number.isFinite(ms) ? Math.floor(ms / 1000) + interval * 60 : null;
}

interface SampleBucket {
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number | null;
}

/**
 * Samples as bars.
 *
 * ForecastEx publishes a close and a size per bucket, never an OHLC bar, so
 * each bar's open, high and low are its close and the response says so. What is
 * *not* invented is a traded flag: the feed carries a sample in every bucket
 * whether or not anything printed, and `volume === 0` is the exchange
 * distinguishing a carried-forward mark from a real one.
 *
 * Buckets are merged rather than assumed unique, so a repeated stamp folds into
 * one bar with its sizes added instead of drawing two bars at one instant.
 */
export function bucketSamples(points: RawFexPricePoint[], interval: CandleInterval): Candle[] {
  const buckets = new Map<number, SampleBucket>();

  for (const point of points) {
    const price = num(point.yes_price);
    const end = sampleEnd(point.interval_date, interval);
    if (price === null || end === null) continue;

    const volume = num(point.volume);
    const bucket = buckets.get(end);
    if (!bucket) {
      buckets.set(end, { open: price, high: price, low: price, close: price, volume });
      continue;
    }
    bucket.high = Math.max(bucket.high, price);
    bucket.low = Math.min(bucket.low, price);
    bucket.close = price;
    if (volume !== null) bucket.volume = (bucket.volume ?? 0) + volume;
  }

  return [...buckets.entries()]
    .sort((a, b) => a[0] - b[0])
    .map(([time, bucket]) => ({
      time,
      open: bucket.open,
      high: bucket.high,
      low: bucket.low,
      close: bucket.close,
      volume: bucket.volume,
      // Stated per session in the archive only, never per bucket here.
      openInterest: null,
      traded: (bucket.volume ?? 0) > 0,
      bid: null,
      ask: null,
    }));
}

export async function getCandles(
  id: string,
  interval: CandleInterval,
  startTs: number,
  endTs: number,
): Promise<CandlesResponse> {
  const bucket = INTERVALS[interval];
  if (!bucket) {
    throw new UpstreamError('ForecastEx publishes no minute bars', {
      code: 'unsupported',
      hint:
        'GET /api/prices?contractId=…&interval=h|d is the whole history feed, and it rejects ' +
        'every other value: the finest bucket the exchange publishes is one hour. A minute ' +
        'grid would have to be rebuilt from the whole-exchange pairs tape by hand.',
    });
  }

  // The case trap: /api/prices answers an upper-cased id with HTTP 200 and an
  // empty series, so the canonical spelling has to be recovered first or the
  // chart is silently blank.
  const contractId = await canonicalContractId(id);

  const days = Math.min(
    Math.max(Math.ceil((Date.now() / 1000 - startTs) / 86_400) + 1, 1),
    MAX_DAYS_BACK,
  );

  const { rows } = await api<RawFexPricePoint>(
    `/api/prices${qs({ contractId, daysBack: days, interval: bucket })}`,
    TTL.candles,
  );

  const period = interval * 60;
  const candles = bucketSamples(rows, interval).filter(
    // The newest bar ends in the future while its session is still open, so the
    // window is generous by one period at the top rather than dropping it.
    (candle) => candle.time >= startTs && candle.time <= endTs + period,
  );

  return {
    venue: VENUE,
    ticker: id.toUpperCase(),
    seriesTicker: contractId.split('_')[0] ?? '',
    interval,
    candles,
    note:
      'ForecastEx publishes price samples rather than OHLC bars: each bar is one sample, so ' +
      'its high and low are its close. A zero-volume bar is the exchange carrying the ' +
      'previous mark forward. The feed labels each bucket by where it opens, so bars are ' +
      'stamped one period later here; a daily bar is a UTC day, not the exchange’s own ' +
      '16:15 CT session, and does not line up with the end-of-session archive.',
  };
}

/* ----------------------------------------------------------------- corpus */

const CATALOGUE_KEY = 'forecastex:catalogue';

interface Catalogue {
  corpus: Corpus;
  /**
   * Upper-cased id → the exact spelling the exchange publishes, which is the
   * only one `/api/prices` answers.
   */
  canonical: Map<string, string>;
}

/**
 * Crawled in three pages of 5,000, ordered by contract id.
 *
 * **The ordering is not a preference.** Crawling the same three pages by
 * `open_interest desc` returns 11,466 rows but only 10,397 distinct ids: ties
 * are not broken, so the offset window shifts between requests and about 9% of
 * the universe is silently dropped while another 9% arrives twice. Ordering by
 * a unique key fixes it, and the rows are deduped anyway — the failure was
 * invisible in the row count, and only a dedupe would have shown it.
 *
 * 5,000 is also the largest page that answers: the Lambda caps its response at
 * 6 MB and a larger page fails outright rather than truncating.
 */
const CORPUS_PAGE = 5000;
const CORPUS_MAX_PAGES = 4;

async function buildCatalogue(): Promise<Catalogue> {
  const rows: RawFexContract[] = [];
  const canonical = new Map<string, string>();
  let truncated = false;

  for (let page = 1; page <= CORPUS_MAX_PAGES; page++) {
    const result = await api<RawFexContract>(
      `/api/contracts${qs({
        page,
        pageSize: CORPUS_PAGE,
        sortBy: 'contract_id',
        sortOrder: 'asc',
      })}`,
      TTL.catalogue,
    );

    for (const row of result.rows) {
      const id = row.contract_id;
      if (!id || canonical.has(id.toUpperCase())) continue;
      canonical.set(id.toUpperCase(), id);
      rows.push(row);
    }

    if (result.nextPage === null) break;
    truncated = page === CORPUS_MAX_PAGES;
  }

  const [products, archive] = await Promise.all([
    productIndex().catch(() => new Map<string, RawFexProduct>()),
    sessionArchive(),
  ]);

  // Group on the event prefix in crawl order, which is contract id order, so
  // the legs of an event arrive together and a ladder keeps its rungs in order.
  const grouped = new Map<string, RawFexContract[]>();
  for (const row of rows) {
    const key = eventTickerOf(row.contract_id ?? '');
    const group = grouped.get(key);
    if (group) group.push(row);
    else grouped.set(key, [row]);
  }

  const events: VenueEvent[] = [];
  const markets: Market[] = [];
  for (const [, legs] of grouped) {
    const event = normaliseEvent(legs, products.get(legs[0]?.product_id ?? ''), archive?.rows);
    events.push(event);
    markets.push(...event.markets);
  }

  return {
    corpus: { venue: VENUE, events, markets, builtAt: Date.now(), truncated },
    canonical,
  };
}

async function catalogue(): Promise<Catalogue> {
  return cache.cached(CATALOGUE_KEY, TTL.catalogue, buildCatalogue);
}

/**
 * The exchange's own spelling of an identifier.
 *
 * Read from the warm catalogue when there is one, because that costs nothing;
 * otherwise from `/api/contracts?contractId=`, which is case-insensitive and
 * answers with the canonical row. The catalogue is *not* built on demand here —
 * a chart request should not pay for an 8.7 MB crawl when one 900-byte lookup
 * settles it.
 */
async function canonicalContractId(id: string): Promise<string> {
  const warm = cache.get<Catalogue>(CATALOGUE_KEY);
  const known = warm?.canonical.get(id.toUpperCase());
  if (known) return known;

  const row = await contractRow(id);
  return row.contract_id ?? id;
}

export async function corpusSnapshot(): Promise<Corpus> {
  return (await catalogue()).corpus;
}

export function warmCorpus(): void {
  corpusSnapshot()
    .then((c) => {
      console.log(
        `[forecastex] corpus warm: ${c.events.length} events, ${c.markets.length} markets`,
      );
    })
    .catch((err: unknown) => {
      console.warn('[forecastex] corpus warm failed:', err instanceof Error ? err.message : err);
    });
}

/**
 * Search the snapshot rather than `/api/contracts?search=`.
 *
 * The exchange's own search is decent — case-insensitive across id, question
 * and display name — but it ranks by nothing in particular and returns
 * contracts where every other venue here returns events. Ranking the local
 * snapshot instead is what makes a ForecastEx result comparable with a Kalshi
 * one for the same words.
 */
export async function search(query: string, limit = 25): Promise<SearchResponse> {
  return searchCorpus(await corpusSnapshot(), query, limit);
}

/**
 * Leaderboards over the snapshot.
 *
 * Open interest ranks on the live catalogue figure; volume and the movers rank
 * on the end-of-session archive merged in at crawl time, so they are a session
 * behind and read as the last full session's board. Contracts the archive did
 * not cover — anything listed since it was published — are dropped from those
 * boards by {@link rankMarkets} rather than ranked as zero.
 */
export async function topMarkets(sort: MoverSort, limit = 25): Promise<Market[]> {
  return rankMarkets((await corpusSnapshot()).markets, sort, limit);
}
