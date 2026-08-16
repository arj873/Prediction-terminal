import { Router } from 'express';
import type { CandleInterval } from '../../shared/types.js';
import { UpstreamError } from '../lib/http.js';
import * as kalshi from '../sources/kalshi.js';
import { VALID_INTERVALS, asyncRoute, intParam, pathParam } from './helpers.js';

export const kalshiRouter: Router = Router();


kalshiRouter.get(
  '/markets',
  asyncRoute(async (req) => {
    const q = req.query;
    return kalshi.listMarkets({
      limit: intParam(q['limit'], 100, 1, 1000),
      cursor: typeof q['cursor'] === 'string' ? q['cursor'] : undefined,
      status: typeof q['status'] === 'string' ? q['status'] : undefined,
      eventTicker: typeof q['event_ticker'] === 'string' ? q['event_ticker'] : undefined,
      seriesTicker: typeof q['series_ticker'] === 'string' ? q['series_ticker'] : undefined,
      tickers: typeof q['tickers'] === 'string' ? q['tickers'] : undefined,
    });
  }),
);

kalshiRouter.get(
  '/markets/:ticker',
  asyncRoute(async (req) => kalshi.getMarket(pathParam(req.params, 'ticker'))),
);

kalshiRouter.get(
  '/markets/:ticker/orderbook',
  asyncRoute(async (req) =>
    kalshi.getOrderBook(pathParam(req.params, 'ticker'), intParam(req.query['depth'], 12, 1, 100)),
  ),
);

kalshiRouter.get(
  '/markets/:ticker/trades',
  asyncRoute(async (req) =>
    kalshi.getTrades(
      pathParam(req.params, 'ticker'),
      intParam(req.query['limit'], 50, 1, 1000),
      typeof req.query['cursor'] === 'string' ? req.query['cursor'] : undefined,
    ),
  ),
);

kalshiRouter.get(
  '/markets/:ticker/candles',
  asyncRoute(async (req) => {
    const interval = intParam(req.query['interval'], 60, 1, 1440);
    if (!VALID_INTERVALS.has(interval)) {
      throw new UpstreamError(`Unsupported candle interval "${interval}"`, {
        code: 'bad_request',
        hint: 'Kalshi supports 1 (1m), 60 (1h) and 1440 (1d) minute candles only.',
      });
    }

    const now = Math.floor(Date.now() / 1000);
    // Default window: enough buckets to fill a chart without hammering upstream.
    const defaultSpan = interval === 1 ? 6 * 3600 : interval === 60 ? 30 * 86400 : 365 * 86400;
    const endTs = intParam(req.query['end'], now, 0, now + 86400);
    const startTs = intParam(req.query['start'], endTs - defaultSpan, 0, endTs);

    if (startTs >= endTs) {
      throw new UpstreamError('Candle window start must be before end', { code: 'bad_request' });
    }

    return kalshi.getCandles(pathParam(req.params, 'ticker'), interval as CandleInterval, startTs, endTs);
  }),
);

kalshiRouter.get(
  '/events',
  asyncRoute(async (req) =>
    kalshi.listEvents({
      limit: intParam(req.query['limit'], 100, 1, 200),
      cursor: typeof req.query['cursor'] === 'string' ? req.query['cursor'] : undefined,
      status: typeof req.query['status'] === 'string' ? req.query['status'] : undefined,
      seriesTicker:
        typeof req.query['series_ticker'] === 'string' ? req.query['series_ticker'] : undefined,
      withNestedMarkets: req.query['nested'] !== 'false',
    }),
  ),
);

kalshiRouter.get(
  '/events/:eventTicker',
  asyncRoute(async (req) => kalshi.getEvent(pathParam(req.params, 'eventTicker'))),
);

kalshiRouter.get(
  '/series',
  asyncRoute(async (req) => ({
    series: await kalshi.listSeries(
      typeof req.query['category'] === 'string' ? req.query['category'] : undefined,
    ),
  })),
);

/**
 * Full-text search across open events.
 *
 * Kalshi exposes no public search endpoint, so this ranks against a cached
 * snapshot of open events. See `sources/kalshi.ts` for why the snapshot is
 * built from events rather than markets.
 */
kalshiRouter.get(
  '/search',
  asyncRoute(async (req) => {
    const query = typeof req.query['q'] === 'string' ? req.query['q'].trim() : '';
    if (!query) {
      throw new UpstreamError('Search needs at least one word', {
        code: 'bad_request',
        hint: 'Usage: SRCH <words>, e.g. `SRCH fed rate cut`.',
      });
    }
    return kalshi.search(query, intParam(req.query['limit'], 25, 1, 100));
  }),
);

const SORTS = new Set(['volume', 'gainers', 'losers', 'open_interest', 'liquidity']);

/** Leaderboards: most traded, biggest movers, deepest books. */
kalshiRouter.get(
  '/top',
  asyncRoute(async (req) => {
    const raw = typeof req.query['sort'] === 'string' ? req.query['sort'].toLowerCase() : 'volume';
    if (!SORTS.has(raw)) {
      throw new UpstreamError(`Unknown sort "${raw}"`, {
        code: 'bad_request',
        hint: `Valid sorts: ${[...SORTS].join(', ')}.`,
      });
    }
    return {
      sort: raw,
      markets: await kalshi.topMarkets(raw as kalshi.MoverSort, intParam(req.query['limit'], 25, 1, 100)),
    };
  }),
);
