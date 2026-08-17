/**
 * The venue route layer, end to end.
 *
 * `routes/helpers.ts` was extracted from three copies of the same hand-inlined
 * coercions, so the cases below pin down what the shared helpers actually do at
 * every boundary a caller can reach: a missing parameter, an empty one, a
 * repeated one (Express hands those over as an array), a non-numeric one, and
 * one far outside the range the route allows.
 *
 * The routers are driven through a real Express app on port 0 rather than by
 * calling the handlers directly, because half of what is under test lives in
 * the middleware chain: `mergeParams` is what makes `:venue` visible to a
 * sub-router at all, `asyncRoute`'s `.catch(next)` is what turns a rejected
 * upstream promise into a response, and the status a client sees is chosen by
 * the error middleware in `server/index.ts` from `UpstreamError.code`.
 *
 * Every upstream is pointed at one local `node:http` fixture host, so the tests
 * can assert *which* broker a request was dispatched to and with which
 * arguments — the venue router's one real job — from the URLs that host was
 * asked for. The Kalshi payloads are trimmed copies of real
 * `api.elections.kalshi.com` responses, fixed-point decimal strings and all.
 */

import assert from 'node:assert/strict';
import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { after, before, describe, it } from 'node:test';
import {
  INTERVAL_HINT,
  VALID_INTERVALS,
  intParam,
  pathParam,
  strParam,
} from '../src/server/routes/helpers.js';

/* ================================================================ helpers */

describe('intParam', () => {
  it('reads a decimal query string as a number', () => {
    assert.equal(intParam('25', 50, 1, 100), 25);
    assert.equal(intParam('1', 50, 1, 100), 1);
  });

  it('falls back when the parameter was not sent at all', () => {
    assert.equal(intParam(undefined, 50, 1, 100), 50);
    assert.equal(intParam(null, 50, 1, 100), 50);
  });

  it('reads `?limit=` — a key with no value — as not sent', () => {
    // Express gives '' for a bare key, and a client that interpolates an unset
    // variable produces exactly that. It means "no preference", not "zero".
    assert.equal(intParam('', 50, 1, 100), 50);
  });

  it('falls back rather than emitting NaN for something that is not a number', () => {
    assert.equal(intParam('abc', 50, 1, 100), 50);
    assert.equal(intParam('1,000', 50, 1, 100), 50);
    assert.equal(intParam('undefined', 50, 1, 100), 50);
    assert.equal(intParam({ deep: 'object' }, 50, 1, 100), 50);
  });

  it('falls back for the infinities, which no clamp would rescue', () => {
    // `Number('1e400')` is Infinity, not a large number; clamping it would hand
    // the upstream the maximum rather than the default the caller expressed.
    assert.equal(intParam('1e400', 50, 1, 100), 50);
    assert.equal(intParam('Infinity', 50, 1, 100), 50);
    assert.equal(intParam('-Infinity', 50, 1, 100), 50);
  });

  it('clamps an absurd limit to the ceiling instead of forwarding it', () => {
    assert.equal(intParam('999999999', 50, 1, 100), 100);
    assert.equal(intParam('9007199254740993', 50, 1, 100), 100);
  });

  it('clamps below the floor, so a zero or negative page size cannot be sent', () => {
    assert.equal(intParam('0', 50, 1, 100), 1);
    assert.equal(intParam('-25', 50, 1, 100), 1);
  });

  it('truncates towards zero rather than rounding', () => {
    assert.equal(intParam('12.9', 50, 1, 100), 12);
    assert.equal(intParam('-0.9', 50, 1, 100), 1);
  });

  it('takes the first value when a key was repeated', () => {
    // `?limit=5&limit=9` arrives as ['5','9'].
    assert.equal(intParam(['5', '9'], 50, 1, 100), 5);
    assert.equal(intParam([], 50, 1, 100), 50);
  });

  it('reads a blank value as zero, which the floor then becomes', () => {
    // Documented because it is asymmetric with '' above rather than because it
    // is desirable: Number(' ') is 0, so a blank value yields the minimum.
    assert.equal(intParam(' ', 50, 1, 100), 1);
    assert.equal(intParam(['', ''], 50, 1, 100), 1);
  });
});

describe('strParam', () => {
  it('trims, because a query string carries the spaces a user typed', () => {
    assert.equal(strParam('  fed rate cut  '), 'fed rate cut');
  });

  it('collapses a repeated key to its first value rather than losing it', () => {
    assert.equal(strParam(['fed', 'rate']), 'fed');
  });

  it('falls back for anything that is not a string', () => {
    assert.equal(strParam(undefined), '');
    assert.equal(strParam(undefined, 'tv'), 'tv');
    assert.equal(strParam({ nested: 'qs' }, 'tv'), 'tv');
    assert.equal(strParam([{ nested: 'qs' }], 'tv'), 'tv');
  });

  it('keeps an empty value empty rather than substituting the fallback', () => {
    // `?category=` is the caller clearing the filter, not omitting it.
    assert.equal(strParam('', 'tv'), '');
    assert.equal(strParam('   ', 'tv'), '');
  });
});

