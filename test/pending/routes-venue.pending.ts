/**
 * Suspected defects in the venue route layer and what it ranks with.
 *
 * These are NOT in the suite glob (`test/*.test.ts`): every case below asserts
 * the behaviour the code says it has, and every one currently fails against the
 * behaviour it actually has. Each was observed by running it against the same
 * local `node:http` fixture host `test/routes-venue.test.ts` uses.
 *
 *   1. `rankMarkets` sorts every `TOP` board backwards. `direction` is applied
 *      to a comparator that already encodes the direction, so the two cancel:
 *      `TOP volume` returns the *quietest* contracts of the whole catalogue,
 *      `TOP gainers` returns the biggest fallers, and `TOP losers` the biggest
 *      risers. Because `limit` slices after the sort, the markets a trader
 *      asked for are not merely out of order — they are not in the response.
 *   2. The candle interval is clamped into [1, 1440] *before* it is checked
 *      against the three buckets the terminal charts, so the "Unsupported
 *      candle interval" guard can never fire for a value outside that range.
 *      `?interval=99999` is silently served as daily bars and `?interval=0` as
 *      1-minute bars, instead of the 400 that `?interval=15` correctly gets.
 *   3. `intParam` returns its `fallback` without clamping it, so a route that
 *      computes a default from another parameter can hand the upstream a value
 *      outside the very range it passed in — a negative unix timestamp, here.
 *   4. `strParam` was extracted specifically to stop a repeated query key from
 *      being silently dropped, and `routes/venue.ts` and `routes/kalshi.ts`
 *      still use the hand-inlined ternary it replaced.
 */

import assert from 'node:assert/strict';
import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { after, before, describe, it } from 'node:test';
import { intParam } from '../../src/server/routes/helpers.js';
import { rankMarkets } from '../../src/server/sources/corpus.js';
import type { Market } from '../../src/shared/types.js';

/* ------------------------------------------------------------- fixtures */

/** Trimmed from a real `/trade-api/v2/events?with_nested_markets=true` page. */
const CORPUS_EVENTS = [
  {
    event_ticker: 'KXFEDDECISION-26SEP',
    series_ticker: 'KXFEDDECISION',
    title: 'Fed decision in September?',
    category: 'Economics',
    markets: [
      {
        ticker: 'KXFEDDECISION-26SEP-T3.75',
        event_ticker: 'KXFEDDECISION-26SEP',
        title: 'Fed decision in September?',
        last_price_dollars: '0.6900',
        previous_price_dollars: '0.6400',
        volume_24h_fp: '1204.00',
        open_interest_fp: '12645.98',
        liquidity_dollars: '28801.55',
      },
      {
        ticker: 'KXFEDDECISION-26SEP-T4.00',
        event_ticker: 'KXFEDDECISION-26SEP',
        title: 'Fed decision in September?',
        last_price_dollars: '0.2200',
        previous_price_dollars: '0.3100',
        volume_24h_fp: '880.00',
        open_interest_fp: '4210.00',
        liquidity_dollars: '9110.00',
      },
    ],
  },
  {
    event_ticker: 'KXHIGHNY-26AUG17',
    series_ticker: 'KXHIGHNY',
    title: 'Highest temperature in NYC on 26AUG17?',
    category: 'Climate',
    markets: [
      {
        // The whale of this snapshot on every figure.
        ticker: 'KXHIGHNY-26AUG17-B82',
        event_ticker: 'KXHIGHNY-26AUG17',
        title: 'Highest temperature in NYC on 26AUG17?',
        last_price_dollars: '0.4400',
        previous_price_dollars: '0.4300',
        volume_24h_fp: '98000.00',
        open_interest_fp: '250000.00',
        liquidity_dollars: '410000.00',
      },
      {
        // …and the dust: one lot traded all day.
        ticker: 'KXHIGHNY-26AUG17-B99',
        event_ticker: 'KXHIGHNY-26AUG17',
        title: 'Highest temperature in NYC on 26AUG17?',
        last_price_dollars: '0.0100',
        previous_price_dollars: '0.0100',
        volume_24h_fp: '1.00',
        open_interest_fp: '2.00',
        liquidity_dollars: '3.00',
      },
    ],
  },
];

const KALSHI_MARKET = {
  ticker: 'KXHIGHNY-26AUG17-B82',
  event_ticker: 'KXHIGHNY-26AUG17',
  title: 'Highest temperature in NYC on 26AUG17?',
  status: 'active',
  yes_bid_dollars: '0.4300',
  yes_ask_dollars: '0.4500',
  last_price_dollars: '0.4400',
  volume_fp: '412000.00',
};

let fixture: Server;
let requests: string[] = [];
let server: Server;
let base: string;

