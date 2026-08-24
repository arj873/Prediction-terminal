/**
 * Typed client for the terminal's own API.
 *
 * Every call funnels through {@link request}, which turns a non-2xx response
 * into an {@link ApiRequestError} carrying the server's `code` and `hint`. The
 * panels surface that hint verbatim — it is the difference between "request
 * failed" and "FRED is blocking this IP; set FRED_API_KEY".
 *
 * The response types are not written here: they are generated from the Rust
 * wire structs into `$gen`, so a field renamed on the server is a compile error
 * in the panel that reads it rather than an `undefined` on screen.
 */

import type {
  ApiError,
  AssetClass,
  BillboardChart,
  BillboardChartsResponse,
  BoxOfficeDay,
  CandlesResponse,
  CatalogueSnapshot,
  CompareResponse,
  EntGenreFilter,
  AwardResult,
  EntResponse,
  DataSearchResponse,
  DataSourcesResponse,
  DataSeriesResponse,
  OptionChain,
  OptionExpiriesResponse,
  OptionPositioning,
  OptionQuoteResponse,
  OptionSurface,
  OptionUnderlyingsResponse,
  HealthResponse,
  ImpliedCandidatesResponse,
  ImpliedMethod,
  ImpliedSeriesResponse,
  ImpliedUnderlyingsResponse,
  LinkedSeriesResponse,
  Market,
  NetflixTop10,
  NewsFeed,
  OrderBook,
  RtSearchResponse,
  RtTitle,
  SearchResponse,
  SpotCandlesResponse,
  SpotQuote,
  SpotSearchResponse,
  PodcastChart,
  ReleaseList,
  TrendList,
  SteamChart,
  StreamChart,
  StreamChartsResponse,
  TopResponse,
  TradesResponse,
  TvSchedule,
  Venue,
  VenueEvent,
} from '$gen';
import { formatRef, normaliseId, type VenueRef } from '../terminal/venue';

/**
 * The bar sizes the terminal charts.
 *
 * Read off the generated response rather than restated, because the Rust
 * `CandleInterval` is a validated newtype that ts-rs emits inline at each use
 * site instead of exporting under its own name.
 */
export type CandleInterval = CandlesResponse['interval'];

export class ApiRequestError extends Error {
  readonly code: string;
  readonly hint: string | undefined;
  readonly status: number;

  constructor(message: string, code: string, status: number, hint?: string) {
    super(message);
    this.name = 'ApiRequestError';
    this.code = code;
    this.status = status;
    this.hint = hint;
  }
}

async function request<T>(path: string, signal?: AbortSignal): Promise<T> {
  let res: Response;
  try {
    res = await fetch(`/api${path}`, { signal, headers: { Accept: 'application/json' } });
  } catch (err) {
    if ((err as Error)?.name === 'AbortError') throw err;
    throw new ApiRequestError(
      'Cannot reach the terminal API server',
      'network_error',
      0,
      'Is the API server running? `npm run dev` starts both halves.',
    );
  }

  const body: unknown = await res.json().catch(() => null);

  if (!res.ok) {
    const error = (body ?? {}) as Partial<ApiError>;
    throw new ApiRequestError(
      error.error ?? `Request failed with HTTP ${res.status}`,
      error.code ?? 'http_error',
      res.status,
      error.hint,
    );
  }

  return body as T;
}

function query(params: Record<string, string | number | undefined>): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== '') search.set(key, String(value));
  }
  const s = search.toString();
  return s ? `?${s}` : '';
}

/* ------------------------------------------------------------------ venues */

function at(ref: VenueRef, path = ''): string {
  return `/venue/${ref.venue}/markets/${encodeURIComponent(ref.id)}${path}`;
}

/**
 * The market API, for any of the brokers in the venue registry.
 *
 * Every call takes a {@link VenueRef} rather than a bare identifier, so a panel
 * cannot ask one venue for another's ticker — the mistake that would otherwise
 * surface as an unexplained 404.
 */
export const venue = {
  market: (ref: VenueRef, signal?: AbortSignal): Promise<Market> => request(at(ref), signal),

  orderBook: (ref: VenueRef, depth = 12, signal?: AbortSignal): Promise<OrderBook> =>
    request(`${at(ref, '/orderbook')}${query({ depth })}`, signal),

  trades: (ref: VenueRef, limit = 50, signal?: AbortSignal): Promise<TradesResponse> =>
    request(`${at(ref, '/trades')}${query({ limit })}`, signal),

  candles: (
    ref: VenueRef,
    interval: CandleInterval,
    start?: number,
    end?: number,
    signal?: AbortSignal,
  ): Promise<CandlesResponse> =>
    request(`${at(ref, '/candles')}${query({ interval, start, end })}`, signal),

  event: (ref: VenueRef, signal?: AbortSignal): Promise<VenueEvent> =>
    request(`/venue/${ref.venue}/events/${encodeURIComponent(ref.id)}`, signal),

  search: (v: Venue, q: string, limit = 25, signal?: AbortSignal): Promise<SearchResponse> =>
    request(`/venue/${v}/search${query({ q, limit })}`, signal),

  top: (v: Venue, sort: string, limit = 25, signal?: AbortSignal): Promise<TopResponse> =>
    request(`/venue/${v}/top${query({ sort, limit })}`, signal),

  catalogue: (v: Venue, signal?: AbortSignal): Promise<CatalogueSnapshot> =>
    request(`/venue/${v}/catalogue`, signal),
};

