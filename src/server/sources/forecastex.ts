/** Placeholder — replaced by the real client. */
import type {
  CandleInterval,
  CandlesResponse,
  Market,
  OrderBook,
  SeriesInfo,
  TradesResponse,
  VenueEvent,
} from '../../shared/types.js';
import { UpstreamError } from '../lib/http.js';
import type { Corpus, MoverSort, SearchResponse } from './corpus.js';

const VENUE = 'forecastex' as const;
const todo = (): never => {
  throw new UpstreamError('Not implemented', { code: 'unsupported' });
};

export function getMarket(_id: string): Promise<Market> { return todo(); }
export function getOrderBook(_id: string, _depth = 12): Promise<OrderBook> { return todo(); }
export function getTrades(_id: string, _limit = 50): Promise<TradesResponse> { return todo(); }
export function getCandles(
  _id: string,
  _interval: CandleInterval,
  _startTs: number,
  _endTs: number,
): Promise<CandlesResponse> { return todo(); }
export function getEvent(_id: string): Promise<VenueEvent> { return todo(); }
export function listSeries(_category?: string): Promise<SeriesInfo[]> { return todo(); }
export function search(_query: string, _limit = 25): Promise<SearchResponse> { return todo(); }
export function topMarkets(_sort: MoverSort, _limit = 25): Promise<Market[]> { return todo(); }
export function corpusSnapshot(): Promise<Corpus> { return todo(); }
export function warmCorpus(): void {
  void VENUE;
}
