/**
 * Reference-data routes.
 *
 * One router for the twelve publishers the terminal reads, because they are one
 * feature: the numbers and records a prediction market settles against. The
 * series half is uniform and goes through the registry; the document half —
 * Congress, EDGAR, data.gov — keeps its own shapes, because a bill is not an
 * observation and pretending otherwise would lose everything that makes it one.
 *
 * Handlers are thin by design. Validation lives in the source modules, next to
 * the parsers that depend on it.
 */

import { Router } from 'express';
import { UpstreamError } from '../lib/http.js';
import * as cftc from '../sources/cftc.js';
import * as congress from '../sources/congress.js';
import * as datagov from '../sources/datagov.js';
import * as datasources from '../sources/datasources.js';
import * as sec from '../sources/secedgar.js';
import { asyncRoute, intParam, pathParam, strParam } from './helpers.js';

export const dataRouter: Router = Router();

const DATE = /^\d{4}(-\d{2}(-\d{2})?)?$/;

/**
 * Read a start or end bound.
 *
 * Deliberately looser than the FRED route's full `YYYY-MM-DD`: the BLS takes a
 * year, SDMX takes a year or a month, and requiring a day would make a reader
 * invent one — `ECO oecd:… 2015` is a reasonable thing to type and every
 * provider here can act on it.
 */
function boundParam(raw: unknown, label: string): string | undefined {
  const value = strParam(raw);
  if (value === '') return undefined;
  if (!DATE.test(value)) {
    throw new UpstreamError(`"${value}" is not a valid ${label} date`, {
      code: 'bad_request',
      hint: 'Bounds are YYYY, YYYY-MM or YYYY-MM-DD, e.g. `ECO UNRATE 2015-01-01`.',
    });
  }
  return value;
}

/* --------------------------------------------------------------- sources */

dataRouter.get('/sources', (_req, res) => {
  res.json({ sources: datasources.sourceStatuses() });
});

/* ---------------------------------------------------------------- series */

/**
 * The reference travels as a query parameter rather than a path segment.
 *
 * Half of these ids contain slashes — `EXR/D.USD.EUR.SP00.A`,
 * `H15/RIFLGFCY10_N.B`, `legacy/GOLD/noncomm_net` — and a path segment cannot
 * hold one without double-encoding that Express then decodes back into route
 * boundaries. A query parameter carries them as themselves.
 */
dataRouter.get(
  '/series',
  asyncRoute(async (req) =>
    datasources.getSeries(
      strParam(req.query['ref']),
      boundParam(req.query['start'], 'start'),
      boundParam(req.query['end'], 'end'),
    ),
  ),
);

dataRouter.get(
  '/search',
  asyncRoute(async (req) => {
    const sources = strParam(req.query['sources']);
    return datasources.searchSeries(
      strParam(req.query['q']),
      sources === '' ? undefined : datasources.parseSourceList(sources),
      intParam(req.query['limit'], 40, 1, 100),
    );
  }),
);

/* ------------------------------------------------------------------- cftc */

dataRouter.get('/cot/reports', (_req, res) => {
  res.json({ reports: cftc.listReports() });
});

dataRouter.get(
  '/cot/markets',
  asyncRoute(async (req) =>
    cftc.findMarkets(
      strParam(req.query['q']),
      strParam(req.query['report'], 'legacy') || 'legacy',
      intParam(req.query['limit'], 40, 1, 200),
    ),
  ),
);

dataRouter.get(
  '/cot',
  asyncRoute(async (req) => {
    const market = strParam(req.query['market']);
    if (!market) {
      throw new UpstreamError('Missing market', {
        code: 'bad_request',
        hint: 'e.g. `COT gold`, `COT crude oil`, `COT e-mini s&p`.',
      });
    }
    return cftc.getReport(market, strParam(req.query['report'], 'legacy') || 'legacy');
  }),
);

/* --------------------------------------------------------------- congress */

dataRouter.get(
  '/congress/bills',
  asyncRoute(async (req) => {
    const congressNumber = intParam(req.query['congress'], 0, 0, 200);
    return congress.searchBills(
      strParam(req.query['q']),
      congressNumber === 0 ? undefined : congressNumber,
      intParam(req.query['limit'], 40, 1, 250),
    );
  }),
);

dataRouter.get(
  '/congress/bill/:congress/:type/:number',
  asyncRoute(async (req) => {
    const number = pathParam(req.params, 'number');
    const congressNumber = Number(pathParam(req.params, 'congress'));
    if (!Number.isInteger(congressNumber) || congressNumber < 1 || congressNumber > 200) {
      throw new UpstreamError(`"${pathParam(req.params, 'congress')}" is not a Congress number`, {
        code: 'bad_request',
        hint: `The current Congress is the ${congress.currentCongress()}th.`,
      });
    }
    if (!/^\d{1,6}$/.test(number)) {
      throw new UpstreamError(`"${number}" is not a bill number`, { code: 'bad_request' });
    }
    return congress.getBill(congressNumber, pathParam(req.params, 'type'), number);
  }),
);

/* -------------------------------------------------------------- sec edgar */

dataRouter.get(
  '/sec/search',
  asyncRoute(async (req) => ({
    query: strParam(req.query['q']),
    results: await sec.search(strParam(req.query['q']), intParam(req.query['limit'], 25, 1, 100)),
  })),
);

dataRouter.get(
  '/sec/filings',
  asyncRoute(async (req) => {
    const form = strParam(req.query['form']);
    return sec.getFilings(strParam(req.query['q']), {
      limit: intParam(req.query['limit'], 40, 1, 250),
      ...(form === '' ? {} : { form }),
    });
  }),
);

dataRouter.get(
  '/sec/concept',
  asyncRoute(async (req) =>
    sec.getConcept(
      strParam(req.query['q']),
      strParam(req.query['tag'], 'Revenues') || 'Revenues',
      strParam(req.query['taxonomy'], 'us-gaap') || 'us-gaap',
    ),
  ),
);

/* --------------------------------------------------------------- data.gov */

dataRouter.get(
  '/gov',
  asyncRoute(async (req) => {
    const cursor = strParam(req.query['cursor']);
    return datagov.searchDatasets(
      strParam(req.query['q']),
      intParam(req.query['limit'], 30, 1, 100),
      cursor === '' ? undefined : cursor,
    );
  }),
);
