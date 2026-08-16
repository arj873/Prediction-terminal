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
import { asyncRoute, intParam, strParam } from './helpers.js';

export const entertainmentRouter: Router = Router();

/* ------------------------------------------------- kalshi entertainment book */

entertainmentRouter.get(
  '/markets',
  asyncRoute(async (req) =>
    entertainment.browse(
      entertainment.assertGenre(strParam(req.query['genre'])),
      intParam(req.query['limit'], 60, 1, 250),
    ),
  ),
);

/* ------------------------------------------------------------ rotten tomatoes */

entertainmentRouter.get(
  '/rt/search',
  asyncRoute(async (req) => rt.search(strParam(req.query['q']), intParam(req.query['limit'], 20, 1, 50))),
);

entertainmentRouter.get(
  '/rt',
  asyncRoute(async (req) => rt.getTitle(strParam(req.query['q']))),
);

/* -------------------------------------------------------------------- netflix */

entertainmentRouter.get(
  '/netflix',
  asyncRoute(async (req) =>
    netflix.getTop10(
      netflix.assertCategory(strParam(req.query['category'], 'tv') || 'tv'),
      netflix.assertScope(strParam(req.query['scope'])),
    ),
  ),
);

/* --------------------------------------------------------- spotify / youtube */

entertainmentRouter.get('/charts', (req, res) => {
  const source = strParam(req.query['source']).toLowerCase();
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
      streamcharts.resolveSpotifyChart(strParam(req.query['scope']), strParam(req.query['period'])),
      intParam(req.query['limit'], 200, 1, 500),
    ),
  ),
);

entertainmentRouter.get(
  '/youtube',
  asyncRoute(async (req) =>
    streamcharts.getChart(
      streamcharts.resolveYouTubeChart(strParam(req.query['view'])),
      intParam(req.query['limit'], 200, 1, 500),
    ),
  ),
);

/* ----------------------------------------------------------------- box office */

entertainmentRouter.get(
  '/boxoffice',
  asyncRoute(async (req) => {
    const date = strParam(req.query['date']);
    return boxoffice.getDaily(date === '' ? undefined : date);
  }),
);

/* ---------------------------------------------------------------------- steam */

entertainmentRouter.get(
  '/steam',
  asyncRoute(async (req) => {
    const query = strParam(req.query['q']);
    return query === ''
      ? steam.getTop(intParam(req.query['limit'], 25, 1, 100))
      : steam.getGame(query);
  }),
);

/* ------------------------------------------------------------------ tv guide */

entertainmentRouter.get(
  '/tv',
  asyncRoute(async (req) => {
    const date = strParam(req.query['date']);
    const country = strParam(req.query['country']);
    return tvmaze.getSchedule(date === '' ? undefined : date, country === '' ? undefined : country);
  }),
);
