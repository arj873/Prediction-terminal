/**
 * `/api/venue/:venue/…` — the same market routes for every broker.
 *
 * One router serves all three, because the normalisers already made their
 * payloads the same shape. The venue segment is validated here so a bad name
 * fails at the door with the list of good ones, rather than as a 404 from
 * whichever upstream happened to be asked.
 */

import { Router } from 'express';
import type { CandleInterval, Venue } from '../../shared/types.js';
import { VENUES, isVenue, normaliseId } from '../../shared/venue.js';
import { UpstreamError } from '../lib/http.js';
import type { MoverSort } from '../sources/corpus.js';
import { sourceFor } from '../sources/venues.js';
import { asyncRoute, intParam, pathParam } from './helpers.js';

export const venueRouter: Router = Router({ mergeParams: true });

const VALID_INTERVALS = new Set<number>([1, 60, 1440]);
const SORTS = new Set(['volume', 'gainers', 'losers', 'open_interest', 'liquidity']);

function venueParam(params: Record<string, string | string[]>): Venue {
  const raw = pathParam(params, 'venue');
  if (!isVenue(raw)) {
    throw new UpstreamError(`Unknown venue "${raw}"`, {
      code: 'bad_request',
      hint: `Venues are: ${VENUES.map((v) => v.id).join(', ')}.`,
    });
  }
  return raw;
}

/** The venue and the identifier, cased the way that venue answers to. */
function ref(params: Record<string, string | string[]>, name = 'id'): { venue: Venue; id: string } {
  const venue = venueParam(params);
  return { venue, id: normaliseId(venue, pathParam(params, name)) };
}

venueRouter.get(
  '/markets/:id',
  asyncRoute(async (req) => {
    const { venue, id } = ref(req.params);
    return sourceFor(venue).getMarket(id);
  }),
);

venueRouter.get(
  '/markets/:id/orderbook',
  asyncRoute(async (req) => {
    const { venue, id } = ref(req.params);
    return sourceFor(venue).getOrderBook(id, intParam(req.query['depth'], 12, 1, 100));
  }),
);

venueRouter.get(
  '/markets/:id/trades',
  asyncRoute(async (req) => {
    const { venue, id } = ref(req.params);
    return sourceFor(venue).getTrades(id, intParam(req.query['limit'], 50, 1, 1000));
  }),
);

venueRouter.get(
  '/markets/:id/candles',
  asyncRoute(async (req) => {
    const { venue, id } = ref(req.params);
    const interval = intParam(req.query['interval'], 60, 1, 1440);
    if (!VALID_INTERVALS.has(interval)) {
      throw new UpstreamError(`Unsupported candle interval "${interval}"`, {
        code: 'bad_request',
        hint: 'The terminal charts 1 (1m), 60 (1h) and 1440 (1d) minute candles only.',
      });
    }

    const now = Math.floor(Date.now() / 1000);
    const defaultSpan = interval === 1 ? 6 * 3600 : interval === 60 ? 30 * 86400 : 365 * 86400;
    const endTs = intParam(req.query['end'], now, 0, now + 86400);
    const startTs = intParam(req.query['start'], endTs - defaultSpan, 0, endTs);

    if (startTs >= endTs) {
      throw new UpstreamError('Candle window start must be before end', { code: 'bad_request' });
    }

    return sourceFor(venue).getCandles(id, interval as CandleInterval, startTs, endTs);
  }),
);

venueRouter.get(
  '/events/:id',
  asyncRoute(async (req) => {
    const { venue, id } = ref(req.params);
    return sourceFor(venue).getEvent(id);
  }),
);

venueRouter.get(
  '/series',
  asyncRoute(async (req) => ({
    series: await sourceFor(venueParam(req.params)).listSeries(
      typeof req.query['category'] === 'string' ? req.query['category'] : undefined,
    ),
  })),
);

venueRouter.get(
  '/search',
  asyncRoute(async (req) => {
    const venue = venueParam(req.params);
    const query = typeof req.query['q'] === 'string' ? req.query['q'].trim() : '';
    if (!query) {
      throw new UpstreamError('Search needs at least one word', {
        code: 'bad_request',
        hint: 'Usage: SRCH <words>, e.g. `SRCH fed rate cut`.',
      });
    }
    return sourceFor(venue).search(query, intParam(req.query['limit'], 25, 1, 100));
  }),
);

venueRouter.get(
  '/top',
  asyncRoute(async (req) => {
    const venue = venueParam(req.params);
    const raw = typeof req.query['sort'] === 'string' ? req.query['sort'].toLowerCase() : 'volume';
    if (!SORTS.has(raw)) {
      throw new UpstreamError(`Unknown sort "${raw}"`, {
        code: 'bad_request',
        hint: `Valid sorts: ${[...SORTS].join(', ')}.`,
      });
    }
    return {
      venue,
      sort: raw,
      markets: await sourceFor(venue).topMarkets(
        raw as MoverSort,
        intParam(req.query['limit'], 25, 1, 100),
      ),
    };
  }),
);

/** What the snapshot behind `SRCH` and `TOP` currently holds. */
venueRouter.get(
  '/catalogue',
  asyncRoute(async (req) => {
    const venue = venueParam(req.params);
    const snapshot = await sourceFor(venue).corpusSnapshot();
    return {
      venue,
      events: snapshot.events.length,
      markets: snapshot.markets.length,
      truncated: snapshot.truncated,
      ageSeconds: Math.round((Date.now() - snapshot.builtAt) / 1000),
    };
  }),
);
