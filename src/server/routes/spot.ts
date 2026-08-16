/**
 * Spot price routes — the true price half of the implied-vs-actual chart.
 *
 * `:class` picks the provider family rather than describing the instrument, so
 * the same two routes serve equities, ETFs, cash indices and crypto pairs.
 */

import { Router } from 'express';
import type { AssetClass, CandleInterval, SpotSearchResult } from '../../shared/types.js';
import { UpstreamError } from '../lib/http.js';
import * as crypto from '../sources/crypto.js';
import { findUnderlying } from '../sources/implied.js';
import * as stocks from '../sources/stocks.js';
import { asyncRoute, intParam, pathParam } from './helpers.js';

export const spotRouter: Router = Router();

const VALID_INTERVALS = new Set<number>([1, 60, 1440]);

function assetClass(raw: string): AssetClass {
  const value = raw.toLowerCase();
  if (value === 'stock' || value === 'stocks' || value === 'equity') return 'stock';
  if (value === 'crypto' || value === 'coin') return 'crypto';
  throw new UpstreamError(`Unknown asset class "${raw}"`, {
    code: 'bad_request',
    hint: 'Use `stock` (equities, ETFs, indices) or `crypto`.',
  });
}

/**
 * Expand a registry alias before hitting an upstream.
 *
 * Somebody types `SPX`; Yahoo knows it as `^GSPC`. Resolving here means the
 * alias table is stated once, next to the Kalshi ladders it also names.
 */
function resolveSymbol(symbol: string, klass: AssetClass): string {
  const underlying = findUnderlying(symbol);
  if (underlying && underlying.assetClass === klass) return underlying.spotSymbol;
  return symbol;
}

spotRouter.get(
  '/search',
  asyncRoute(async (req) => {
    const query = typeof req.query['q'] === 'string' ? req.query['q'].trim() : '';
    if (!query) {
      throw new UpstreamError('Search needs a symbol or a name', {
        code: 'bad_request',
        hint: 'Usage: `SSRCH <words>`, e.g. `SSRCH apple`.',
      });
    }
    const limit = intParam(req.query['limit'], 20, 1, 50);
    const wanted = typeof req.query['class'] === 'string' ? assetClass(req.query['class']) : null;

    const [equities, coins] = await Promise.all([
      wanted === 'crypto' ? [] : stocks.search(query, limit).catch(() => []),
      wanted === 'stock' ? [] : crypto.search(query, limit).catch(() => []),
    ]);

    const results: SpotSearchResult[] = [
      ...coins.map((product) => ({
        symbol: product.base_currency ?? product.id.split('-')[0] ?? product.id,
        name: product.display_name ?? product.id,
        assetClass: 'crypto' as const,
        venue: 'Coinbase',
        hasImplied: false,
      })),
      ...equities,
    ].map((result) => ({
      ...result,
      // Flag the rows that can carry an implied overlay, so the search result
      // answers "and can I chart the market's forecast of it?" in one pass.
      hasImplied: findUnderlying(result.symbol) !== undefined,
    }));

    return { query, results: results.slice(0, limit) };
  }),
);

spotRouter.get(
  '/:class/:symbol',
  asyncRoute(async (req) => {
    const klass = assetClass(pathParam(req.params, 'class'));
    const symbol = resolveSymbol(pathParam(req.params, 'symbol'), klass);
    return klass === 'crypto' ? crypto.getQuote(symbol) : stocks.getQuote(symbol);
  }),
);

spotRouter.get(
  '/:class/:symbol/candles',
  asyncRoute(async (req) => {
    const klass = assetClass(pathParam(req.params, 'class'));
    const symbol = resolveSymbol(pathParam(req.params, 'symbol'), klass);

    const interval = intParam(req.query['interval'], 60, 1, 1440);
    if (!VALID_INTERVALS.has(interval)) {
      throw new UpstreamError(`Unsupported candle interval "${interval}"`, {
        code: 'bad_request',
        hint:
          'Intervals are 1 (1m), 60 (1h) and 1440 (1d) minutes — the same buckets ' +
          'Kalshi uses, so an implied overlay lines up with the price.',
      });
    }

    const now = Math.floor(Date.now() / 1000);
    const defaultSpan = interval === 1 ? 6 * 3600 : interval === 60 ? 30 * 86400 : 365 * 86400;
    const endTs = intParam(req.query['end'], now, 0, now + 86400);
    const startTs = intParam(req.query['start'], endTs - defaultSpan, 0, endTs);

    if (startTs >= endTs) {
      throw new UpstreamError('Candle window start must be before end', { code: 'bad_request' });
    }

    return klass === 'crypto'
      ? crypto.getCandles(symbol, interval as CandleInterval, startTs, endTs)
      : stocks.getCandles(symbol, interval as CandleInterval, startTs, endTs);
  }),
);
