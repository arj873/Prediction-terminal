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
  KalshiEvent,
  Market,
  NetflixTop10,
  OptionChain,
  OptionExpiry,
  OptionPositioning,
  OptionQuoteResponse,
  OptionSurface,
  OptionUnderlying,
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
} from '../../shared/types.js';

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

/* ------------------------------------------------------------------ kalshi */

/** Mirrors the server's `EventSearchHit`. */
export interface EventSearchHit {
  event: Omit<KalshiEvent, 'markets'>;
  markets: Market[];
  volume24h: number;
  score: number;
}

export interface SearchResponse {
  query: string;
  hits: EventSearchHit[];
  scanned: number;
  snapshotAgeSeconds: number;
}

export const kalshi = {
  market: (ticker: string, signal?: AbortSignal): Promise<Market> =>
    request(`/kalshi/markets/${encodeURIComponent(ticker)}`, signal),

  orderBook: (ticker: string, depth = 12, signal?: AbortSignal): Promise<OrderBook> =>
    request(`/kalshi/markets/${encodeURIComponent(ticker)}/orderbook${query({ depth })}`, signal),

  trades: (ticker: string, limit = 50, signal?: AbortSignal): Promise<TradesResponse> =>
    request(`/kalshi/markets/${encodeURIComponent(ticker)}/trades${query({ limit })}`, signal),

  candles: (
    ticker: string,
    interval: CandleInterval,
    start?: number,
    end?: number,
    signal?: AbortSignal,
  ): Promise<CandlesResponse> =>
    request(
      `/kalshi/markets/${encodeURIComponent(ticker)}/candles${query({ interval, start, end })}`,
      signal,
    ),

  event: (eventTicker: string, signal?: AbortSignal): Promise<KalshiEvent> =>
    request(`/kalshi/events/${encodeURIComponent(eventTicker)}`, signal),

  search: (q: string, limit = 25, signal?: AbortSignal): Promise<SearchResponse> =>
    request(`/kalshi/search${query({ q, limit })}`, signal),

  top: (
    sort: string,
    limit = 25,
    signal?: AbortSignal,
  ): Promise<{ sort: string; markets: Market[] }> =>
    request(`/kalshi/top${query({ sort, limit })}`, signal),
};

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

/* ----------------------------------------------------------------- options */

export interface OptionExpiriesResponse {
  symbol: string;
  name: string;
  assetClass: AssetClass;
  spot: number | null;
  expiries: OptionExpiry[];
  venue: string;
  source: string;
}

export const options = {
  underlyings: (
    signal?: AbortSignal,
  ): Promise<{ underlyings: OptionUnderlying[]; note?: string }> =>
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
    interval: CandleInterval = 60,
    start?: number,
    end?: number,
    signal?: AbortSignal,
  ): Promise<OptionQuoteResponse> =>
    request(
      `/options/contract/${encodeURIComponent(contract)}${query({ interval, start, end })}`,
      signal,
    ),
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
  time: string;
}

export const health = (signal?: AbortSignal): Promise<Health> => request('/health', signal);
