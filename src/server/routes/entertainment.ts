/**
 * Entertainment routes.
 *
 * One router for the whole entertainment surface — the Kalshi market browser
 * and the six data feeds those markets settle against — because they are one
 * feature and share a prefix. Each handler is a thin shell: validation lives in
 * the source modules next to the parsers that depend on it.
 */

import { Router } from 'express';
import * as boxoffice from '../sources/boxoffice.js';
import * as entertainment from '../sources/entertainment.js';
import * as netflix from '../sources/netflix.js';
import * as rt from '../sources/rottentomatoes.js';
import * as steam from '../sources/steam.js';
import * as streamcharts from '../sources/streamcharts.js';
import * as tvmaze from '../sources/tvmaze.js';
import { asyncRoute, intParam } from './helpers.js';

export const entertainmentRouter: Router = Router();

/** Read a query parameter as a single trimmed string. */
function str(raw: unknown, fallback = ''): string {
  if (Array.isArray(raw)) return typeof raw[0] === 'string' ? raw[0].trim() : fallback;
  return typeof raw === 'string' ? raw.trim() : fallback;
}

/* ------------------------------------------------- kalshi entertainment book */

entertainmentRouter.get(
  '/markets',
  asyncRoute(async (req) =>
    entertainment.browse(
      entertainment.assertGenre(str(req.query['genre'])),
      intParam(req.query['limit'], 60, 1, 250),
    ),
  ),
);

/* ------------------------------------------------------------ rotten tomatoes */

entertainmentRouter.get(
  '/rt/search',
  asyncRoute(async (req) => rt.search(str(req.query['q']), intParam(req.query['limit'], 20, 1, 50))),
);

entertainmentRouter.get(
  '/rt',
  asyncRoute(async (req) => rt.getTitle(str(req.query['q']))),
);

/* -------------------------------------------------------------------- netflix */

entertainmentRouter.get(
  '/netflix',
  asyncRoute(async (req) =>
    netflix.getTop10(
      netflix.assertCategory(str(req.query['category'], 'tv') || 'tv'),
      netflix.assertScope(str(req.query['scope'])),
    ),
  ),
);

/* --------------------------------------------------------- spotify / youtube */

entertainmentRouter.get('/charts', (req, res) => {
  const source = str(req.query['source']).toLowerCase();
  res.json({
    charts: streamcharts.listCharts(
      source === 'spotify' || source === 'youtube' ? source : undefined,
    ),
  });
});

entertainmentRouter.get(
  '/spotify',
  asyncRoute(async (req) =>
    streamcharts.getChart(
      streamcharts.resolveSpotifyChart(str(req.query['scope']), str(req.query['period'])),
      intParam(req.query['limit'], 200, 1, 500),
    ),
  ),
);

entertainmentRouter.get(
  '/youtube',
  asyncRoute(async (req) =>
    streamcharts.getChart(
      streamcharts.resolveYouTubeChart(str(req.query['view'])),
      intParam(req.query['limit'], 200, 1, 500),
    ),
  ),
);

/* ----------------------------------------------------------------- box office */

entertainmentRouter.get(
  '/boxoffice',
  asyncRoute(async (req) => {
    const date = str(req.query['date']);
    return boxoffice.getDaily(date === '' ? undefined : date);
  }),
);

/* ---------------------------------------------------------------------- steam */

entertainmentRouter.get(
  '/steam',
  asyncRoute(async (req) => {
    const query = str(req.query['q']);
    return query === ''
      ? steam.getTop(intParam(req.query['limit'], 25, 1, 100))
      : steam.getGame(query);
  }),
);

/* ------------------------------------------------------------------ tv guide */

entertainmentRouter.get(
  '/tv',
  asyncRoute(async (req) => {
    const date = str(req.query['date']);
    const country = str(req.query['country']);
    return tvmaze.getSchedule(date === '' ? undefined : date, country === '' ? undefined : country);
  }),
);