describe('pathParam', () => {
  it('reads an ordinary segment', () => {
    assert.equal(pathParam({ venue: 'kalshi' }, 'venue'), 'kalshi');
  });

  it('collapses the array Express types a wildcard segment as', () => {
    assert.equal(pathParam({ id: ['a', 'b'] }, 'id'), 'a');
  });

  it('answers with an empty string for a segment that is not there', () => {
    assert.equal(pathParam({}, 'venue'), '');
    assert.equal(pathParam({ id: [] }, 'id'), '');
  });
});

describe('VALID_INTERVALS', () => {
  it('is the three buckets the hint text promises', () => {
    assert.deepEqual([...VALID_INTERVALS].sort((a, b) => a - b), [1, 60, 1440]);
    assert.match(INTERVAL_HINT, /1 \(1m\), 60 \(1h\) and 1440 \(1d\)/);
  });
});

/* ================================================================ fixtures */

/** Trimmed from a real `/trade-api/v2/markets/{ticker}` response. */
const FED_MARKETS = [
  {
    ticker: 'KXFEDDECISION-26SEP-T3.75',
    event_ticker: 'KXFEDDECISION-26SEP',
    title: 'Fed decision in September?',
    yes_sub_title: '3.75% or above',
    status: 'active',
    market_type: 'binary',
    yes_bid_dollars: '0.6800',
    yes_ask_dollars: '0.7000',
    no_bid_dollars: '0.3000',
    no_ask_dollars: '0.3200',
    last_price_dollars: '0.6900',
    previous_price_dollars: '0.6400',
    volume_fp: '39942.93',
    volume_24h_fp: '1204.00',
    open_interest_fp: '12645.98',
    liquidity_dollars: '28801.55',
    open_time: '2026-07-31T14:00:00Z',
    close_time: '2026-09-17T18:00:00Z',
    strike_type: 'greater_or_equal',
    floor_strike: 3.75,
  },
  {
    ticker: 'KXFEDDECISION-26SEP-T4.00',
    event_ticker: 'KXFEDDECISION-26SEP',
    title: 'Fed decision in September?',
    yes_sub_title: '4.00% or above',
    status: 'active',
    last_price_dollars: '0.2200',
    previous_price_dollars: '0.3100',
    volume_fp: '18220.00',
    volume_24h_fp: '880.00',
    open_interest_fp: '4210.00',
    liquidity_dollars: '9110.00',
  },
  {
    // No turnover in 24h: a change here is a stale print, so the movers boards
    // must leave it out even though `change` is a number.
    ticker: 'KXFEDDECISION-26SEP-T4.25',
    event_ticker: 'KXFEDDECISION-26SEP',
    title: 'Fed decision in September?',
    yes_sub_title: '4.25% or above',
    status: 'active',
    last_price_dollars: '0.0500',
    previous_price_dollars: '0.9000',
    volume_fp: '120.00',
    volume_24h_fp: '0',
    open_interest_fp: '60.00',
    liquidity_dollars: '30.00',
  },
];

/**
 * Four days of the NYC high-temperature ladder, 30 strikes each.
 *
 * A strike ladder is how Kalshi's catalogue actually gets big, and 120 markets
 * is enough to see the `limit` ceiling bite on `TOP`.
 */
const TEMP_EVENTS = ['26AUG17', '26AUG18', '26AUG19', '26AUG20'].map((day, e) => ({
  event_ticker: `KXHIGHNY-${day}`,
  series_ticker: 'KXHIGHNY',
  title: `Highest temperature in NYC on ${day}?`,
  category: 'Climate',
  mutually_exclusive: true,
  markets: Array.from({ length: 30 }, (_, i) => ({
    ticker: `KXHIGHNY-${day}-B${60 + i}`,
    event_ticker: `KXHIGHNY-${day}`,
    title: `Highest temperature in NYC on ${day}?`,
    yes_sub_title: `${60 + i}° to ${61 + i}°`,
    status: 'active',
    last_price_dollars: '0.1000',
    previous_price_dollars: '0.1000',
    volume_fp: '900.00',
    // Distinct and descending, so a volume board has one right answer.
    volume_24h_fp: String(5000 - (e * 30 + i) * 7),
    open_interest_fp: '400.00',
    liquidity_dollars: '250.00',
    strike_type: 'between',
    floor_strike: 60 + i,
    cap_strike: 61 + i,
  })),
}));

