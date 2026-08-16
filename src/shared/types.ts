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

/* ----------------------------------------------------------------- options */

export type OptionType = 'call' | 'put';

/**
 * The Greek set, in the units a trader reads them in.
 *
 * `null` rather than `0` throughout: a contract with no derivable volatility
 * has *unknown* sensitivities, and a zero delta is a real and very different
 * statement. See `shared/greeks.ts` for the units of each.
 */
export interface OptionGreeks {
  /** Per 1 unit of underlying. Spot delta, not forward delta. */
  delta: number | null;
  /** Delta per 1 unit of underlying. */
  gamma: number | null;
  /** Per 1 volatility point — a move from 40% to 41%. */
  vega: number | null;
  /** Per calendar day. */
  theta: number | null;
  /** Per 1 percentage point of interest rate. */
  rho: number | null;
}

/** Where a contract's volatility number came from. */
export type IvSource = 'solved' | 'venue';

export interface OptionContract {
  /** The venue's own instrument id, e.g. `AAPL260918C00300000`, `BTC-25DEC26-104000-C`. */
  contract: string;
  type: OptionType;
  strike: number;
  /** Expiry instant, unix seconds. */
  expiry: number;
  bid: number | null;
  ask: number | null;
  /** Book mid, or the venue's mark when the book is one-sided. */
  mid: number | null;
  last: number | null;
  change: number | null;
  /** The venue's own mark price, where it publishes one. */
  mark: number | null;
  volume: number | null;
  openInterest: number | null;
  /** Decimal, so `0.42` is 42%. */
  iv: number | null;
  ivSource: IvSource | null;
  greeks: OptionGreeks;
  /** Model value at {@link iv}. Equals {@link mid} when the vol was solved from it. */
  theo: number | null;
  intrinsic: number | null;
  /** Premium over intrinsic — what actually decays. */
  extrinsic: number | null;
  /** Underlying price at which this contract returns its premium at expiry. */
  breakeven: number | null;
  inTheMoney: boolean;
}

/** One expiry on the board, as offered in the picker. */
export interface OptionExpiry {
  /** Expiry instant, unix seconds. */
  expiry: number;
  /** `YYYY-MM-DD`. */
  date: string;
  /** Act/365. */
  yearsToExpiry: number;
  daysToExpiry: number;
  contracts: number;
  openInterest: number;
  volume: number;
}

/**
 * How the forward for an expiry was arrived at.
 *
 * `parity` and `venue` are observed; `assumed` means nothing in the market
 * would say, and the number came from a configured rate. The panel shows which,
 * because a Greek is only as trustworthy as its carry.
 */
export type ForwardSource = 'parity' | 'venue' | 'assumed';

export interface OptionChain {
  symbol: string;
  name: string;
  assetClass: AssetClass;
  currency: string;
  /** Underlying price now. */
  spot: number | null;
  /** Forward for this expiry. */
  forward: number | null;
  forwardSource: ForwardSource;
  /** `e^(-rT)` for this expiry. */
  discountFactor: number | null;
  /** Continuous rate implied by the discount factor. */
  rate: number | null;
  /** Continuous dividend/borrow yield implied by spot against the forward. */
  carry: number | null;
  expiry: OptionExpiry;
  /** Every expiry on the board, for the picker. */
  expiries: OptionExpiry[];
  calls: OptionContract[];
  puts: OptionContract[];
  /** At-the-money volatility for this expiry, decimal. */
  atmIv: number | null;
  /** Contracts per unit of underlying — 100 for US listed equity options, 1 on Deribit. */
  contractSize: number;
  /** Exchange the quotes came from. */
  venue: string;
  source: string;
  /** Anything the reader needs to know to read the numbers correctly. */
  note?: string;
}

/** One rung of the volatility smile. */
export interface OptionSmilePoint {
  strike: number;
  /** `K/S`. */
  moneyness: number | null;
  callIv: number | null;
  putIv: number | null;
  /** The rung's headline vol — out-of-the-money side, which is the traded one. */
  iv: number | null;
  volume: number;
  openInterest: number;
}