function mark(): number {
  return requests.length;
}
function since(at: number): string[] {
  return requests.slice(at);
}

async function get(path: string): Promise<{ status: number; body: any }> {
  const res = await fetch(`${base}${path}`);
  return { status: res.status, body: (await res.json()) as unknown };
}

before(async () => {
  fixture = createServer((req, res) => {
    const url = new URL(req.url ?? '/', 'http://fixture');
    requests.push(`${url.pathname}${url.search}`);
    const p = url.pathname;
    const json = (body: unknown): void => {
      res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(body));
    };

    if (p === '/kalshi/events') return json({ events: CORPUS_EVENTS, cursor: '' });
    if (p === '/kalshi/markets') return json({ markets: [KALSHI_MARKET], cursor: 'NEXTPAGE' });
    if (/\/candlesticks$/.test(p)) return json({ candlesticks: [] });
    if (/^\/kalshi\/markets\/[^/]+$/.test(p)) {
      const ticker = decodeURIComponent(p.slice('/kalshi/markets/'.length));
      return json({ market: { ...KALSHI_MARKET, ticker, event_ticker: ticker } });
    }
    res.writeHead(404, { 'content-type': 'application/json' }).end('{}');
  });

  await new Promise<void>((resolve) => fixture.listen(0, '127.0.0.1', resolve));
  const port = (fixture.address() as AddressInfo).port;
  process.env['KALSHI_API_BASE'] = `http://127.0.0.1:${port}/kalshi`;
  process.env['POLYMARKET_GAMMA_BASE'] = `http://127.0.0.1:${port}/pm-gamma`;
  process.env['POLYMARKET_US_API_BASE'] = `http://127.0.0.1:${port}/pmus`;

  const { createApp } = await import('../../src/server/index.js');
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

/* ------------------------------------------------------ 1. inverted boards */

const market = (ticker: string, volume24h: number, change: number): Market =>
  ({ ticker, volume24h, change, openInterest: volume24h, liquidity: volume24h }) as Market;

describe('rankMarkets direction', () => {
  const markets = [
    market('KXHIGHNY-26AUG17-B82', 98000, 0.01),
    market('KXFEDDECISION-26SEP-T4.00', 880, -0.09),
    market('KXFEDDECISION-26SEP-T3.75', 1204, 0.05),
  ];

  it('puts the most traded market at the top of the volume board', () => {
    // Observed order: ['KXFEDDECISION-26SEP-T4.00', 'KXFEDDECISION-26SEP-T3.75',
    // 'KXHIGHNY-26AUG17-B82'] — 880, 1204, 98000. `direction` is -1 for volume
    // and the comparator is already `field(b) - field(a)`, which is descending
    // on its own, so multiplying by -1 turns it ascending.
    assert.deepEqual(
      rankMarkets(markets, 'volume', 3).map((m) => m.ticker),
      ['KXHIGHNY-26AUG17-B82', 'KXFEDDECISION-26SEP-T3.75', 'KXFEDDECISION-26SEP-T4.00'],
    );
  });

  it('puts the biggest riser at the top of the gainers board', () => {
    // Observed: -0.09 first, i.e. the day's biggest *faller* heads `TOP gainers`.
    assert.equal(rankMarkets(markets, 'gainers', 3)[0]?.change, 0.05);
  });

  it('puts the biggest faller at the top of the losers board', () => {
    // Observed: +0.05 first.
    assert.equal(rankMarkets(markets, 'losers', 3)[0]?.change, -0.09);
  });

  it('ranks open interest and liquidity deepest-first too', () => {
    // Observed for both: 880, 1204, 98000 — thinnest first.
    assert.equal(rankMarkets(markets, 'open_interest', 3)[0]?.openInterest, 98000);
    assert.equal(rankMarkets(markets, 'liquidity', 3)[0]?.liquidity, 98000);
  });

  it('returns the busiest markets from TOP, not the quietest', async () => {
    // The consequence at the route, and the reason `limit` makes it worse than
    // a display-order bug: the board is sliced after the sort, so a one-row
    // `TOP` answers with the single deadest contract in the catalogue.
    // Observed body: markets[0].ticker === 'KXHIGHNY-26AUG17-B99', volume24h 1.
    const { body } = await get('/api/venue/kalshi/top?sort=volume&limit=1');
    assert.equal(body.markets[0].ticker, 'KXHIGHNY-26AUG17-B82');
    assert.equal(body.markets[0].volume24h, 98000);
  });

  it('returns the busiest markets from the legacy /api/kalshi/top as well', async () => {
    // Observed: 'KXHIGHNY-26AUG17-B99'.
    const { body } = await get('/api/kalshi/top?sort=volume&limit=1');
    assert.equal(body.markets[0].ticker, 'KXHIGHNY-26AUG17-B82');
  });
});

/* -------------------------------------------- 2. clamp before validation */

describe('the candle interval guard', () => {
  it('refuses an interval above the largest bucket instead of serving daily bars', async () => {
    // Observed: 200, and `{"interval":1440}` with a
    // `period_interval=1440` request on the wire. `?interval=15` — a smaller,
    // no-less-wrong value — correctly gets a 400, so the guard is plainly meant
    // to fire here too; `intParam(raw, 60, 1, 1440)` clamps 99999 to 1440 first.
    const { status, body } = await get('/api/venue/kalshi/markets/KXCLAMP-A/candles?interval=99999');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
  });

  it('refuses interval=0 instead of serving 1-minute bars', async () => {
    // Observed: 200 with `{"interval":1}` — a request for no bucket at all
    // comes back as the finest one the terminal has.
    const { status } = await get('/api/venue/kalshi/markets/KXCLAMP-B/candles?interval=0');
    assert.equal(status, 400);
  });

  it('refuses a negative interval instead of serving 1-minute bars', async () => {
    // Observed: 200 with `{"interval":1}`.
    const { status } = await get('/api/venue/kalshi/markets/KXCLAMP-C/candles?interval=-60');
    assert.equal(status, 400);
  });

  it('refuses the same intervals on the legacy /api/kalshi router', async () => {
    // Observed: 200 for both. The two routers share the bug because they share
    // the copy-pasted block `helpers.ts` was extracted around.
    assert.equal((await get('/api/kalshi/markets/KXCLAMP-D/candles?interval=99999')).status, 400);
    assert.equal((await get('/api/kalshi/markets/KXCLAMP-E/candles?interval=0')).status, 400);
  });
});

/* ------------------------------------------- 3. the fallback is not clamped */

describe('intParam clamping', () => {
  it('clamps the fallback into the range it was given, as "clamped" implies', () => {
    // Observed: -5 and 500. Every other path through the function returns a
    // value inside [min, max]; the `raw === undefined` early return does not.
    assert.equal(intParam(undefined, -5, 0, 100), 0);
    assert.equal(intParam('', 500, 0, 100), 100);
  });

  it('never asks Kalshi for a negative unix timestamp', async () => {
    // `?end=0` is inside the route's own declared range for `end` (min 0), and
    // the start default is then `endTs - defaultSpan`, returned unclamped
    // despite `min` being 0. Observed on the wire:
    //   /kalshi/series/KXWINDOW/markets/KXWINDOW-NEG/candlesticks
    //     ?start_ts=-2592000&end_ts=0&period_interval=60
    // The `startTs >= endTs` guard passes precisely because start went negative,
    // so the request is forwarded rather than refused.
    const at = mark();
    await get('/api/venue/kalshi/markets/KXWINDOW-NEG/candles?end=0');
    const call = since(at).find((u) => u.includes('candlesticks')) ?? '';
    const startTs = Number(new URLSearchParams(call.slice(call.indexOf('?'))).get('start_ts'));
    assert.ok(startTs >= 0, `start_ts was ${startTs}`);
  });
});

/* --------------------------------------- 4. the routes never call strParam */

describe('repeated query keys', () => {
  it('searches on the first q when the key is repeated', async () => {
    // `helpers.strParam` exists for exactly this and documents it: "the
    // hand-inlined ternaries elsewhere did not handle the array case at all".
    // `routes/venue.ts` and `routes/kalshi.ts` still carry those ternaries, so
    // `req.query.q` is `['fed','rate']`, `typeof … === 'string'` is false, and
    // the query becomes ''.
    // Observed: 400 {"error":"Search needs at least one word", …}.
    const { status, body } = await get('/api/venue/kalshi/search?q=fed&q=rate');
    assert.equal(status, 200);
    assert.equal(body.query, 'fed');
  });

  it('keeps the pagination cursor when the key is repeated', async () => {
    // The silent variant, and the damaging one: the filter is dropped rather
    // than reported, so the caller is served page 1 again under a 200 and a
    // paging loop never terminates.
    // Observed on the wire: '/kalshi/markets?limit=100' — no cursor at all.
    const at = mark();
    await get('/api/kalshi/markets?cursor=NEXTPAGE&cursor=NEXTPAGE');
    assert.deepEqual(since(at), ['/kalshi/markets?limit=100&cursor=NEXTPAGE']);
  });

  it('keeps the status filter when the key is repeated', async () => {
    // Observed: '/kalshi/markets?limit=7' — the caller asked for open markets
    // and was given every market the upstream lists, settled ones included.
    const at = mark();
    await get('/api/kalshi/markets?limit=7&status=open&status=open');
    assert.deepEqual(since(at), ['/kalshi/markets?limit=7&status=open']);
  });
});
