/**
 * `/api/xv/…` — the cross-venue views.
 *
 * `series` answers "who else lists this?"; `compare` answers "at what price?".
 */

import { Router } from 'express';
import { VENUES, isVenue } from '../../shared/venue.js';
import { UpstreamError } from '../lib/http.js';
import { compare, linkedSeries } from '../sources/crossvenue.js';
import { asyncRoute, intParam } from './helpers.js';

export const crossVenueRouter: Router = Router();

crossVenueRouter.get(
  '/series',
  asyncRoute(async (req) =>
    linkedSeries(
      typeof req.query['q'] === 'string' ? req.query['q'].trim() : '',
      intParam(req.query['limit'], 40, 1, 200),
    ),
  ),
);

crossVenueRouter.get(
  '/compare',
  asyncRoute(async (req) => {
    const event = typeof req.query['event'] === 'string' ? req.query['event'].trim() : '';
    if (!event) {
      throw new UpstreamError('Comparison needs an event', {
        code: 'bad_request',
        hint: 'Usage: `XV <event-ticker>`, e.g. `XV KXFEDDECISION-26OCT`.',
      });
    }

    const raw = typeof req.query['venue'] === 'string' ? req.query['venue'] : '';
    if (raw && !isVenue(raw)) {
      throw new UpstreamError(`Unknown venue "${raw}"`, {
        code: 'bad_request',
        hint: `Venues are: ${VENUES.map((v) => v.id).join(', ')}.`,
      });
    }

    return compare(event, isVenue(raw) ? raw : undefined);
  }),
);
