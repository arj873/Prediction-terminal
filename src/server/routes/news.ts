/**
 * News routes.
 *
 * One endpoint, kept off `/api/ent` because a market-news wire is not an
 * entertainment feed. Validation lives in the source module next to the
 * normaliser that depends on it, so this is a shell.
 */

import { Router } from 'express';
import * as alpaca from '../sources/alpaca.js';
import { asyncRoute, intParam } from './helpers.js';

export const newsRouter: Router = Router();

/** Read a query parameter as a single trimmed string. */
function str(raw: unknown, fallback = ''): string {
  if (Array.isArray(raw)) return typeof raw[0] === 'string' ? raw[0].trim() : fallback;
  return typeof raw === 'string' ? raw.trim() : fallback;
}

newsRouter.get(
  '/',
  asyncRoute(async (req) =>
    alpaca.getNews(
      alpaca.assertSymbols(str(req.query['symbols'])),
      intParam(req.query['limit'], 30, 1, alpaca.MAX_LIMIT),
      intParam(req.query['days'], alpaca.DEFAULT_DAYS, 1, alpaca.MAX_DAYS),
    ),
  ),
);