/* ------------------------------------------------------------- cross-venue */

export const xv = {
  series: (q = '', limit = 40, signal?: AbortSignal): Promise<LinkedSeriesResponse> =>
    request(`/xv/series${query({ q, limit })}`, signal),

  compare: (event: string, v?: Venue, signal?: AbortSignal): Promise<CompareResponse> =>
    request(`/xv/compare${query({ event, venue: v })}`, signal),
};

/** Re-export so panels can build a ref without importing two modules. */
export { formatRef, normaliseId };
export type { VenueRef };

/* -------------------------------------------------------------------- spot */

export const spot = {
  quote: (assetClass: AssetClass, symbol: string, signal?: AbortSignal): Promise<SpotQuote> =>
    request(`/spot/${assetClass}/${encodeURIComponent(symbol)}`, signal),

  candles: (
    assetClass: AssetClass,
    symbol: string,
    interval: CandleInterval,
    start?: number,
    end?: number,
    signal?: AbortSignal,
  ): Promise<SpotCandlesResponse> =>
    request(
      `/spot/${assetClass}/${encodeURIComponent(symbol)}/candles${query({ interval, start, end })}`,
      signal,
    ),

  search: (
    q: string,
    assetClass?: AssetClass,
    limit = 20,
    signal?: AbortSignal,
  ): Promise<SpotSearchResponse> =>
    request(`/spot/search${query({ q, class: assetClass, limit })}`, signal),
};

/* ----------------------------------------------------------------- implied */

export const implied = {
  underlyings: (signal?: AbortSignal): Promise<ImpliedUnderlyingsResponse> =>
    request('/implied/underlyings', signal),

  candidates: (symbol: string, signal?: AbortSignal): Promise<ImpliedCandidatesResponse> =>
    request(`/implied/candidates${query({ symbol })}`, signal),

  series: (
    eventTicker: string,
    interval: CandleInterval,
    start?: number,
    end?: number,
    method: ImpliedMethod = 'median',
    signal?: AbortSignal,
  ): Promise<ImpliedSeriesResponse> =>
    request(
      `/implied/series${query({ event: eventTicker, interval, start, end, method })}`,
      signal,
    ),
};

/* ------------------------------------------------------------ data sources */

export const data = {
  /**
   * Chart `[source:]id` at whichever publisher the prefix names.
   *
   * The reference is *not* URL-encoded as one component: half of these
   * identifiers contain slashes — an SDMX key is `EXR/D.USD.EUR.SP00.A` and an
   * EIA id is a route path — and the route takes the rest of the path as the
   * reference precisely so they survive. Each segment is encoded on its own, so
   * a `#` or a `?` inside one still cannot end the path early.
   */
  series: (
    reference: string,
    start?: string,
    end?: string,
    signal?: AbortSignal,
  ): Promise<DataSeriesResponse> =>
    request(
      `/data/series/${reference.split('/').map(encodeURIComponent).join('/')}${query({ start, end })}`,
      signal,
    ),

  search: (
    q: string,
    sources: readonly string[] = [],
    limit = 40,
    signal?: AbortSignal,
  ): Promise<DataSearchResponse> =>
    request(
      `/data/search${query({ q, sources: sources.length ? sources.join(',') : undefined, limit })}`,
      signal,
    ),

  sources: (signal?: AbortSignal): Promise<DataSourcesResponse> => request('/data/sources', signal),
};

/* ----------------------------------------------------------------- options */

/**
 * Four views over one option board.
 *
 * The symbol decides the venue rather than the caller: an underlying either has
 * a Deribit board or it does not, and `OPT BTC` should not have to be spelled
 * differently from `OPT AAPL`. A contract lookup is routed on the *shape* of
 * the identifier instead, so an OCC symbol and a Deribit instrument name can
 * both be pasted straight in.
 */