const CORPUS_EVENTS = [
  {
    event_ticker: 'KXFEDDECISION-26SEP',
    series_ticker: 'KXFEDDECISION',
    title: 'Fed decision in September?',
    sub_title: 'Target rate after the September FOMC',
    category: 'Economics',
    mutually_exclusive: true,
    markets: FED_MARKETS,
  },
  ...TEMP_EVENTS,
];

/** Trimmed from a real `/markets/trades` response. */
const KALSHI_TRADES = {
  trades: [
    {
      trade_id: '3f1c8a2e-6b31-4a6d-9f0a-2b7c5d8e1a44',
      ticker: 'KXFEDDECISION-26SEP-T3.75',
      created_time: '2026-08-14T19:58:11.428Z',
      count_fp: '250.00',
      yes_price_dollars: '0.6900',
      taker_side: 'yes',
      is_block_trade: false,
    },
  ],
  cursor: '',
};

/** Trimmed from a real gamma `/markets?slug=` response — a one-element array. */
const GAMMA_MARKET = {
  id: '516710',
  slug: 'will-the-fed-cut-rates-in-september',
  question: 'Will the Fed cut rates in September?',
  conditionId: '0x9f2c',
  outcomes: '["Yes", "No"]',
  outcomePrices: '["0.69", "0.31"]',
  clobTokenIds: '["7145", "2199"]',
  bestBid: 0.68,
  bestAsk: 0.7,
  lastTradePrice: 0.69,
  volumeNum: 8412300.55,
  volume24hr: 214880.12,
  liquidityNum: 331201.4,
  endDate: '2026-09-17T18:00:00Z',
  active: true,
  closed: false,
  acceptingOrders: true,
};

/* ============================================================ fixture host */

let fixture: Server;
let requests: string[] = [];
let server: Server;
let base: string;

/** Upstream paths recorded since the marker returned by {@link mark}. */
function mark(): number {
  return requests.length;
}
function since(at: number): string[] {
  return requests.slice(at);
}

async function get(path: string): Promise<{ status: number; body: any; type: string | null }> {
  const res = await fetch(`${base}${path}`);
  return {
    status: res.status,
    type: res.headers.get('content-type'),
    body: (await res.json()) as unknown,
  };
}

before(async () => {
  fixture = createServer((req, res) => {
    const url = new URL(req.url ?? '/', 'http://fixture');
    requests.push(`${url.pathname}${url.search}`);
    const p = url.pathname;
    const json = (body: unknown): void => {
      res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(body));
    };

    /* ---- Kalshi ---------------------------------------------------- */
    if (p === '/kalshi/markets/trades') return json(KALSHI_TRADES);
    if (p === '/kalshi/events') return json({ events: CORPUS_EVENTS, cursor: '' });
    if (p === '/kalshi/series/') return json({ series: [{ ticker: 'KXHIGHNY', title: 'NYC high' }] });
    if (/^\/kalshi\/markets\/[^/]+\/orderbook$/.test(p)) {
      return json({
        orderbook_fp: {
          yes_dollars: [
            ['0.6600', '1200.00'],
            ['0.6800', '340.50'],
          ],
          no_dollars: [
            ['0.2900', '900.00'],
            ['0.3000', '75.25'],
          ],
        },
      });
    }
    if (/\/candlesticks$/.test(p)) return json({ candlesticks: [] });
    if (/^\/kalshi\/events\/[^/]+$/.test(p)) {
      return json({ event: CORPUS_EVENTS[0] });
    }
    if (/^\/kalshi\/markets\/[^/]+$/.test(p)) {
      const ticker = decodeURIComponent(p.slice('/kalshi/markets/'.length));
      if (ticker === 'KXGONE') {
        return res.writeHead(404, { 'content-type': 'application/json' }).end('{}');
      }
      if (ticker === 'KXBROKEN') {
        return res.writeHead(500, { 'content-type': 'text/plain' }).end('internal error');
      }
      return json({ market: { ...FED_MARKETS[0], ticker, event_ticker: ticker } });
    }

    /* ---- Polymarket International ----------------------------------- */
    if (p === '/pm-gamma/markets') {
      return json([{ ...GAMMA_MARKET, slug: url.searchParams.get('slug') }]);
    }
    if (p === '/pm-gamma/events') return json([]);

    /* ---- Polymarket US ---------------------------------------------- */
    if (p === '/pmus/v1/events') return json({ events: [] });

    res.writeHead(404, { 'content-type': 'application/json' }).end('{}');
  });

  await new Promise<void>((resolve) => fixture.listen(0, '127.0.0.1', resolve));
  const port = (fixture.address() as AddressInfo).port;

  // Set before the source modules load: each reads its base URL at import time.
  process.env['KALSHI_API_BASE'] = `http://127.0.0.1:${port}/kalshi`;
  process.env['POLYMARKET_GAMMA_BASE'] = `http://127.0.0.1:${port}/pm-gamma`;
  process.env['POLYMARKET_CLOB_BASE'] = `http://127.0.0.1:${port}/pm-clob`;
  process.env['POLYMARKET_DATA_BASE'] = `http://127.0.0.1:${port}/pm-data`;
  process.env['POLYMARKET_US_API_BASE'] = `http://127.0.0.1:${port}/pmus`;

  const { createApp } = await import('../src/server/index.js');
  server = createApp().listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => server.once('listening', () => resolve()));
  base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
});

