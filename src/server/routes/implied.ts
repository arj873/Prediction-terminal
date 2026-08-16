/**
 * Implied-price routes.
 *
 * Two endpoints, matching the two questions the panel asks: *which ladders
 * price this symbol* (the picker), and *what did one of them imply over time*
 * (the overlay line).
 */

import { Router } from 'express';
import type { CandleInterval, ImpliedMethod } from '../../shared/types.js';
import { UpstreamError } from '../lib/http.js';
import { getCandidates, getImpliedSeries, listUnderlyings } from '../sources/implied.js';
import { asyncRoute, intParam } from './helpers.js';

export const impliedRouter: Router = Router();

const VALID_INTERVALS = new Set<number>([1, 60, 1440]);

/** Every symbol with a mapped ladder — powers `HELP IMP` and symbol validation. */
impliedRouter.get(
  '/underlyings',
  asyncRoute(async () => ({
    underlyings: listUnderlyings().map((u) => ({
      symbol: u.symbol,
      name: u.name,
      assetClass: u.assetClass,
      aliases: u.aliases ?? [],
    })),
  })),
);

impliedRouter.get(
  '/candidates',
  asyncRoute(async (req) => {
    const symbol = typeof req.query['symbol'] === 'string' ? req.query['symbol'].trim() : '';
    if (!symbol) {
      throw new UpstreamError('Which underlying?', {
        code: 'bad_request',
        hint: 'Usage: `IMP <symbol>`, e.g. `IMP BTC`.',
      });
    }
    return getCandidates(symbol);
  }),
);

impliedRouter.get(
  '/series',
  asyncRoute(async (req) => {
    const eventTicker =
      typeof req.query['event'] === 'string' ? req.query['event'].trim().toUpperCase() : '';
    if (!eventTicker) {
      throw new UpstreamError('Which Kalshi event?', {
        code: 'bad_request',
        hint: 'Pass `event=<event-ticker>`, e.g. `event=KXBTCD-26AUG1617`.',
      });
    }

    const interval = intParam(req.query['interval'], 60, 1, 1440);
    if (!VALID_INTERVALS.has(interval)) {
      throw new UpstreamError(`Unsupported candle interval "${interval}"`, {
        code: 'bad_request',
        hint: 'Kalshi supports 1 (1m), 60 (1h) and 1440 (1d) minute candles only.',
      });
    }

    const rawMethod = typeof req.query['method'] === 'string' ? req.query['method'] : 'median';
    if (rawMethod !== 'median' && rawMethod !== 'mean') {
      throw new UpstreamError(`Unknown method "${rawMethod}"`, {
        code: 'bad_request',
        hint:
          '`median` reads the 50% crossing and needs no assumption beyond the quoted ' +
          'strikes. `mean` weights the whole distribution but has to guess at the tails.',
      });
    }

    // Snap the window to a 15-second grid. Two panels polling the same overlay
    // a second apart would otherwise ask for two windows that differ only in
    // their last second, miss the cache, and each re-run the whole strike
    // fan-out. Nothing is lost: no candle bucket is shorter than a minute.
    const GRID = 15;
    const now = Math.floor(Date.now() / 1000 / GRID) * GRID;
    const defaultSpan = interval === 1 ? 6 * 3600 : interval === 60 ? 30 * 86400 : 365 * 86400;
    const requestedEnd = intParam(req.query['end'], now, 0, now + 86400);
    const endTs = Math.floor(requestedEnd / GRID) * GRID;
    const startTs =
      Math.floor(intParam(req.query['start'], endTs - defaultSpan, 0, endTs) / GRID) * GRID;

    if (startTs >= endTs) {
      throw new UpstreamError('Window start must be before end', { code: 'bad_request' });
    }

    return getImpliedSeries(
      eventTicker,
      interval as CandleInterval,
      startTs,
      endTs,
      rawMethod as ImpliedMethod,
    );
  }),
);
