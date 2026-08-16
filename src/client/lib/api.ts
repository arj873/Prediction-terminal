/**
 * Typed client for the terminal's own API.
 *
 * Every call funnels through {@link request}, which turns a non-2xx response
 * into an {@link ApiRequestError} carrying the server's `code` and `hint`. The
 * panels surface that hint verbatim — it is the difference between "request
 * failed" and "FRED is blocking this IP; set FRED_API_KEY".
 */

import type {
  ApiError,
  AssetClass,
  CompareResponse,
  BillboardChart,
  BillboardChartListItem,
  BoxOfficeDay,
  CandleInterval,
  CandlesResponse,
  EntGenre,
  EntResponse,
  FredSearchResponse,
  FredSeriesResponse,
  ImpliedCandidatesResponse,
  ImpliedMethod,
  ImpliedSeriesResponse,
  LinkedSeriesResponse,
  Market,
  NetflixTop10,
  NewsFeed,
  OrderBook,
  RtSearchResponse,
  RtTitle,
  SpotCandlesResponse,
  SpotQuote,
  SpotSearchResult,
  SteamChart,
  StreamChart,
  StreamChartListItem,
  TradesResponse,
  TvSchedule,
  Venue,
  VenueEvent,
} from '../../shared/types.js';
import { formatRef, normaliseId, type VenueRef } from '../../shared/venue.js';

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

/** Mirrors the server's `EventSearchHit`. */
export interface EventSearchHit {
  event: Omit<VenueEvent, 'markets'>;
  markets: Market[];
  volume24h: number | null;
  score: number;
}

export interface SearchResponse {
  query: string;
  hits: EventSearchHit[];
  scanned: number;
  snapshotAgeSeconds: number;
  truncated: boolean;
}

export interface CatalogueInfo {
  venue: Venue;
  events: number;
  markets: number;
  truncated: boolean;
  ageSeconds: number;
}

function at(ref: VenueRef, path = ''): string {
  return `/venue/${ref.venue}/markets/${encodeURIComponent(ref.id)}${path}`;
}

/**
 * The market API, for any of the three brokers.
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

  top: (
    v: Venue,
    sort: string,
    limit = 25,
    signal?: AbortSignal,
  ): Promise<{ venue: Venue; sort: string; markets: Market[] }> =>
    request(`/venue/${v}/top${query({ sort, limit })}`, signal),

  catalogue: (v: Venue, signal?: AbortSignal): Promise<CatalogueInfo> =>
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
  ): Promise<{ query: string; results: SpotSearchResult[] }> =>
    request(`/spot/search${query({ q, class: assetClass, limit })}`, signal),
};

/* ----------------------------------------------------------------- implied */

export interface UnderlyingInfo {
  symbol: string;
  name: string;
  assetClass: AssetClass;
  aliases: string[];
}

export const implied = {
  underlyings: (signal?: AbortSignal): Promise<{ underlyings: UnderlyingInfo[] }> =>
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

/* -------------------------------------------------------------------- fred */

export const fred = {
  series: (
    id: string,
    start?: string,
    end?: string,
    signal?: AbortSignal,
  ): Promise<FredSeriesResponse> =>
    request(`/fred/series/${encodeURIComponent(id)}${query({ start, end })}`, signal),

  search: (q: string, limit = 25, signal?: AbortSignal): Promise<FredSearchResponse> =>
    request(`/fred/search${query({ q, limit })}`, signal),
};

/* --------------------------------------------------------------- billboard */

export const billboard = {
  chart: (slug: string, date?: string, signal?: AbortSignal): Promise<BillboardChart> =>
    request(`/billboard/chart/${encodeURIComponent(slug)}${query({ date })}`, signal),

  charts: (signal?: AbortSignal): Promise<{ charts: BillboardChartListItem[] }> =>
    request('/billboard/charts', signal),
};

/* -------------------------------------------------------------------- news */

export const news = {
  /** Headlines, newest first. No symbols means the whole wire. */
  feed: (
    symbols: string[],
    limit = 30,
    days = 7,
    signal?: AbortSignal,
  ): Promise<NewsFeed> =>
    request(`/news${query({ symbols: symbols.join(','), limit, days })}`, signal),
};

/* ----------------------------------------------------------- entertainment */

export const ent = {
  /** Kalshi's entertainment book, grouped by genre. */
  markets: (genre: EntGenre | 'all', limit = 60, signal?: AbortSignal): Promise<EntResponse> =>
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

  charts: (source?: string, signal?: AbortSignal): Promise<{ charts: StreamChartListItem[] }> =>
    request(`/ent/charts${query({ source })}`, signal),

  boxOffice: (date?: string, signal?: AbortSignal): Promise<BoxOfficeDay> =>
    request(`/ent/boxoffice${query({ date })}`, signal),

  steam: (q?: string, limit = 25, signal?: AbortSignal): Promise<SteamChart> =>
    request(`/ent/steam${query({ q, limit })}`, signal),

  tv: (date?: string, country?: string, signal?: AbortSignal): Promise<TvSchedule> =>
    request(`/ent/tv${query({ date, country })}`, signal),
};

/* ------------------------------------------------------------------ health */

export interface Health {
  ok: boolean;
  uptimeSeconds: number;
  cache: { hits: number; misses: number; entries: number; evictions: number };
  fredApiKey: boolean;
  /** Whether this deployment can serve `NEWS` — the feed needs a key pair. */
  alpacaKeys: boolean;
  time: string;
}

export const health = (signal?: AbortSignal): Promise<Health> => request('/health', signal);