after(() => {
  server.closeAllConnections();
  server.close();
  fixture.closeAllConnections();
  fixture.close();
});

/* ============================================================== dispatch */

describe('venue dispatch', () => {
  it('sends a kalshi reference to the Kalshi API, shouted the way Kalshi answers', async () => {
    // Kalshi 404s a lower-case ticker, so the route folds case before asking.
    const at = mark();
    const { status, body } = await get('/api/venue/kalshi/markets/kxfeddecision-26sep-t3.75');
    assert.equal(status, 200);
    assert.deepEqual(since(at), ['/kalshi/markets/KXFEDDECISION-26SEP-T3.75']);
    assert.equal(body.venue, 'kalshi');
    assert.equal(body.ticker, 'KXFEDDECISION-26SEP-T3.75');
  });

  it('sends a polymarket reference to gamma, lower-cased the way Polymarket answers', async () => {
    const at = mark();
    const { status, body } = await get('/api/venue/polymarket/markets/Will-The-Fed-Cut-Rates');
    assert.equal(status, 200);
    assert.deepEqual(since(at), ['/pm-gamma/markets?slug=will-the-fed-cut-rates']);
    assert.equal(body.venue, 'polymarket');
    assert.equal(body.ticker, 'will-the-fed-cut-rates');
  });

  it('sends a polymarket-us reference to the US gateway', async () => {
    const at = mark();
    const { status } = await get('/api/venue/polymarket-us/catalogue');
    assert.equal(status, 200);
    const paths = since(at);
    assert.ok(paths.length > 0);
    assert.ok(paths.every((u) => u.startsWith('/pmus/v1/events')), paths.join(' '));
  });

  it('answers 501 for a venue that publishes no tape, without asking it', async () => {
    // The venue is reachable and the request is well formed; it simply has no
    // public trades endpoint. A 502 would blame the network for a fact.
    const at = mark();
    const { status, body } = await get('/api/venue/polymarket-us/markets/tec-mlb-nlchamp-lad/trades');
    assert.equal(status, 501);
    assert.equal(body.code, 'unsupported');
    assert.deepEqual(since(at), []);
    assert.match(body.hint, /public endpoints/);
  });

  it('answers 501 for a venue that publishes no price history', async () => {
    const at = mark();
    const { status, body } = await get('/api/venue/polymarket-us/markets/tec-mlb-nlchamp-lad/candles');
    assert.equal(status, 501);
    assert.equal(body.code, 'unsupported');
    assert.deepEqual(since(at), []);
  });

  it('routes the orderbook, trades, event and series verbs to the same venue', async () => {
    const at = mark();
    await get('/api/venue/kalshi/markets/KXDISPATCH-A/orderbook');
    await get('/api/venue/kalshi/markets/KXDISPATCH-A/trades');
    await get('/api/venue/kalshi/events/kxfeddecision-26sep');
    await get('/api/venue/kalshi/series');
    assert.deepEqual(since(at), [
      '/kalshi/markets/KXDISPATCH-A/orderbook?depth=12',
      '/kalshi/markets/trades?ticker=KXDISPATCH-A&limit=50',
      '/kalshi/events/KXFEDDECISION-26SEP?with_nested_markets=true',
      '/kalshi/series/',
    ]);
  });

  it('passes the series category filter through', async () => {
    const at = mark();
    await get('/api/venue/kalshi/series?category=Economics');
    assert.deepEqual(since(at), ['/kalshi/series/?category=Economics']);
  });
});

/* ======================================================= venue validation */