export const options = {
  underlyings: (signal?: AbortSignal): Promise<OptionUnderlyingsResponse> =>
    request('/options/underlyings', signal),

  expiries: (symbol: string, signal?: AbortSignal): Promise<OptionExpiriesResponse> =>
    request(`/options/${encodeURIComponent(symbol)}/expiries`, signal),

  chain: (symbol: string, expiry?: string, signal?: AbortSignal): Promise<OptionChain> =>
    request(`/options/${encodeURIComponent(symbol)}/chain${query({ expiry })}`, signal),

  surface: (symbol: string, expiry?: string, signal?: AbortSignal): Promise<OptionSurface> =>
    request(`/options/${encodeURIComponent(symbol)}/surface${query({ expiry })}`, signal),

  positioning: (
    symbol: string,
    expiry?: string,
    signal?: AbortSignal,
  ): Promise<OptionPositioning> =>
    request(`/options/${encodeURIComponent(symbol)}/positioning${query({ expiry })}`, signal),

  contract: (
    contract: string,
    interval?: number,
    signal?: AbortSignal,
  ): Promise<OptionQuoteResponse> =>
    request(`/options/contract/${encodeURIComponent(contract)}${query({ interval })}`, signal),
};

/* -------------------------------------------------------------------- fred */

/**
 * FRED's own two endpoints, kept because `FRED <id>` predates the other eleven
 * publishers and every habit, example and README line in this terminal says it.
 * `ECO` reaches the same series through `data.series`.
 */
export const fred = {
  series: (
    id: string,
    start?: string,
    end?: string,
    signal?: AbortSignal,
  ): Promise<DataSeriesResponse> =>
    request(`/fred/series/${encodeURIComponent(id)}${query({ start, end })}`, signal),

  search: (q: string, limit = 25, signal?: AbortSignal): Promise<DataSearchResponse> =>
    request(`/fred/search${query({ q, limit })}`, signal),
};

/* --------------------------------------------------------------- billboard */

export const billboard = {
  chart: (slug: string, date?: string, signal?: AbortSignal): Promise<BillboardChart> =>
    request(`/billboard/chart/${encodeURIComponent(slug)}${query({ date })}`, signal),

  charts: (signal?: AbortSignal): Promise<BillboardChartsResponse> =>
    request('/billboard/charts', signal),
};

/* -------------------------------------------------------------------- news */

export const news = {
  /** Headlines, newest first. No symbols means the whole wire. */
  feed: (symbols: string[], limit = 30, days = 7, signal?: AbortSignal): Promise<NewsFeed> =>
    request(`/news${query({ symbols: symbols.join(','), limit, days })}`, signal),
};

/* ----------------------------------------------------------- entertainment */

export const ent = {
  /** Kalshi's entertainment book, grouped by genre. */
  markets: (genre: EntGenreFilter, limit = 60, signal?: AbortSignal): Promise<EntResponse> =>
    request(`/ent/markets${query({ genre, limit })}`, signal),

  rt: (q: string, signal?: AbortSignal): Promise<RtTitle> =>
    request(`/ent/rt${query({ q })}`, signal),

  rtSearch: (q: string, limit = 20, signal?: AbortSignal): Promise<RtSearchResponse> =>
    request(`/ent/rt/search${query({ q, limit })}`, signal),

  netflix: (category: string, scope: string, signal?: AbortSignal): Promise<NetflixTop10> =>
    request(`/ent/netflix${query({ category, scope })}`, signal),

  spotify: (
    scope: string,
    period: string,
    limit = 200,
    signal?: AbortSignal,
  ): Promise<StreamChart> => request(`/ent/spotify${query({ scope, period, limit })}`, signal),

  youtube: (view: string, limit = 200, signal?: AbortSignal): Promise<StreamChart> =>
    request(`/ent/youtube${query({ view, limit })}`, signal),

  charts: (source?: string, signal?: AbortSignal): Promise<StreamChartsResponse> =>
    request(`/ent/charts${query({ source })}`, signal),

  boxOffice: (date?: string, signal?: AbortSignal): Promise<BoxOfficeDay> =>
    request(`/ent/boxoffice${query({ date })}`, signal),

  steam: (q?: string, limit = 25, signal?: AbortSignal): Promise<SteamChart> =>
    request(`/ent/steam${query({ q, limit })}`, signal),

  tv: (date?: string, country?: string, signal?: AbortSignal): Promise<TvSchedule> =>
    request(`/ent/tv${query({ date, country })}`, signal),

  /** Nominees and winners, from Wikidata. Omit the year for every ceremony. */
  awards: (q: string, year?: number, limit = 300, signal?: AbortSignal): Promise<AwardResult> =>
    request(`/ent/awards${query({ q, year, limit })}`, signal),

  trends: (geo?: string, limit = 25, signal?: AbortSignal): Promise<TrendList> =>
    request(`/ent/trends${query({ geo, limit })}`, signal),

  releases: (q: string, kind?: string, limit = 25, signal?: AbortSignal): Promise<ReleaseList> =>
    request(`/ent/releases${query({ q, kind, limit })}`, signal),

  podcasts: (
    view?: string,
    country?: string,
    limit = 50,
    signal?: AbortSignal,
  ): Promise<PodcastChart> => request(`/ent/podcasts${query({ view, country, limit })}`, signal),
};

/* ------------------------------------------------------------------ health */

export const health = (signal?: AbortSignal): Promise<HealthResponse> => request('/health', signal);
