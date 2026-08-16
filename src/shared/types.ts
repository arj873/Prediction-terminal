/**
 * Wire contract shared by the server routes and the browser client.
 *
 * Everything the server hands back is already normalised: prices are plain
 * numbers in dollars (0..1 for a Kalshi binary contract), sizes and volumes are
 * plain numbers, and timestamps are unix seconds (UTC). The upstream Kalshi API
 * speaks in fixed-point decimal *strings* (`"0.6900"`, `"12645.98"`); none of
 * that leaks past the server boundary.
 */

/* ------------------------------------------------------------------ errors */

export interface ApiError {
  error: string;
  /** Machine-readable reason, e.g. `upstream_blocked`, `not_found`. */
  code: string;
  /** Operator-facing remediation hint, shown verbatim in the terminal. */
  hint?: string;
  /** Upstream HTTP status, when the failure came from a remote host. */
  status?: number;
}

/* ------------------------------------------------------------------ kalshi */

export type MarketStatus =
  | 'unopened'
  | 'open'
  | 'closed'
  | 'settled'
  | 'determined'
  | 'finalized'
  | string;

export interface Market {
  ticker: string;
  eventTicker: string;
  seriesTicker: string;
  title: string;
  /** Short label for the YES leg, e.g. `"82.5° or above"`. */
  yesSubTitle: string;
  noSubTitle: string;
  status: MarketStatus;
  marketType: string;
  /** Dollars, 0..1. `null` when the book side is empty. */
  yesBid: number | null;
  yesAsk: number | null;
  noBid: number | null;
  noAsk: number | null;
  /** Midpoint of the YES book, or last price when the book is one-sided. */
  mid: number | null;
  lastPrice: number | null;
  previousPrice: number | null;
  /** `lastPrice - previousPrice`, in dollars. */
  change: number | null;
  volume: number;
  volume24h: number;
  openInterest: number;
  liquidity: number;
  openTime: string;
  closeTime: string;
  expirationTime: string;
  /** Settlement result once determined: `"yes"`, `"no"`, or `""`. */
  result: string;
  rulesPrimary: string;
  category?: string;
  /**
   * How this contract's strike relates to the settlement value. `null` on a
   * plain yes/no market that has no numeric strike at all.
   *
   * `greater` — settles YES above {@link floorStrike}
   * `less`    — settles YES at or below {@link capStrike}
   * `between` — settles YES inside `[floorStrike, capStrike]`
   */
  strikeType: StrikeType | null;
  /** Lower bound of the YES region, in the underlying's units. */
  floorStrike: number | null;
  /** Upper bound of the YES region, in the underlying's units. */
  capStrike: number | null;
}

export type StrikeType = 'greater' | 'greater_or_equal' | 'less' | 'less_or_equal' | 'between';

export interface KalshiEvent {
  eventTicker: string;
  seriesTicker: string;
  title: string;
  subTitle: string;
  category: string;
  mutuallyExclusive: boolean;
  markets: Market[];
}

export interface MarketsResponse {
  markets: Market[];
  cursor: string | null;
}

export interface EventsResponse {
  events: KalshiEvent[];
  cursor: string | null;
}

/** One row of the resting order book, price ascending. */
export interface BookLevel {
  price: number;
  size: number;
}

export interface OrderBook {
  ticker: string;
  /** Resting YES bids, best (highest) first. */
  yes: BookLevel[];
  /** Resting NO bids, best (highest) first. */
  no: BookLevel[];
  /**
   * The NO book expressed in YES terms (`1 - noPrice`), best (lowest) first —
   * i.e. the YES ask ladder. Derived server-side so the client never has to
   * remember the inversion.
   */
  yesAsks: BookLevel[];
  bestYesBid: number | null;
  bestYesAsk: number | null;
  spread: number | null;
  mid: number | null;
}

export interface Trade {
  tradeId: string;
  ticker: string;
  ts: number;
  count: number;
  yesPrice: number;
  noPrice: number;
  /** Which side the aggressor lifted. */
  takerSide: 'yes' | 'no' | string;
  isBlockTrade: boolean;
}

export interface TradesResponse {
  trades: Trade[];
  cursor: string | null;
}

/** Candlestick period, in minutes. The only values Kalshi accepts. */
export type CandleInterval = 1 | 60 | 1440;

export interface Candle {
  /** Period *end*, unix seconds. */
  time: number;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
  openInterest: number;
  /**
   * False when no trade printed in the period and the candle was synthesised
   * from the previous close (Kalshi returns only `previous_dollars` there).
   */
  traded: boolean;
  bid: number | null;
  ask: number | null;
}

export interface CandlesResponse {
  ticker: string;
  seriesTicker: string;
  interval: CandleInterval;
  candles: Candle[];
}

export interface SeriesInfo {
  ticker: string;
  title: string;
  category: string;
  frequency: string;
  tags: string[];
}

/* -------------------------------------------------------------------- spot */

/**
 * Which upstream family a symbol is priced from. This is a routing decision,
 * not a taxonomy: `stock` covers equities, ETFs and cash indices, because they
 * all come from the same equity feed.
 */
export type AssetClass = 'stock' | 'crypto';

