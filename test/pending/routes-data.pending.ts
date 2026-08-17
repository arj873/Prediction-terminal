/**
 * Suspected defects in the data routes. These FAIL against the current
 * `src/server/routes/*` and `src/server/sources/stocks.ts`, and are parked
 * outside the `test/*.test.ts` glob.
 *
 * Two of the three are the same root cause, and the codebase already names it:
 * `routes/helpers.ts` introduced `strParam` because "the hand-inlined ternaries
 * elsewhere did not handle the array case at all, so `?q=a&q=b` reached the
 * source layer as `''` and silently lost the query rather than erroring".
 * `routes/entertainment.ts` and `routes/news.ts` were converted; `crossvenue.ts`
 * and `fred.ts` were not, and both still drop a repeated parameter on the floor
 * and answer 200 to a question they did not actually ask.
 *
 * The third is the cache guarantee the whole server is built around. `TTL.quote`
 * is documented as "short, but long enough to absorb a panel refresh burst", and
 * `routes/spot.ts` quantises the candle window to a 15-second grid precisely so
 * that "a new cache key on every poll" cannot happen. The equity *quote* path
 * builds its Yahoo URL from an unquantised `Date.now()`, so it has exactly the
 * defect the candles route was fixed for: every poll in a new second is a new
 * cache key and a fresh upstream request.
 */

import assert from 'node:assert/strict';
import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { after, before, describe, it } from 'node:test';

/* ------------------------------------------------------------- fixtures */

/** Trimmed `query1.finance.yahoo.com/v8/finance/chart/AAPL?interval=1d`. */
const YAHOO_CHART = {
  chart: {
    result: [
      {
        meta: {
          currency: 'USD',
          symbol: 'AAPL',
          fullExchangeName: 'NasdaqGS',
          regularMarketPrice: 305.93,
          chartPreviousClose: 313.33,
          regularMarketTime: 1786737601,
          longName: 'Apple Inc.',
        },
        timestamp: [1786627800, 1786714200],
        indicators: {
          quote: [
            {
              open: [304.21, 306],
              high: [306, 307.49],
              low: [302.05, 304.3],
              close: [305.26, 305.93],
              volume: [40349300, 28186700],
            },
          ],
        },
      },
    ],
    error: null,
  },
};

const FRED_CSV = `observation_date,UNRATE
2024-01-01,3.7
2024-02-01,3.9
`;

const FRED_SERIES_HTML = `<!doctype html><html><body>
<h1 id="page-title">Unemployment Rate <span>(UNRATE)</span></h1>
<p><span class="series-meta-label">Units:</span> <span class="series-meta-value">Percent, Seasonally Adjusted</span></p>
<div id="notes-container">Fixture notes.</div>
</body></html>`;

const KALSHI_FED_EVENT = {
  event_ticker: 'KXFEDDECISION-26OCT',
  series_ticker: 'KXFEDDECISION',
  title: 'Fed decision in October 2026?',
  sub_title: 'FOMC',
  category: 'Economics',
  markets: [
    {
      ticker: 'KXFEDDECISION-26OCT-C25',
      event_ticker: 'KXFEDDECISION-26OCT',
      yes_sub_title: '25 bps decrease',
      yes_bid_dollars: '0.6300',
      yes_ask_dollars: '0.6500',
      volume_24h_fp: '184320.00',
      close_time: '2026-10-28T18:00:00Z',
    },
  ],
};

const KALSHI_OSCARS_EVENT = {
  event_ticker: 'KXOSCARPIC-26',
  series_ticker: 'KXOSCARPIC',
  title: 'Academy Award for Best Picture 2026?',
  sub_title: 'Oscars',
  category: 'Entertainment',
  markets: [
    {
      ticker: 'KXOSCARPIC-26-DUNE',
      event_ticker: 'KXOSCARPIC-26',
      yes_sub_title: 'Dune: Part Three',
      yes_bid_dollars: '0.2100',
      yes_ask_dollars: '0.2400',
      volume_24h_fp: '9000.00',
      close_time: '2027-03-14T04:00:00Z',
    },
  ],
};