/** One expiry on the at-the-money term structure. */
export interface OptionTermPoint {
  expiry: number;
  date: string;
  daysToExpiry: number;
  atmIv: number | null;
  forward: number | null;
  openInterest: number;
  volume: number;
}

export interface OptionSurface {
  symbol: string;
  name: string;
  assetClass: AssetClass;
  spot: number | null;
  expiry: OptionExpiry;
  forward: number | null;
  atmIv: number | null;
  smile: OptionSmilePoint[];
  term: OptionTermPoint[];
  /** 25-delta put vol minus 25-delta call vol — the smile's asymmetry. */
  skew: number | null;
  venue: string;
  source: string;
  note?: string;
}

/** Open interest and volume at one strike, both legs. */
export interface OptionStrikeStat {
  strike: number;
  callOpenInterest: number;
  putOpenInterest: number;
  callVolume: number;
  putVolume: number;
  /** Total writer payout if the underlying settled here. */
  painPayout: number | null;
}

export interface OptionPositioning {
  symbol: string;
  name: string;
  assetClass: AssetClass;
  spot: number | null;
  expiry: OptionExpiry;
  strikes: OptionStrikeStat[];
  /** Strike where the least option value pays out — a positioning read, not a forecast. */
  maxPain: number | null;
  totalCallOpenInterest: number;
  totalPutOpenInterest: number;
  totalCallVolume: number;
  totalPutVolume: number;
  putCallOpenInterest: number | null;
  putCallVolume: number | null;
  venue: string;
  source: string;
  note?: string;
}

/** A single contract with its chain context — what `OPD` shows. */
export interface OptionQuoteResponse {
  symbol: string;
  name: string;
  assetClass: AssetClass;
  currency: string;
  spot: number | null;
  forward: number | null;
  forwardSource: ForwardSource;
  rate: number | null;
  carry: number | null;
  contractSize: number;
  contract: OptionContract;
  /** The other leg at the same strike and expiry, when the venue lists it. */
  pair: OptionContract | null;
  /** Price history, where the venue publishes any. Empty otherwise. */
  history: SpotCandle[];
  /** Why {@link history} is empty, when it is. */
  historyNote?: string;
  venue: string;
  source: string;
}