describe('the venue segment', () => {
  it('rejects a name no broker answers to, and says which do', async () => {
    const at = mark();
    const { status, body } = await get('/api/venue/nasdaq/markets/AAPL');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
    assert.equal(body.error, 'Unknown venue "nasdaq"');
    assert.equal(body.hint, 'Venues are: kalshi, polymarket, polymarket-us.');
    // The point of validating here: nothing was asked of any upstream.
    assert.deepEqual(since(at), []);
  });

  it('rejects a command-line alias, which is not the on-the-wire id', async () => {
    // `pm:` is how a trader types it; `/api/venue/polymarket` is the route.
    const { status, body } = await get('/api/venue/pm/markets/some-slug');
    assert.equal(status, 400);
    assert.equal(body.error, 'Unknown venue "pm"');
  });

  it('rejects the right id in the wrong case rather than guessing', async () => {
    const { status, body } = await get('/api/venue/Kalshi/markets/KXA');
    assert.equal(status, 400);
    assert.equal(body.error, 'Unknown venue "Kalshi"');
  });

  it('is validated on every verb, not just the market lookup', async () => {
    for (const path of ['/search?q=fed', '/top', '/series', '/catalogue', '/events/x']) {
      const { status, body } = await get(`/api/venue/nasdaq${path}`);
      assert.equal(status, 400, path);
      assert.equal(body.code, 'bad_request', path);
    }
  });
});

/* =========================================================== candle window */

describe('the candle window', () => {
  it('charts the three intervals the terminal supports', async () => {
    for (const interval of [1, 60, 1440]) {
      const at = mark();
      const { status, body } = await get(
        `/api/venue/kalshi/markets/KXWINDOW-${interval}/candles?interval=${interval}`,
      );
      assert.equal(status, 200);
      assert.equal(body.interval, interval);
      assert.ok(
        since(at).some((u) => u.includes(`period_interval=${interval}`)),
        since(at).join(' '),
      );
    }
  });

  it('refuses an interval it has no buckets for', async () => {
    const { status, body } = await get('/api/venue/kalshi/markets/KXA/candles?interval=15');
    assert.equal(status, 400);
    assert.equal(body.error, 'Unsupported candle interval "15"');
    assert.match(body.hint, /1 \(1m\), 60 \(1h\) and 1440 \(1d\)/);
  });

  it('defaults to hourly when no interval is asked for', async () => {
    const at = mark();
    const { body } = await get('/api/venue/kalshi/markets/KXWINDOW-DEFAULT/candles');
    assert.equal(body.interval, 60);
    assert.ok(since(at).some((u) => u.includes('period_interval=60')));
  });

  it('sizes the default window to the bucket, so a chart is never one bar', async () => {
    // 6h of minutes, 30d of hours, 365d of days.
    const spans: Record<number, number> = { 1: 6 * 3600, 60: 30 * 86400, 1440: 365 * 86400 };
    for (const [interval, span] of Object.entries(spans)) {
      const at = mark();
      await get(`/api/venue/kalshi/markets/KXSPAN-${interval}/candles?interval=${interval}`);
      const call = since(at).find((u) => u.includes('candlesticks'));
      assert.ok(call, `no candlesticks call for ${interval}`);
      const q = new URLSearchParams(call.slice(call.indexOf('?')));
      assert.equal(Number(q.get('end_ts')) - Number(q.get('start_ts')), span);
    }
  });

  it('honours an explicit window verbatim', async () => {
    const at = mark();
    await get('/api/venue/kalshi/markets/KXEXPLICIT/candles?interval=60&start=1786600000&end=1786700000');
    const call = since(at).find((u) => u.includes('candlesticks'));
    assert.ok(call);
    assert.match(call, /start_ts=1786600000/);
    assert.match(call, /end_ts=1786700000/);
  });

  it('rejects a window that ends where it starts', async () => {
    const at = mark();
    const { status, body } = await get(
      '/api/venue/kalshi/markets/KXA/candles?start=1786700000&end=1786700000',
    );
    assert.equal(status, 400);
    assert.equal(body.error, 'Candle window start must be before end');
    assert.deepEqual(since(at), []);
  });

  it('rejects a window that runs backwards, having clamped start to end', async () => {
    const { status, body } = await get(
      '/api/venue/kalshi/markets/KXA/candles?start=1786800000&end=1786700000',
    );
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
  });

  it('caps an end far in the future at a day out, rather than asking for it', async () => {
    const at = mark();
    await get('/api/venue/kalshi/markets/KXFUTURE/candles?interval=1440&end=99999999999');
    const call = since(at).find((u) => u.includes('candlesticks'));
    assert.ok(call);
    const endTs = Number(new URLSearchParams(call.slice(call.indexOf('?'))).get('end_ts'));
    const ceiling = Math.floor(Date.now() / 1000) + 86400;
    assert.ok(endTs <= ceiling && endTs > ceiling - 60, `end_ts ${endTs} vs ceiling ${ceiling}`);
  });

  it('looks the market up to learn its series, because the prefix is only a guess', async () => {
    // `KXMVECROSSCATEGORY-SHARD1` style series contain a hyphen, so the path
    // segment cannot be derived from the ticker; the candlesticks endpoint 404s
    // on a wrong guess.
    const at = mark();
    await get('/api/venue/kalshi/markets/KXSERIES-LOOKUP-A/candles?interval=1440');
    const calls = since(at);
    assert.equal(calls[0], '/kalshi/markets/KXSERIES-LOOKUP-A');
    assert.match(calls[1] ?? '', /^\/kalshi\/series\/KXSERIES\/markets\/KXSERIES-LOOKUP-A\/candlesticks/);
  });
});