const PMUS_FED_EVENT = {
  ticker: 'usfed-fomc-2026-10-28',
  slug: 'usfed-fomc-2026-10-28',
  seriesSlug: 'usfed-fomc',
  title: 'Fed decision in October 2026?',
  category: 'macro',
  markets: [
    {
      slug: 'usfed-fomc-2026-10-28-c25',
      title: '25 bps decrease',
      status: 'MARKET_STATUS_OPEN',
      bestBidQuote: { value: '0.6600' },
      bestAskQuote: { value: '0.6800' },
    },
  ],
};

const PMUS_OSCARS_EVENT = {
  ticker: 'oscars-2026',
  slug: 'oscars-2026',
  seriesSlug: 'oscars-2026',
  title: 'Academy Award for Best Picture 2026?',
  category: 'culture',
  markets: [
    {
      slug: 'oscars-2026-dune',
      title: 'Dune: Part Three',
      status: 'MARKET_STATUS_OPEN',
      bestBidQuote: { value: '0.2200' },
      bestAskQuote: { value: '0.2500' },
    },
  ],
};

/* --------------------------------------------------------------- harness */

let upstream: Server;
let apiServer: Server;
let apiBase = '';
let seen: string[] = [];
let cache: (typeof import('../../src/server/lib/cache.js'))['cache'];

async function get(path: string): Promise<{ status: number; body: any }> {
  const response = await fetch(`${apiBase}${path}`);
  const text = await response.text();
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    body = text;
  }
  return { status: response.status, body };
}

before(async () => {
  upstream = createServer((req, res) => {
    const url = new URL(req.url ?? '/', 'http://127.0.0.1');
    const path = url.pathname;
    seen.push(`${path}${url.search}`);

    const json = (body: unknown): void => {
      res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(body));
    };

    if (path.startsWith('/yahoo/v8/finance/chart/')) return json(YAHOO_CHART);
    if (path === '/fred/graph/fredgraph.csv') {
      res.writeHead(200, { 'content-type': 'text/csv' }).end(FRED_CSV);
      return;
    }
    if (path.startsWith('/fred/series/')) {
      res.writeHead(200, { 'content-type': 'text/html' }).end(FRED_SERIES_HTML);
      return;
    }
    if (path === '/kalshi/events') {
      return json({ events: [KALSHI_FED_EVENT, KALSHI_OSCARS_EVENT], cursor: '' });
    }
    if (path === '/pmgamma/events') return json([]);
    if (path === '/pmus/v1/events') {
      const category = url.searchParams.get('categories');
      if (Number(url.searchParams.get('offset') ?? '0') > 0) return json({ events: [] });
      if (!category) return json({ events: [PMUS_FED_EVENT, PMUS_OSCARS_EVENT] });
      if (category === 'macro') return json({ events: [PMUS_FED_EVENT] });
      if (category === 'culture') return json({ events: [PMUS_OSCARS_EVENT] });
      return json({ events: [] });
    }

    res.writeHead(404, { 'content-type': 'text/plain' }).end('no fixture');
  });

  await new Promise<void>((resolve) => upstream.listen(0, '127.0.0.1', resolve));
  const fixture = `http://127.0.0.1:${(upstream.address() as AddressInfo).port}`;

  process.env['YAHOO_API_BASE'] = `${fixture}/yahoo`;
  process.env['NASDAQ_API_BASE'] = `${fixture}/nasdaq`;
  process.env['COINBASE_API_BASE'] = `${fixture}/coinbase`;
  process.env['KALSHI_API_BASE'] = `${fixture}/kalshi`;
  process.env['POLYMARKET_GAMMA_BASE'] = `${fixture}/pmgamma`;
  process.env['POLYMARKET_US_API_BASE'] = `${fixture}/pmus`;
  process.env['FRED_WEB_BASE'] = `${fixture}/fred`;
  delete process.env['FRED_API_KEY'];

  const { createApp } = await import('../../src/server/index.js');
  ({ cache } = await import('../../src/server/lib/cache.js'));

  apiServer = createApp().listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => apiServer.once('listening', () => resolve()));
  apiBase = `http://127.0.0.1:${(apiServer.address() as AddressInfo).port}`;
});

