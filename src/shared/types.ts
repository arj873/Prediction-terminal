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
}

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