/* ===================================================== query-param coercion */

describe('limit, depth and their edges over the wire', () => {
  it('defaults the book depth, and forwards an explicit one', async () => {
    const at = mark();
    await get('/api/venue/kalshi/markets/KXDEPTH-A/orderbook');
    await get('/api/venue/kalshi/markets/KXDEPTH-A/orderbook?depth=3');
    assert.deepEqual(since(at), [
      '/kalshi/markets/KXDEPTH-A/orderbook?depth=12',
      '/kalshi/markets/KXDEPTH-A/orderbook?depth=3',
    ]);
  });

  it('clamps a book depth beyond the ceiling instead of forwarding it', async () => {
    const at = mark();
    await get('/api/venue/kalshi/markets/KXDEPTH-B/orderbook?depth=100000');
    assert.deepEqual(since(at), ['/kalshi/markets/KXDEPTH-B/orderbook?depth=100']);
  });

  it('actually truncates the book to the depth asked for', async () => {
    const { body } = await get('/api/venue/kalshi/markets/KXDEPTH-C/orderbook?depth=1');
    assert.equal(body.yes.length, 1);
    assert.equal(body.yes[0].price, 0.68);
    assert.equal(body.yesAsks.length, 1);
    // A NO bid at 0.30 is an offer to sell YES at 0.70.
    assert.equal(body.yesAsks[0].price, 0.7);
  });

  it('reads a missing, empty and non-numeric trade limit all as the default', async () => {
    const at = mark();
    await get('/api/venue/kalshi/markets/KXLIM-A/trades');
    await get('/api/venue/kalshi/markets/KXLIM-B/trades?limit=');
    await get('/api/venue/kalshi/markets/KXLIM-C/trades?limit=all');
    assert.deepEqual(since(at), [
      '/kalshi/markets/trades?ticker=KXLIM-A&limit=50',
      '/kalshi/markets/trades?ticker=KXLIM-B&limit=50',
      '/kalshi/markets/trades?ticker=KXLIM-C&limit=50',
    ]);
  });

  it('clamps a trade limit to the range the upstream accepts', async () => {
    const at = mark();
    await get('/api/venue/kalshi/markets/KXLIM-D/trades?limit=999999');
    await get('/api/venue/kalshi/markets/KXLIM-E/trades?limit=0');
    await get('/api/venue/kalshi/markets/KXLIM-F/trades?limit=12.9');
    assert.deepEqual(since(at), [
      '/kalshi/markets/trades?ticker=KXLIM-D&limit=1000',
      '/kalshi/markets/trades?ticker=KXLIM-E&limit=1',
      '/kalshi/markets/trades?ticker=KXLIM-F&limit=12',
    ]);
  });

  it('normalises the trade tape it gets back', async () => {
    const { body } = await get('/api/venue/kalshi/markets/KXFEDDECISION-26SEP-T3.75/trades');
    assert.equal(body.trades.length, 1);
    assert.equal(body.trades[0].venue, 'kalshi');
    assert.equal(body.trades[0].yesPrice, 0.69);
    // Only one side is sent; the pair sums to 1.
    assert.equal(body.trades[0].noPrice, 0.31);
    assert.equal(body.trades[0].count, 250);
    assert.equal(body.cursor, null);
  });
});

/* ======================================================== search and top */

describe('search', () => {
  it('ranks the cached catalogue against the words given', async () => {
    const { status, body } = await get('/api/venue/kalshi/search?q=fed%20decision');
    assert.equal(status, 200);
    assert.equal(body.query, 'fed decision');
    assert.equal(body.hits[0].event.eventTicker, 'KXFEDDECISION-26SEP');
    assert.equal(body.scanned, CORPUS_EVENTS.length);
    assert.equal(body.truncated, false);
  });

  it('trims the query before deciding it is empty', async () => {
    const { status, body } = await get('/api/venue/kalshi/search?q=%20%20temperature%20%20');
    assert.equal(status, 200);
    assert.equal(body.query, 'temperature');
    assert.equal(body.hits.length, 4);
  });

  it('refuses a search with no words rather than scanning everything', async () => {
    for (const q of ['', '%20%20']) {
      const { status, body } = await get(`/api/venue/kalshi/search?q=${q}`);
      assert.equal(status, 400);
      assert.equal(body.error, 'Search needs at least one word');
      assert.match(body.hint, /SRCH fed rate cut/);
    }
  });

  it('refuses a search with no q at all', async () => {
    const { status, body } = await get('/api/venue/kalshi/search');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
  });

  it('returns an empty hit list, not an error, when nothing matches', async () => {
    const { status, body } = await get('/api/venue/kalshi/search?q=zeppelin');
    assert.equal(status, 200);
    assert.deepEqual(body.hits, []);
    assert.equal(body.scanned, CORPUS_EVENTS.length);
  });

  it('honours the result limit and its ceiling', async () => {
    const one = await get('/api/venue/kalshi/search?q=temperature&limit=1');
    assert.equal(one.body.hits.length, 1);
    const clamped = await get('/api/venue/kalshi/search?q=temperature&limit=99999');
    assert.equal(clamped.body.hits.length, 4);
    const floored = await get('/api/venue/kalshi/search?q=temperature&limit=0');
    assert.equal(floored.body.hits.length, 1);
  });
});