after(() => {
  apiServer?.close();
  upstream?.close();
});

/* ====================================================================== */

describe('GET /api/xv/series with a repeated q', () => {
  it('filters on a query sent twice instead of ignoring it', async () => {
    // A repeated key is what a client sends when it appends a filter to a URL
    // that already carries one — `?q=fed` + `&q=fed`. Every other route in this
    // file reads such a parameter through `strParam`, which takes the first
    // value; `crossvenue.ts` still uses the hand-inlined ternary, so the whole
    // board comes back as though nothing had been asked for.
    cache.clear();
    const { status, body } = await get('/api/xv/series?q=oscars&q=oscars');
    assert.equal(status, 200);

    // ACTUAL: query is '' and `series` carries both the Oscars board and the
    // Fed board — the filter was silently discarded.
    assert.equal(body.query, 'oscars');
    assert.deepEqual(
      body.series.map((s: { key: string }) => s.key),
      ['oscars-2026'],
    );
  });
});

describe('GET /api/fred/series/:id with a repeated date', () => {
  it('sends the requested window upstream instead of dropping it', async () => {
    // `dateParam` returns `undefined` for anything that is not a string, and a
    // repeated key arrives as an array. So the window vanishes: no `cosd`, no
    // `coed`, and FRED answers with the *entire* series while the route reports
    // 200 as though the window had been honoured. A caller charting 2020 gets
    // 1948 onward and is never told.
    cache.clear();
    seen = [];
    const { status } = await get(
      '/api/fred/series/UNRATE?start=2020-01-01&start=2020-06-01&end=2021-01-01',
    );
    assert.equal(status, 200);

    const csv = seen.find((r) => r.startsWith('/fred/graph/fredgraph.csv'));
    assert.ok(csv, 'expected a fredgraph.csv request');
    // ACTUAL: '/fred/graph/fredgraph.csv?id=UNRATE&coed=2021-01-01' — `cosd` is
    // absent entirely.
    assert.match(csv, /cosd=2020-01-01/);
  });
});

describe('GET /api/spot/:class/:symbol quote caching', () => {
  it('serves a second equity quote inside TTL.quote from cache', async () => {
    // `TTL.quote` is 3s and documented as "long enough to absorb a panel refresh
    // burst". `stocks.getQuote` builds its Yahoo URL from a bare
    // `Math.floor(Date.now() / 1000)`, so `period1`/`period2` — and therefore
    // the cache key — advance every second. Four panels quoting AAPL on their
    // own timers are four upstream calls per tick, which is the exact failure
    // `routes/spot.ts` snaps the candle window to a 15-second grid to prevent.
    cache.clear();
    seen = [];
    await get('/api/spot/stock/AAPL');
    assert.equal(seen.filter((r) => r.includes('/yahoo/v8/finance/chart/')).length, 1);

    await new Promise((resolve) => setTimeout(resolve, 1_200));
    seen = [];
    await get('/api/spot/stock/AAPL');

    // ACTUAL: one more request, with `period1`/`period2` one second later —
    // e.g. period1=1786321736 then period1=1786321737.
    assert.deepEqual(seen, []);
  });

  it('does not vary the upstream URL by the wall clock', async () => {
    cache.clear();
    seen = [];
    await get('/api/spot/stock/AAPL');
    await new Promise((resolve) => setTimeout(resolve, 1_200));
    await get('/api/spot/stock/AAPL');

    const charts = seen.filter((r) => r.includes('/yahoo/v8/finance/chart/'));
    const windows = new Set(charts.map((r) => r.slice(r.indexOf('?'))));
    // ACTUAL: 2 — the window moves with `Date.now()`, so no two polls share a key.
    assert.equal(windows.size, 1, `asked for ${[...windows].join(' and ')}`);
  });
});