/** Live quote for an underlying. Prices are in the instrument's own currency. */
export interface SpotQuote {
  symbol: string;
  assetClass: AssetClass;
  name: string;
  currency: string;
  price: number | null;
  /** Previous session's close for equities, price 24h ago for crypto. */
  previousClose: number | null;
  change: number | null;
  changePercent: number | null;
  dayOpen: number | null;
  dayHigh: number | null;
  dayLow: number | null;
  volume: number | null;
  /** Exchange or venue the print came from, e.g. `NASDAQ`, `Coinbase`. */
  venue: string;
  /** When the price was observed, unix seconds. */
  time: number;
  /** Which provider answered — surfaced in the panel header. */
  source: string;
}

/**
 * One bar of an underlying's price history.
 *
 * Deliberately shares {@link CandleInterval} with Kalshi: an implied-price line
 * is only readable against the true price if both sit on the same buckets.
 */
export interface SpotCandle {
  /** Period *start*, unix seconds — the convention both upstreams use. */
  time: number;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
}

export interface SpotCandlesResponse {
  symbol: string;
  assetClass: AssetClass;
  name: string;
  currency: string;
  interval: CandleInterval;
  candles: SpotCandle[];
  source: string;
}

export interface SpotSearchResult {
  symbol: string;
  name: string;
  assetClass: AssetClass;
  venue: string;
  /** True when the terminal knows a Kalshi ladder that prices this symbol. */
  hasImplied: boolean;
}

/* ----------------------------------------------------------------- implied */

/** How a strike ladder is collapsed into one number. */
export type ImpliedMethod = 'median' | 'mean';

/**
 * A Kalshi ladder that prices an underlying, as offered in the picker.
 *
 * One candidate is one *event* — a single expiry with its full strike ladder.
 * A single strike cannot imply a price; the ladder is the unit of choice.
 */
export interface ImpliedCandidate {
  eventTicker: string;
  seriesTicker: string;
  title: string;
  subTitle: string;
  /** When the ladder settles, ISO 8601. Empty when Kalshi does not state one. */
  strikeDate: string;
  /** Contracts in the ladder with a usable numeric strike. */
  strikes: number;
  /** How many of those are quoted (a two-sided book or a last print). */
  quoted: number;
  /** Summed 24h volume across the ladder, in contracts. */
  volume24h: number;
  /** Lowest and highest strike, so the picker can show the covered range. */
  strikeLow: number | null;
  strikeHigh: number | null;
  /** Implied price from the live book right now, or `null` if underivable. */
  implied: number | null;
  /** Probability mass sitting outside the quoted strike range. */
  tailMass: number | null;
}

export interface ImpliedCandidatesResponse {
  symbol: string;
  assetClass: AssetClass;
  name: string;
  candidates: ImpliedCandidate[];
  /**
   * Why the list is empty, when it is. "No Kalshi ladder prices this symbol" is
   * an answer, not a failure — the panel shows this instead of an error.
   */
  note?: string;
}

export interface ImpliedPoint {
  /** Bucket end, unix seconds — aligned to the Kalshi candle grid. */
  time: number;
  /** Implied price of the underlying, or `null` when the ladder was unusable. */
  value: number | null;
  /** Strikes that were quoted in this bucket. */
  strikes: number;
  /** Probability mass outside the quoted strike range in this bucket. */
  tailMass: number;
}

export interface ImpliedSeriesResponse {
  eventTicker: string;
  title: string;
  strikeDate: string;
  method: ImpliedMethod;
  interval: CandleInterval;
  /** Tickers of the contracts that fed the calculation. */
  contributors: string[];
  /** Ladder contracts that were skipped for having no quotes at all. */
  skipped: number;
  points: ImpliedPoint[];
}

/* -------------------------------------------------------------------- fred */

export interface FredObservation {
  /** `YYYY-MM-DD`. */
  date: string;
  /** `null` for FRED's `.` missing-value marker. */
  value: number | null;
}

export interface FredSeries {
  id: string;
  title: string;
  units: string;
  unitsShort: string;
  frequency: string;
  seasonalAdjustment: string;
  lastUpdated: string;
  observationStart: string;
  observationEnd: string;
  notes: string;
  /** Where the payload came from — the terminal shows this in the panel header. */
  source: 'scrape' | 'api';
}

export interface FredSeriesResponse {
  series: FredSeries;
  observations: FredObservation[];
}

export interface FredSearchResult {
  id: string;
  title: string;
  units?: string;
  frequency?: string;
  seasonalAdjustment?: string;
  observationRange?: string;
}

export interface FredSearchResponse {
  query: string;
  results: FredSearchResult[];
  source: 'scrape' | 'api';
}

/* --------------------------------------------------------------- billboard */

export interface BillboardEntry {
  rank: number;
  title: string;
  artist: string;
  /** Last week's rank; `null` for a new entry. */
  lastWeek: number | null;
  peak: number | null;
  weeksOnChart: number | null;
  imageUrl: string | null;
  /** `rank` improvement vs `lastWeek`; positive means the entry moved up. */
  move: number | null;
  isNew: boolean;
}

export interface BillboardChart {
  /** Slug, e.g. `hot-100`. */
  chart: string;
  title: string;
  /** Chart week, `YYYY-MM-DD`. */
  date: string;
  entries: BillboardEntry[];
  sourceUrl: string;
}

export interface BillboardChartListItem {
  slug: string;
  name: string;
}