describe('top', () => {
  it('ranks on 24h volume by default and names the board it ranked', async () => {
    // Which end of the board comes back is asserted in
    // test/pending/routes-venue.pending.ts — it is currently the wrong one.
    const { status, body } = await get('/api/venue/kalshi/top');
    assert.equal(status, 200);
    assert.equal(body.venue, 'kalshi');
    assert.equal(body.sort, 'volume');
    assert.equal(body.markets.length, 25);
    assert.ok(body.markets.every((m: { volume24h: unknown }) => typeof m.volume24h === 'number'));
  });

  it('accepts the sort in any case a trader types it', async () => {
    const { body } = await get('/api/venue/kalshi/top?sort=GAINERS');
    assert.equal(body.sort, 'gainers');
  });

  it('refuses a sort it cannot rank on, and lists the ones it can', async () => {
    const { status, body } = await get('/api/venue/kalshi/top?sort=sharpe');
    assert.equal(status, 400);
    assert.equal(body.error, 'Unknown sort "sharpe"');
    assert.equal(body.hint, 'Valid sorts: volume, gainers, losers, open_interest, liquidity.');
  });

  it('draws both movers boards from the same pool: everything that traded and moved', async () => {
    const gainers = await get('/api/venue/kalshi/top?sort=gainers&limit=100');
    const losers = await get('/api/venue/kalshi/top?sort=losers&limit=100');
    assert.equal(gainers.body.markets.length, 100);
    assert.equal(losers.body.markets.length, 100);
    for (const board of [gainers, losers]) {
      assert.ok(board.body.markets.every((m: { change: unknown }) => typeof m.change === 'number'));
      assert.ok(board.body.markets.every((m: { volume24h: number }) => m.volume24h > 0));
    }
  });

  it('leaves a market with no 24h turnover off both movers boards', async () => {
    // -85c on no volume is a stale print, not a move.
    for (const sort of ['gainers', 'losers']) {
      const { body } = await get(`/api/venue/kalshi/top?sort=${sort}&limit=100`);
      assert.ok(
        !body.markets.some((m: { ticker: string }) => m.ticker === 'KXFEDDECISION-26SEP-T4.25'),
        sort,
      );
    }
  });

  it('clamps an absurd limit to the ceiling rather than returning the catalogue', async () => {
    const { body } = await get('/api/venue/kalshi/top?limit=1000000');
    assert.equal(body.markets.length, 100);
  });

  it('reads a non-numeric limit as the default', async () => {
    const { body } = await get('/api/venue/kalshi/top?limit=lots');
    assert.equal(body.markets.length, 25);
  });

  it('clamps a zero limit up to one rather than returning nothing', async () => {
    const { body } = await get('/api/venue/kalshi/top?limit=0');
    assert.equal(body.markets.length, 1);
  });
});

describe('catalogue', () => {
  it('reports what the cached snapshot behind SRCH and TOP actually holds', async () => {
    const { status, body } = await get('/api/venue/kalshi/catalogue');
    assert.equal(status, 200);
    assert.equal(body.venue, 'kalshi');
    assert.equal(body.events, CORPUS_EVENTS.length);
    assert.equal(body.markets, 123);
    assert.equal(body.truncated, false);
    assert.equal(typeof body.ageSeconds, 'number');
    assert.ok(body.ageSeconds >= 0);
  });

  it('serves the snapshot from cache rather than re-crawling per request', async () => {
    const at = mark();
    await get('/api/venue/kalshi/catalogue');
    await get('/api/venue/kalshi/catalogue');
    assert.deepEqual(since(at), []);
  });
});

/* ================================================== error response shape */

