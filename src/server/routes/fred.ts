import { Router } from 'express';
import { UpstreamError } from '../lib/http.js';
import * as fred from '../sources/fred.js';
import { asyncRoute, intParam, pathParam } from './helpers.js';

export const fredRouter: Router = Router();

const DATE = /^\d{4}-\d{2}-\d{2}$/;

function dateParam(raw: unknown, label: string): string | undefined {
  if (typeof raw !== 'string' || raw === '') return undefined;
  if (!DATE.test(raw)) {
    throw new UpstreamError(`"${raw}" is not a valid ${label} date`, {
      code: 'bad_request',
      hint: 'Dates are YYYY-MM-DD, e.g. `FRED UNRATE 2015-01-01`.',
    });
  }
  return raw;
}

fredRouter.get(
  '/search',
  asyncRoute(async (req) => {
    const q = typeof req.query['q'] === 'string' ? req.query['q'] : '';
    return fred.searchSeries(q, intParam(req.query['limit'], 25, 1, 100));
  }),
);

fredRouter.get(
  '/series/:id',
  asyncRoute(async (req) =>
    fred.getSeries(
      pathParam(req.params, 'id'),
      dateParam(req.query['start'], 'start'),
      dateParam(req.query['end'], 'end'),
    ),
  ),
);
