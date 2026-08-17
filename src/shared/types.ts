/**
 * Wire contract shared by the server routes and the browser client.
 *
 * Everything the server hands back is already normalised: prices are plain
 * numbers in dollars (0..1 for a binary contract), sizes and volumes are plain
 * numbers, and timestamps are unix seconds (UTC). Each upstream has its own
 * dialect — Kalshi speaks fixed-point decimal *strings* (`"0.6900"`,
 * `"12645.98"`), Polymarket wraps prices in `{value, currency}` objects and
 * ships JSON arrays as JSON *strings* — and none of that leaks past the server
 * boundary. A normalised {@link Market} from Kalshi and one from either
 * Polymarket are the same shape, and carry a {@link Venue} saying which.
 */

import type { Venue } from './venue.js';

export type { Venue } from './venue.js';

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

/* ----------------------------------------------------------------- markets */

export type MarketStatus =
  | 'unopened'
  | 'open'
  | 'closed'
  | 'settled'
  | 'determined'
  | 'finalized'
  | string;

export interface Market {
  venue: Venue;
  /**
   * The contract's identifier at its venue: a Kalshi ticker
   * (`KXFEDDECISION-26SEP-T3.75`) or a Polymarket market slug. Upper-case at
   * Kalshi, lower-case at both Polymarkets — the venue decides, not the caller.
   */
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
  /**
   * Contracts traded, ever and in the last 24h.
   *
   * `null` means *this venue does not publish the figure*, which is not the
   * same as zero and must not render as one: Polymarket US's public catalogue
   * carries no volume at all, and a `0` in that column would read as a dead
   * market rather than an unanswered question. The same rule governs
   * {@link openInterest} and {@link liquidity}.
   */
  volume: number | null;
  volume24h: number | null;
  openInterest: number | null;
  /** Resting depth, in dollars. */
  liquidity: number | null;
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

/**
 * One question, with every contract that resolves it.
 *
 * All three venues group markets this way — Kalshi calls it an event, both
 * Polymarkets call it an event too — so it is the unit the terminal compares
 * across brokers.
 */
export interface VenueEvent {
  venue: Venue;
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
  events: VenueEvent[];
  cursor: string | null;
}

/** One row of the resting order book, price ascending. */
export interface BookLevel {
  price: number;
  size: number;
}

export interface OrderBook {
  venue: Venue;
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
  venue: Venue;
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

/**
 * Candlestick period, in minutes.
 *
 * The only values Kalshi accepts, and therefore the grid the whole terminal
 * uses: an implied-price line, a spot chart and a Polymarket price history are
 * only readable against each other if they sit on the same buckets.
 */
export type CandleInterval = 1 | 60 | 1440;

export interface Candle {
  /** Period *end*, unix seconds. */
  time: number;
  open: number;
  high: number;
  low: number;
  close: number;
  /**
   * Contracts traded in the period, and open interest at its end. `null` where
   * the venue's history carries prices only — Polymarket International
   * publishes a price series with no size attached, and drawing that as a zero
   * volume bar would assert something the upstream never said.
   */
  volume: number | null;
  openInterest: number | null;
  /**
   * False when nothing printed in the period and the candle was carried
   * forward from the previous close (Kalshi returns only `previous_dollars`
   * there; Polymarket simply has no sample in the bucket).
   */
  traded: boolean;
  bid: number | null;
  ask: number | null;
}

export interface CandlesResponse {
  venue: Venue;
  ticker: string;
  seriesTicker: string;
  interval: CandleInterval;
  candles: Candle[];
  /**
   * How the bars were obtained, when it is not the venue's own candle feed.
   * Polymarket International publishes a price *sample* series rather than
   * OHLC, so its bars are aggregated here and say so instead of implying a
   * traded high and low that the upstream never stated.
   */
  note?: string;
}

export interface SeriesInfo {
  venue: Venue;
  ticker: string;
  title: string;
  category: string;
  frequency: string;
  tags: string[];
}

/* ------------------------------------------------------------- cross-venue */

/**
 * How sure the terminal is that two venues are listing the same question.
 *
 * `linked` is the only band that is *stated* rather than inferred: it comes
 * from the curated table of series identifiers, which is checked against live
 * catalogues. Everything below it is a text match, and is labelled as one, so
 * a trader never mistakes a guess for a fact.
 */
export type MatchConfidence = 'linked' | 'strong' | 'likely' | 'weak';

export const MATCH_CONFIDENCES: readonly MatchConfidence[] = [
  'linked',
  'strong',
  'likely',
  'weak',
] as const;

/** One venue's listing of a series that at least one other venue also lists. */
export interface SeriesLeg {
  venue: Venue;
  /** Series identifier at that venue: `KXFEDDECISION`, `fomc`, `fed-decision`. */
  seriesTicker: string;
  title: string;
  /** Open events in the series. */
  events: number;
  /** Open contracts across those events. */
  markets: number;
  volume24h: number | null;
  /** Soonest close across the series' open events, ISO. */
  closeTime: string;
  /** The event that stands for the series — what a click opens. */
  sampleEvent: string;
}

/** A recurring question the terminal believes more than one broker lists. */
export interface LinkedSeries {
  /** Stable canonical key, e.g. `fed-decision`. */
  key: string;
  title: string;
  /** The venues carrying it, busiest first. Always two or more. */
  legs: SeriesLeg[];
  confidence: MatchConfidence;
  /** Why these were paired — a curated link, or the terms that matched. */
  reason: string;
  score: number;
}

export interface LinkedSeriesResponse {
  query: string;
  series: LinkedSeries[];
  /** Series scanned, per venue. */
  scanned: Record<string, number>;
  /**
   * Venues that did not answer. A board covering two of three brokers has to
   * say which one is missing, or a "no match" reads as "no such market".
   */
  unavailable: { venue: Venue; error: string }[];
  snapshotAgeSeconds: number;
}

/** One venue's quote for a contract in a side-by-side comparison. */
export interface CompareLeg {
  venue: Venue;
  ticker: string;
  eventTicker: string;
  yesBid: number | null;
  yesAsk: number | null;
  mid: number | null;
  volume24h: number | null;
}

/** One outcome, quoted at every venue that lists it. */
export interface CompareRow {
  /** The outcome, as the richest venue words it. */
  label: string;
  legs: CompareLeg[];
  /** Richest mid minus cheapest mid, in dollars. `null` under two quotes. */
  divergence: number | null;
  /**
   * Cheapest ask anywhere minus the richest bid anywhere. Positive means the
   * books are crossed between brokers — buy the ask at one, sell the bid at the
   * other — before fees, latency and the fact that both legs must fill.
   */
  edge: number | null;
  /** The two venues that edge is between, cheap side first. */
  edgeVenues: Venue[];
}

export interface CompareEventLeg {
  venue: Venue;
  eventTicker: string;
  title: string;
  closeTime: string;
  /** Confidence that this event is the same question as the first leg's. */
  confidence: MatchConfidence;
  score: number;
  reason: string;
}

export interface CompareResponse {
  title: string;
  /** The events being compared, the anchor first. */
  events: CompareEventLeg[];
  rows: CompareRow[];
  /**
   * Contracts only one venue lists. Named rather than dropped — a ladder that
   * is finer at one broker is information, not noise.
   */
  unmatched: { venue: Venue; label: string; ticker: string }[];
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

/* ------------------------------------------------------------ economic data */

/**
 * One observation of a published statistic.
 *
 * Eight publishers feed this shape and every one of them marks a missing
 * reading differently — FRED writes `.`, the BLS omits the month, SDMX sends an
 * empty `OBS_VALUE`. All of them arrive here as `null`, because a gap in a
 * series is a fact about the series and dropping the row would silently
 * compress the time axis.
 */
export interface DataObservation {
  /** `YYYY-MM-DD`. A quarter or a year is dated to the day it begins. */
  date: string;
  value: number | null;
}

/**
 * What a series *is*, in enough detail to read the numbers.
 *
 * A statistic without its units, frequency and vintage is noise: 333.918 is a
 * CPI index level, a percent or billions of dollars depending on facts that
 * live here and nowhere else in the payload.
 */
export interface DataSeries {
  /** Which publisher, as a {@link DataSourceId}. */
  provider: string;
  /** The identifier at that publisher — `UNRATE`, `LNS14000000`, `EXR/D.USD.EUR.SP00.A`. */
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
  /**
   * Which arm of the provider answered — `scrape`, `api`, `v1`, `v2`. Shown in
   * the panel header, because how much to trust a number depends on whether it
   * came from the source of record or a fallback behind it.
   */
  source: string;
  /** The publisher's own page for this series, so a reader can check it. */
  sourceUrl: string;
}

export interface DataSeriesResponse {
  series: DataSeries;
  observations: DataObservation[];
}

export interface DataSearchResult {
  /** Which publisher, as a {@link DataSourceId}. */
  provider: string;
  id: string;
  title: string;
  units?: string;
  frequency?: string;
  seasonalAdjustment?: string;
  observationRange?: string;
}

export interface DataSearchResponse {
  query: string;
  results: DataSearchResult[];
  /**
   * Providers that were asked and did not answer. A board covering six of eight
   * publishers has to say which two are missing, or "no match" reads as "no such
   * series anywhere".
   */
  unavailable: { provider: string; error: string }[];
  /** Providers that were skipped for want of a credential, and what to set. */
  skipped: { provider: string; hint: string }[];
}

/** What a deployment can currently serve, for the `SRC` board and `/health`. */
export interface DataSourceStatus {
  id: string;
  label: string;
  code: string;
  prefix: string;
  kind: string;
  covers: string;
  site: string;
  idExample: string;
  /** False when a required credential is unset. */
  available: boolean;
  /** Why it is unavailable, or what a key would add when one is optional. */
  note: string;
}

/* -------------------------------------------------------------------- fred */

// FRED is one of the eight series publishers rather than a shape of its own.
// The aliases keep the older, FRED-specific names meaning what they always did.
export type FredObservation = DataObservation;
export type FredSeries = DataSeries;
export type FredSeriesResponse = DataSeriesResponse;
export type FredSearchResult = DataSearchResult;
export type FredSearchResponse = DataSearchResponse;

/* -------------------------------------------------------- cftc: commitments */

/** One trader category's position in a Commitments of Traders report. */
export interface CotCategory {
  /** e.g. `Non-commercial`, `Dealer/Intermediary`, `Managed money`. */
  name: string;
  long: number | null;
  short: number | null;
  /** Spreading contracts, where the report breaks them out. `null` where it does not. */
  spreading: number | null;
  /** `long - short`. */
  net: number | null;
  /** Week-on-week change in {@link net}, when the prior report is available. */
  netChange: number | null;
  /** Share of open interest held long, 0..100. */
  percentLong: number | null;
  percentShort: number | null;
  traderCount: number | null;
}

export interface CotReport {
  /** `legacy`, `disaggregated` or `financial`. */
  report: string;
  reportLabel: string;
  /** CFTC's own market name, e.g. `GOLD - COMMODITY EXCHANGE INC.`. */
  market: string;
  exchange: string;
  /** CFTC contract market code — the stable identity behind the name. */
  contractCode: string;
  /** Report Tuesday, `YYYY-MM-DD`. */
  date: string;
  openInterest: number | null;
  openInterestChange: number | null;
  categories: CotCategory[];
  sourceUrl: string;
}

export interface CotMarket {
  contractCode: string;
  market: string;
  exchange: string;
  /** Most recent report date for this market, `YYYY-MM-DD`. */
  latest: string;
  openInterest: number | null;
}

export interface CotMarketsResponse {
  query: string;
  report: string;
  markets: CotMarket[];
}

/* ---------------------------------------------------------------- congress */

/** One action in a bill's legislative history. */
export interface BillAction {
  /** `YYYY-MM-DD`. */
  date: string;
  text: string;
  /** `House`, `Senate` or `` when Congress.gov states none. */
  chamber: string;
}

export interface Bill {
  /** Congress number, e.g. `119`. */
  congress: number;
  /** `hr`, `s`, `hjres`, `sjres`, `hconres`, `sconres`, `hres`, `sres`. */
  type: string;
  number: string;
  title: string;
  /** `HR 1 (119th)` — what a reader would say out loud. */
  label: string;
  originChamber: string;
  introducedDate: string;
  /** The most recent action, which is what "where is this bill" means. */
  latestAction: BillAction | null;
  sponsor: string;
  sponsorParty: string;
  sponsorState: string;
  cosponsors: number | null;
  policyArea: string;
  /** Whether the bill has become law, as Congress.gov's own action text states it. */
  becameLaw: boolean;
  url: string;
}

export interface BillDetail extends Bill {
  /** Congress.gov's plain-language summary, newest version. Empty when none is filed. */
  summary: string;
  /** Full action history, newest first. */
  actions: BillAction[];
  committees: string[];
}

export interface BillSearchResponse {
  query: string;
  congress: number | null;
  bills: Bill[];
  source: string;
}

/* --------------------------------------------------------------- sec edgar */

export interface SecCompany {
  /** Zero-padded 10-digit CIK, as EDGAR writes it. */
  cik: string;
  name: string;
  tickers: string[];
  exchanges: string[];
  /** Standard Industrial Classification code and its description. */
  sic: string;
  sicDescription: string;
  fiscalYearEnd: string;
  url: string;
}

export interface SecFiling {
  /** EDGAR accession number, e.g. `0000320193-24-000123`. */
  accession: string;
  /** `10-K`, `8-K`, `4`, `13F-HR`… */
  form: string;
  /** `YYYY-MM-DD`. */
  filed: string;
  /** Period the filing reports on, `YYYY-MM-DD`. Empty for forms with no period. */
  reportDate: string;
  /** Items cited on an 8-K, e.g. `2.02,9.01`. Empty for other forms. */
  items: string;
  primaryDocument: string;
  description: string;
  size: number | null;
  url: string;
}

export interface SecFilingsResponse {
  company: SecCompany;
  filings: SecFiling[];
  /** Forms present in the response, so a panel can offer a filter. */
  forms: string[];
}

/** One reported value of an XBRL fact — the unit of SEC financial history. */
export interface SecFact {
  /** Period end, `YYYY-MM-DD`. */
  end: string;
  /** Period start for a duration fact; empty for an instant. */
  start: string;
  value: number;
  /** Fiscal year and period the issuer filed it under, e.g. `2024` / `Q3`. */
  fiscalYear: number | null;
  fiscalPeriod: string;
  form: string;
  filed: string;
  accession: string;
  /** Unit of measure, e.g. `USD`, `shares`. */
  unit: string;
}

export interface SecConceptResponse {
  company: SecCompany;
  /** `us-gaap`, `ifrs-full`, `dei`. */
  taxonomy: string;
  /** The XBRL tag, e.g. `Revenues`. */
  tag: string;
  label: string;
  description: string;
  unit: string;
  /** Every reported value, oldest first. Restatements included and dated. */
  facts: SecFact[];
}

export interface SecSearchResult {
  cik: string;
  ticker: string;
  name: string;
}

/* ---------------------------------------------------------------- data.gov */

export interface DataGovDataset {
  id: string;
  title: string;
  description: string;
  /** Publishing organisation, e.g. `Bureau of Labor Statistics`. */
  publisher: string;
  /** Subject tags the publisher filed it under. */
  themes: string[];
  /** Last modified, `YYYY-MM-DD`. */
  modified: string;
  /** How often it is updated, in plain words. */
  frequency: string;
  /** Downloadable distributions, richest format first. */
  formats: string[];
  /** The dataset's landing page, or its first distribution when it has none. */
  url: string;
}

export interface DataGovSearchResponse {
  query: string;
  datasets: DataGovDataset[];
  /** Opaque cursor for the next page, or `null` at the end. */
  cursor: string | null;
  source: string;
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

/* -------------------------------------------------------------------- news */

/** One headline from the news wire. All text is plain — no markup, no escapes. */
export interface NewsArticle {
  /** Upstream article id, kept as text: it is an identity, never an amount. */
  id: string;
  headline: string;
  /** One-paragraph precis. Empty for a headline-only item, which is normal. */
  summary: string;
  author: string;
  /** Who filed it, e.g. `benzinga`. */
  publisher: string;
  /** Article link. Empty when the item has none, or its scheme was not http(s). */
  url: string;
  /** Publication time, unix seconds (UTC). */
  time: number;
  /** Last edit, unix seconds. Equal to {@link time} for an unrevised item. */
  updated: number;
  /** Tickers the publisher tagged, e.g. `["NVDA", "AMD"]`. */
  symbols: string[];
}

export interface NewsFeed {
  /** Symbols the wire was filtered to; empty means everything. */
  symbols: string[];
  /** How many days back the window reaches — the panel states it. */
  days: number;
  /** Newest first, by publication time. */
  articles: NewsArticle[];
  /** Provider, shown in the panel so the wire is never passed off as ours. */
  source: string;
  sourceUrl: string;
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