export interface OptionUnderlying {
  symbol: string;
  name: string;
  assetClass: AssetClass;
  venue: string;
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

/* ----------------------------------------------------- entertainment: kalshi */

/**
 * Genre tags for a Kalshi entertainment event.
 *
 * Tags, not a single category, because the clusters genuinely overlap: an Oscar
 * market is both `film` and `awards`, and someone typing `ENT film` expects to
 * see it. An event carries every tag that applies.
 */
export type EntGenre = 'music' | 'film' | 'tv' | 'games' | 'awards' | 'celeb';

export const ENT_GENRES: readonly EntGenre[] = [
  'music',
  'film',
  'tv',
  'games',
  'awards',
  'celeb',
] as const;

export interface EntEvent {
  eventTicker: string;
  seriesTicker: string;
  title: string;
  subTitle: string;
  genres: EntGenre[];
  /** Markets in the event, most liquid first. */
  markets: Market[];
  volume24h: number;
  openInterest: number;
  /** Soonest close across the event's markets, ISO — the clock that matters. */
  closeTime: string;
  /** The data feed Kalshi settles this series against, when we know of one. */
  feed?: EntFeed;
}

/** A terminal command that shows the data a market settles against. */
export interface EntFeed {
  /** Command to run, e.g. `RT dune part three`. */
  command: string;
  /** Human label for the upstream, e.g. `Rotten Tomatoes`. */
  source: string;
}

export interface EntResponse {
  genre: EntGenre | 'all';
  events: EntEvent[];
  /** Events scanned in the snapshot. */
  scanned: number;
  snapshotAgeSeconds: number;
  /** Per-genre counts, so the panel can show what else is available. */
  counts: Record<string, number>;
}

/* ------------------------------------------------ entertainment: rotten tomatoes */

export interface RtScore {
  /** 0..100, or `null` when the score has not been issued yet. */
  score: number | null;
  /** Average critic/user rating on the source's own scale, e.g. `8.40`. */
  averageRating: string;
  reviewCount: number | null;
  /** `certified fresh` / `fresh` / `rotten`, as RT states it. */
  state: string;
  certified: boolean;
}

export interface RtTitle {
  /** RT url slug, e.g. `dune_part_two`. */
  slug: string;
  title: string;
  year: string;
  mediaType: string;
  /** The Tomatometer — critics. */
  critics: RtScore;
  /** The Popcornmeter — verified audience. */
  audience: RtScore;
  synopsis: string;
  sourceUrl: string;
}

export interface RtSearchResult {
  slug: string;
  title: string;
  year: string;
  mediaType: string;
  criticsScore: number | null;
}

export interface RtSearchResponse {
  query: string;
  results: RtSearchResult[];
}

/* ------------------------------------------------------ entertainment: netflix */

export interface NetflixEntry {
  rank: number;
  title: string;
  /** Season label for TV rows; `""` when Netflix reports `N/A`. */
  season: string;
  /** Views for the week (global feed only; country feeds are rank-only). */
  views: number | null;
  hoursViewed: number | null;
  /** Runtime in hours, as Netflix publishes it. */
  runtime: number | null;
  weeksInTop10: number | null;
}

export interface NetflixTop10 {
  /** `global`, or an ISO-3166 alpha-2 country code. */
  scope: string;
  scopeLabel: string;
  /** `tv` or `films`. */
  category: string;
  categoryLabel: string;
  /** Week ending, `YYYY-MM-DD`. */
  week: string;
  entries: NetflixEntry[];
  sourceUrl: string;
}

/* ------------------------------------------- entertainment: spotify / youtube */

export interface StreamEntry {
  rank: number;
  /** Previous position; `null` for a debut. */
  lastRank: number | null;
  title: string;
  artist: string;
  /** Streams or views for the period. */
  streams: number | null;
  /** Change vs the previous period. */
  streamsChange: number | null;
  /** Cumulative total since entering the chart. */
  total: number | null;
  peak: number | null;
  days: number | null;
  move: number | null;
  isNew: boolean;
}

export interface StreamChart {
  /** `spotify` or `youtube`. */
  source: string;
  /** Chart identifier, e.g. `us-daily`. */
  chart: string;
  title: string;
  /** Date the chart covers, when the page states one. */
  date: string;
  entries: StreamEntry[];
  sourceUrl: string;
}

export interface StreamChartListItem {
  slug: string;
  name: string;
  source: string;
}

/* --------------------------------------------------- entertainment: box office */

export interface BoxOfficeEntry {
  rank: number;
  /** Yesterday's rank; `null` when absent. */
  lastRank: number | null;
  title: string;
  /** Gross for the day, in whole dollars. */
  gross: number | null;
  /** Percent change vs the previous day. */
  changeDay: number | null;
  /** Percent change vs the same day last week. */
  changeWeek: number | null;
  theaters: number | null;
  /** Per-theatre average, in dollars. */
  average: number | null;
  totalGross: number | null;
  daysInRelease: number | null;
  distributor: string;
  move: number | null;
  isNew: boolean;
}

export interface BoxOfficeDay {
  /** `YYYY-MM-DD`. */
  date: string;
  title: string;
  entries: BoxOfficeEntry[];
  /** Summed gross across the chart, in dollars. */
  totalGross: number;
  sourceUrl: string;
}

/* -------------------------------------------------------- entertainment: steam */

export interface SteamGame {
  appId: number;
  name: string;
  rank: number | null;
  /** Players in game right now. */
  currentPlayers: number | null;
  /** Highest concurrent players in the last 24h. */
  peakPlayers: number | null;
}

export interface SteamChart {
  /** `top` for the most-played leaderboard, or `game` for a single title. */
  view: string;
  games: SteamGame[];
  sourceUrl: string;
}

/* ------------------------------------------------------- entertainment: tv guide */

export interface TvEpisode {
  /** `HH:MM`, network local time. */
  airtime: string;
  show: string;
  network: string;
  season: number | null;
  episode: number | null;
  name: string;
  /** Minutes, when the schedule states one. */
  runtime: number | null;
  type: string;
}

export interface TvSchedule {
  /** `YYYY-MM-DD`. */
  date: string;
  country: string;
  episodes: TvEpisode[];
  sourceUrl: string;
}