describe('the error envelope', () => {
  it('turns an upstream 404 into a 404 carrying the upstream status', async () => {
    const { status, body } = await get('/api/venue/kalshi/markets/KXGONE');
    assert.equal(status, 404);
    assert.equal(body.code, 'not_found');
    assert.equal(body.status, 404);
    assert.match(body.hint, /no such resource/);
  });

  it('turns an upstream outage into a 502, not a 500', async () => {
    const { status, body } = await get('/api/venue/kalshi/markets/KXBROKEN');
    assert.equal(status, 502);
    assert.equal(body.code, 'upstream_status');
    assert.equal(body.status, 500);
  });

  it('answers every error as JSON with an error and a code', async () => {
    const { type, body } = await get('/api/venue/nasdaq/markets/x');
    assert.match(type ?? '', /application\/json/);
    assert.equal(typeof body.error, 'string');
    assert.equal(typeof body.code, 'string');
  });

  it('omits the hint when there is nothing useful to say', async () => {
    const { body } = await get('/api/venue/kalshi/markets/KXA/candles?start=5&end=5');
    assert.equal('hint' in body, false);
  });

  it('404s an unknown path under a known venue as an API route miss', async () => {
    const { status, body } = await get('/api/venue/kalshi/nonsense');
    assert.equal(status, 404);
    assert.equal(body.error, 'No such API route');
    assert.equal(body.code, 'not_found');
  });

  it('404s a market path with no identifier rather than asking for an empty one', async () => {
    const at = mark();
    const { status } = await get('/api/venue/kalshi/markets/');
    assert.equal(status, 404);
    assert.deepEqual(since(at), []);
  });
});

/* ================================================ the legacy kalshi router */

describe('the /api/kalshi router', () => {
  it('shares the helpers with the venue router: same defaults, same clamps', async () => {
    const at = mark();
    await get('/api/kalshi/markets/KXLEGACY-A/orderbook');
    await get('/api/kalshi/markets/KXLEGACY-B/orderbook?depth=100000');
    await get('/api/kalshi/markets/KXLEGACY-C/trades?limit=');
    await get('/api/kalshi/markets/KXLEGACY-D/trades?limit=99999');
    assert.deepEqual(since(at), [
      '/kalshi/markets/KXLEGACY-A/orderbook?depth=12',
      '/kalshi/markets/KXLEGACY-B/orderbook?depth=100',
      '/kalshi/markets/trades?ticker=KXLEGACY-C&limit=50',
      '/kalshi/markets/trades?ticker=KXLEGACY-D&limit=1000',
    ]);
  });

  it('rejects the same unsupported interval with the same status', async () => {
    const { status, body } = await get('/api/kalshi/markets/KXA/candles?interval=15');
    assert.equal(status, 400);
    assert.equal(body.error, 'Unsupported candle interval "15"');
    assert.match(body.hint, /Kalshi supports/);
  });

  it('rejects the same backwards window', async () => {
    const { status, body } = await get('/api/kalshi/markets/KXA/candles?start=200&end=100');
    assert.equal(status, 400);
    assert.equal(body.error, 'Candle window start must be before end');
  });

  it('rejects the same empty search and unknown sort', async () => {
    const empty = await get('/api/kalshi/search?q=%20');
    assert.equal(empty.status, 400);
    assert.equal(empty.body.error, 'Search needs at least one word');

    const sort = await get('/api/kalshi/top?sort=sharpe');
    assert.equal(sort.status, 400);
    assert.equal(sort.body.error, 'Unknown sort "sharpe"');
  });

  it('answers /top without the venue field the venue router adds', async () => {
    const { status, body } = await get('/api/kalshi/top?limit=3');
    assert.equal(status, 200);
    assert.equal(body.sort, 'volume');
    assert.equal(body.markets.length, 3);
    assert.equal('venue' in body, false);
  });

  it('clamps the events limit to 200, a tighter ceiling than markets', async () => {
    const at = mark();
    await get('/api/kalshi/events?limit=99999&status=open');
    assert.deepEqual(since(at), ['/kalshi/events?limit=200&status=open&with_nested_markets=true']);
  });

  it('asks for nested markets unless the caller says not to', async () => {
    const at = mark();
    await get('/api/kalshi/events?limit=5&status=unopened&nested=false');
    assert.deepEqual(since(at), ['/kalshi/events?limit=5&status=unopened']);
  });

  it('passes the market list filters through untouched', async () => {
    const at = mark();
    await get('/api/kalshi/markets?limit=2&status=open&event_ticker=KXFEDDECISION-26SEP&cursor=abc123');
    assert.deepEqual(since(at), [
      '/kalshi/markets?limit=2&cursor=abc123&status=open&event_ticker=KXFEDDECISION-26SEP',
    ]);
  });

  it('takes the ticker from the path exactly as sent, with no case folding', async () => {
    // Unlike the venue router, this legacy path does not normalise the ticker.
    const at = mark();
    await get('/api/kalshi/markets/kxlegacy-case');
    assert.deepEqual(since(at), ['/kalshi/markets/kxlegacy-case']);
  });
});
