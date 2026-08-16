/**
 * One interface over the three brokers.
 *
 * Every venue module already exports the same handful of verbs; this states
 * that as a type and hands back the right module for a {@link Venue}, so the
 * routes and the cross-venue code are written once instead of three times.
 *
 * The interface is deliberately the union of what the venues *can* answer, not
 * the intersection — Polymarket US has no public candles or tape, and it throws
 * a described `unsupported` error rather than the type pretending the method is
 * absent. A terminal that silently omitted a chart would be worse than one that
 * says why there isn't one.
 */

import type {
  CandleInterval,
  CandlesResponse,
  Market,
  OrderBook,
  SeriesInfo,
  TradesResponse,
  Venue,
  VenueEvent,
} from '../../shared/types.js';
import type { Corpus, MoverSort, SearchResponse } from './corpus.js';
import * as kalshi from './kalshi.js';
import * as polymarket from './polymarket.js';
import * as polymarketUs from './polymarketus.js';

export interface VenueSource {
  getMarket(id: string): Promise<Market>;
  getOrderBook(id: string, depth?: number): Promise<OrderBook>;
  getTrades(id: string, limit?: number): Promise<TradesResponse>;
  getCandles(
    id: string,
    interval: CandleInterval,
    startTs: number,
    endTs: number,
  ): Promise<CandlesResponse>;
  getEvent(id: string): Promise<VenueEvent>;
  listSeries(category?: string): Promise<SeriesInfo[]>;
  search(query: string, limit?: number): Promise<SearchResponse>;
  topMarkets(sort: MoverSort, limit?: number): Promise<Market[]>;
  corpusSnapshot(): Promise<Corpus>;
  warmCorpus(): void;
}

const SOURCES: Record<Venue, VenueSource> = {
  kalshi,
  polymarket,
  'polymarket-us': polymarketUs,
};

export function sourceFor(venue: Venue): VenueSource {
  return SOURCES[venue];
}

/** Warm every catalogue in the background, so the first search pays for none. */
export function warmAll(): void {
  for (const source of Object.values(SOURCES)) source.warmCorpus();
}
