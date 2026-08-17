/**
 * The data route handlers, driven end to end over HTTP.
 *
 * Every one of these routes is a thin shell whose whole job is the join between
 * an HTTP request and an upstream: which parameters are required, what shape
 * comes back, and — the part a unit test of the source module cannot see — what
 * status code a failure upstream turns into by the time it reaches a panel.
 *
 * So the app is the real `createApp()` on port 0, complete with its error
 * middleware, and every upstream is pointed at one local fixture server via the
 * base-URL environment variables the source modules read at load time. The
 * fixtures are trimmed copies of real payloads (a Yahoo chart, Coinbase's
 * positional candles, Kalshi's `_dollars`/`_fp` decimal strings, Polymarket US's
 * `{value}` money objects, FRED's CSV, Alpaca's news wire) because the routes'
 * failure modes are the upstreams' quirks arriving intact.
 *
 * The cases that matter most here are the fan-outs. `/api/xv/series` crawls
 * three catalogues and `/api/xv/compare` re-quotes every venue live; both are
 * written to survive one source failing, and "one source failed" is the state
 * they will actually spend their life in. Those tests fail a single venue on
 * purpose and assert that the others still print, and that the casualty is
 * *named* rather than silently missing.
 */

import assert from 'node:assert/strict';
import { createServer, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';
import { after, before, describe, it } from 'node:test';

/* ====================================================================== */
/*  fixtures                                                              */
/* ====================================================================== */

/** Trimmed `query1.finance.yahoo.com/v8/finance/chart/AAPL?interval=1d`. */
const YAHOO_CHART = {
  chart: {
    result: [
      {
        meta: {
          currency: 'USD',
          symbol: 'AAPL',
          exchangeName: 'NMS',
          fullExchangeName: 'NasdaqGS',
          instrumentType: 'EQUITY',
          regularMarketPrice: 305.93,
          chartPreviousClose: 313.33,
          regularMarketDayHigh: 307.49,
          regularMarketDayLow: 304.3,
          regularMarketVolume: 26054077,
          regularMarketTime: 1786737601,
          longName: 'Apple Inc.',
          shortName: 'Apple Inc.',
        },
        timestamp: [1786368600, 1786455000, 1786541400, 1786627800, 1786714200],
        indicators: {
          quote: [
            {
              open: [306.83, 307.75, 305.1, 304.21, 306],
              high: [308.26, 309.97, 305.66, 306, 307.49],
              low: [304.61, 302.79, 300.57, 302.05, 304.3],
              close: [308.26, 304.91, 302.25, 305.26, 305.93],
              volume: [44812500, 37476700, 41657800, 40349300, 28186700],
            },
          ],
        },
      },
    ],
    error: null,
  },
};

/** Trimmed `/v1/finance/search?q=apple`. */
const YAHOO_SEARCH = {
  quotes: [
    {
      symbol: 'AAPL',
      shortname: 'Apple Inc.',
      longname: 'Apple Inc.',
      exchDisp: 'NASDAQ',
      quoteType: 'EQUITY',
      isYahooFinance: true,
    },
    { symbol: '^GSPC', shortname: 'S&P 500', exchDisp: 'SNP', quoteType: 'INDEX', isYahooFinance: true },
    // Yahoo mixes news-desk entities into the quote list; they carry
    // `isYahooFinance: false` and are not tradeable symbols.
    { symbol: 'Apple Inc', quoteType: 'EQUITY', isYahooFinance: false },
  ],
};

/** Trimmed `api.nasdaq.com/api/quote/FALLBK/info?assetclass=stocks`. */
const NASDAQ_INFO = {
  data: {
    symbol: 'FALLBK',
    companyName: 'Fallback Industries Inc. Common Stock',
    exchange: 'NASDAQ-GS',
    primaryData: {
      lastSalePrice: '$41.20',
      netChange: '0.35',
      percentageChange: '0.86%',
      volume: '1,204,331',
    },
    keyStats: { PreviousClose: { value: '$40.85' }, OpenPrice: { value: '$40.90' } },
  },
  status: { rCode: 200, bCodeMessage: null },
};

/** Trimmed `api.exchange.coinbase.com/products`. */
const COINBASE_PRODUCTS = [
  {
    id: 'BTC-USD',
    base_currency: 'BTC',
    quote_currency: 'USD',
    display_name: 'BTC/USD',
    status: 'online',
    trading_disabled: false,
  },
  {
    id: 'BTC-USDC',
    base_currency: 'BTC',
    quote_currency: 'USDC',
    display_name: 'BTC/USDC',
    status: 'online',
    trading_disabled: false,
  },
  {
    id: 'ETH-USD',
    base_currency: 'ETH',
    quote_currency: 'USD',
    display_name: 'ETH/USD',
    status: 'online',
    trading_disabled: false,
  },
];

const COINBASE_TICKER = {
  trade_id: 812376123,
  price: '63039.67',
  size: '0.00126',
  bid: '63039.00',
  ask: '63040.15',
  volume: '12345.51234000',
  time: '2026-08-14T20:31:07.482913Z',
};

const COINBASE_STATS = {
  open: '62000.00',
  high: '63500.00',
  low: '61800.00',
  last: '63039.67',
  volume: '12345.51234000',
};

/** `[time, low, high, open, close, volume]`, newest first, as Coinbase serves it. */
const COINBASE_CANDLES: [number, number, number, number, number, number][] = [
  [1786886400, 62800.1, 63600.0, 62950.5, 63039.67, 812.3311],
  [1786800000, 61750.0, 62400.0, 61900.0, 62000.0, 1104.552],
];

/* ---------------------------------------------------------------- kalshi */

const KALSHI_FED_EVENT = {
  event_ticker: 'KXFEDDECISION-26OCT',
  series_ticker: 'KXFEDDECISION',
  title: 'Fed decision in October 2026?',
  sub_title: 'FOMC',
  category: 'Economics',
  mutually_exclusive: true,
  markets: [
    {
      ticker: 'KXFEDDECISION-26OCT-C25',
      event_ticker: 'KXFEDDECISION-26OCT',
      title: 'Fed decision in October 2026?',
      yes_sub_title: '25 bps decrease',
      no_sub_title: 'Not a 25 bps decrease',
      status: 'active',
      yes_bid_dollars: '0.6300',
      yes_ask_dollars: '0.6500',
      last_price_dollars: '0.6400',
      previous_price_dollars: '0.6000',
      volume_24h_fp: '184320.00',
      volume_fp: '921000.00',
      open_interest_fp: '55210.00',
      close_time: '2026-10-28T18:00:00Z',
    },
    {
      ticker: 'KXFEDDECISION-26OCT-NC',
      event_ticker: 'KXFEDDECISION-26OCT',
      title: 'Fed decision in October 2026?',
      yes_sub_title: 'No change',
      no_sub_title: 'Some change',
      status: 'active',
      yes_bid_dollars: '0.3300',
      yes_ask_dollars: '0.3600',
      last_price_dollars: '0.3400',
      volume_24h_fp: '90120.00',
      open_interest_fp: '31000.00',
      close_time: '2026-10-28T18:00:00Z',
    },
  ],
};

const KALSHI_RT_EVENT = {
  event_ticker: 'KXRT-26DUNE3',
  series_ticker: 'KXRT',
  title: 'Dune: Part Three Tomatometer?',
  sub_title: 'On release',
  category: 'Entertainment',
  markets: [
    {
      ticker: 'KXRT-26DUNE3-T85',
      event_ticker: 'KXRT-26DUNE3',
      title: 'Dune: Part Three Tomatometer?',
      yes_sub_title: '85% or above',
      status: 'active',
      yes_bid_dollars: '0.7100',
      yes_ask_dollars: '0.7400',
      last_price_dollars: '0.7200',
      volume_24h_fp: '4210.00',
      open_interest_fp: '9100.00',
      close_time: '2026-12-18T05:00:00Z',
      strike_type: 'greater_or_equal',
      floor_strike: 85,
    },
  ],
};

/** A real strike ladder: `greater_or_equal` rungs over one settlement level. */
const KALSHI_BTC_LADDER = {
  event_ticker: 'KXBTCD-26AUG1617',
  series_ticker: 'KXBTCD',
  title: 'Bitcoin price today at 5pm EDT?',
  sub_title: 'Aug 16, 2026',
  category: 'Crypto',
  mutually_exclusive: false,
  markets: [
    {
      ticker: 'KXBTCD-26AUG1617-T62000',
      event_ticker: 'KXBTCD-26AUG1617',
      yes_sub_title: '$62,000 or above',
      status: 'active',
      yes_bid_dollars: '0.9100',
      yes_ask_dollars: '0.9300',
      last_price_dollars: '0.9200',
      volume_24h_fp: '5200.00',
      open_interest_fp: '18000.00',
      close_time: '2026-08-16T21:00:00Z',
      strike_type: 'greater_or_equal',
      floor_strike: 62000,
    },
    {
      ticker: 'KXBTCD-26AUG1617-T63000',
      event_ticker: 'KXBTCD-26AUG1617',
      yes_sub_title: '$63,000 or above',
      status: 'active',
      yes_bid_dollars: '0.5200',
      yes_ask_dollars: '0.5500',
      last_price_dollars: '0.5300',
      volume_24h_fp: '9100.00',
      open_interest_fp: '22000.00',
      close_time: '2026-08-16T21:00:00Z',
      strike_type: 'greater_or_equal',
      floor_strike: 63000,
    },
    {
      ticker: 'KXBTCD-26AUG1617-T64000',
      event_ticker: 'KXBTCD-26AUG1617',
      yes_sub_title: '$64,000 or above',
      status: 'active',
      yes_bid_dollars: '0.1200',
      yes_ask_dollars: '0.1400',
      last_price_dollars: '0.1300',
      volume_24h_fp: '3300.00',
      open_interest_fp: '12000.00',
      close_time: '2026-08-16T21:00:00Z',
      strike_type: 'greater_or_equal',
      floor_strike: 64000,
    },
    {
      ticker: 'KXBTCD-26AUG1617-T65000',
      event_ticker: 'KXBTCD-26AUG1617',
      yes_sub_title: '$65,000 or above',
      status: 'active',
      yes_bid_dollars: '0.0200',
      yes_ask_dollars: '0.0400',
      last_price_dollars: '0.0300',
      volume_24h_fp: '900.00',
      open_interest_fp: '4000.00',
      close_time: '2026-08-16T21:00:00Z',
      strike_type: 'greater_or_equal',
      floor_strike: 65000,
    },
  ],
};

/** Two contracts with numeric strikes is one short of a ladder. */
const KALSHI_SHORT_LADDER = {
  event_ticker: 'KXBTCD-26AUG1618',
  series_ticker: 'KXBTCD',
  title: 'Bitcoin price today at 6pm EDT?',
  category: 'Crypto',
  markets: KALSHI_BTC_LADDER.markets.slice(0, 2),
};

/** `candlesticks` rows: one traded period, one with only a carried close. */
function kalshiCandles(closeDollars: string): unknown {
  return {
    candlesticks: [
      {
        end_period_ts: 1786800000,
        volume_fp: '120.00',
        open_interest_fp: '4000.00',
        price: {
          open_dollars: closeDollars,
          high_dollars: closeDollars,
          low_dollars: closeDollars,
          close_dollars: closeDollars,
        },
        yes_bid: { close_dollars: closeDollars },
        yes_ask: { close_dollars: closeDollars },
      },
      {
        end_period_ts: 1786886400,
        volume_fp: '0.00',
        open_interest_fp: '4000.00',
        // No prints this period — only the previous close, which is how Kalshi
        // reports a quiet hour on a rung nobody traded.
        price: { previous_dollars: closeDollars },
        yes_bid: { close_dollars: closeDollars },
        yes_ask: { close_dollars: closeDollars },
      },
    ],
  };
}

/* ---------------------------------------------------------- polymarket us */

const PMUS_FED_EVENT = {
  id: 'evt_991',
  ticker: 'usfed-fomc-2026-10-28',
  slug: 'usfed-fomc-2026-10-28',
  seriesSlug: 'usfed-fomc',
  title: 'Fed decision in October 2026?',
  category: 'macro',
  active: true,
  closed: false,
  markets: [
    {
      id: 'mkt_1',
      slug: 'usfed-fomc-2026-10-28-c25',
      title: '25 bps decrease',
      question: 'Will the Fed decrease rates by 25 bps in October 2026?',
      status: 'MARKET_STATUS_OPEN',
      endDate: '2026-10-28T18:00:00Z',
      bestBidQuote: { value: '0.6600' },
      bestAskQuote: { value: '0.6800' },
    },
    {
      id: 'mkt_2',
      slug: 'usfed-fomc-2026-10-28-nc',
      title: 'No change',
      question: 'Will the Fed hold rates in October 2026?',
      status: 'MARKET_STATUS_OPEN',
      endDate: '2026-10-28T18:00:00Z',
      bestBidQuote: { value: '0.3000' },
      bestAskQuote: { value: '0.3200' },
    },
  ],
};

/* ------------------------------------------------------------------ fred */

const FRED_CSV = `observation_date,UNRATE
2024-01-01,3.7
2024-02-01,3.9
2024-03-01,.
2024-04-01,3.9
`;

const FRED_SERIES_HTML = `<!doctype html><html><head>
<title>Unemployment Rate (UNRATE) | FRED | St. Louis Fed</title></head><body>
<h1 id="page-title">Unemployment Rate <span>(UNRATE)</span></h1>
<p><span class="series-meta-label">Units:</span> <span class="series-meta-value">Percent, Seasonally Adjusted</span></p>
<p><span class="series-meta-label">Frequency:</span> <span class="series-meta-value">Monthly</span></p>
<div class="series-obs-range">1948-01-01 to 2024-04-01</div>
<div id="notes-container">Fixture notes.</div>
</body></html>`;

const FRED_SEARCH_HTML = `<html><body>
<div><a href="/series/UNRATE">Unemployment Rate</a><div class="series-meta">Percent, Monthly, Seasonally Adjusted</div></div>
<div><a href="/series/U6RATE">Broader unemployment</a></div>
</body></html>`;

/** FRED answers an unknown id with a page, not a 404. */
const FRED_NOT_FOUND_HTML = `<!doctype html><html><body><h1>Page not found</h1></body></html>`;

/* ---------------------------------------------------------------- alpaca */

const ALPACA_NEWS = {
  news: [
    {
      id: 47374123,
      headline: 'Nvidia Q3 Beat: Data Center Revenue Tops Street&#39;s $30B View',
      author: 'Benzinga Newsdesk',
      created_at: '2026-08-14T20:31:07Z',
      updated_at: '2026-08-14T20:33:41Z',
      summary: '<p>Shares of <b>NVIDIA</b> rose in after-hours trade.</p>',
      url: 'https://www.benzinga.com/news/26/08/47374123/nvidia-q3',
      symbols: ['NVDA'],
      source: 'benzinga',
    },
  ],
  next_page_token: null,
};

/* ====================================================================== */
/*  fixture upstream + app                                                */
/* ====================================================================== */

const ALPACA_KEY = 'PKFIXTUREKEYID';
const ALPACA_SECRET = 'fixturesecret';

let upstream: Server;
let apiServer: Server;
let apiBase = '';
let seen: string[] = [];
let cache: (typeof import('../src/server/lib/cache.js'))['cache'];

/** Flipped per test to fail exactly one venue's catalogue crawl. */
let gammaDown = false;
/** Flipped to make the live re-quote of one partner event fail. */
let pmusEventDown = false;
/** Flipped to take the second arm of the symbol-search fan-out down too. */
let coinbaseDown = false;

function json(res: Parameters<Parameters<typeof createServer>[1]>[1], body: unknown): void {
  res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(body));
}

function html(res: Parameters<Parameters<typeof createServer>[1]>[1], body: string): void {
  res.writeHead(200, { 'content-type': 'text/html' }).end(body);
}

function handle(rawUrl: string, res: Parameters<Parameters<typeof createServer>[1]>[1]): void {
  const url = new URL(rawUrl, 'http://127.0.0.1');
  const path = url.pathname;
  const q = url.searchParams;
  seen.push(`${path}${url.search}`);

  /* ---- yahoo ---- */
  if (path.startsWith('/yahoo/v8/finance/chart/')) {
    const symbol = decodeURIComponent(path.slice('/yahoo/v8/finance/chart/'.length));
    // Yahoo answers a delisted symbol with an empty result and a description.
    if (symbol === 'NOSUCHSYM') {
      json(res, { chart: { result: [], error: { code: 'Not Found', description: 'No data found, symbol may be delisted' } } });
      return;
    }
    // A rate-limited or proxied Yahoo hands back an HTML interstitial with a
    // 200, which is neither an error status nor JSON.
    if (symbol === 'FALLBK' || symbol === 'FAILBOTH') {
      html(res, '<html><body>Too Many Requests</body></html>');
      return;
    }
    json(res, YAHOO_CHART);
    return;
  }
  if (path === '/yahoo/v1/finance/search') {
    if ((q.get('q') ?? '').toLowerCase() === 'btc') {
      res.writeHead(404, { 'content-type': 'text/plain' }).end('not found');
      return;
    }
    json(res, YAHOO_SEARCH);
    return;
  }

  /* ---- nasdaq ---- */
  if (path.startsWith('/nasdaq/api/quote/')) {
    const rest = path.slice('/nasdaq/api/quote/'.length).split('/');
    const symbol = decodeURIComponent(rest[0] ?? '');
    if (symbol === 'FALLBK' && q.get('assetclass') === 'stocks') {
      json(res, NASDAQ_INFO);
      return;
    }
    // Nasdaq reports "we do not file this symbol under that class" in-band,
    // with a 200 and a non-200 rCode.
    json(res, { data: null, status: { rCode: 400, bCodeMessage: [{ errorMessage: 'no data' }] } });
    return;
  }

  /* ---- coinbase ---- */
  if (path === '/coinbase/products') {
    if (coinbaseDown) {
      res.writeHead(404, { 'content-type': 'application/json' }).end(JSON.stringify({ message: 'NotFound' }));
      return;
    }
    json(res, COINBASE_PRODUCTS);
    return;
  }
  if (path.startsWith('/coinbase/products/')) {
    const rest = path.slice('/coinbase/products/'.length).split('/');
    const product = decodeURIComponent(rest[0] ?? '');
    const leaf = rest[1] ?? '';
    if (product !== 'BTC-USD') {
      res.writeHead(404, { 'content-type': 'application/json' }).end(JSON.stringify({ message: 'NotFound' }));
      return;
    }
    if (leaf === 'ticker') return json(res, COINBASE_TICKER);
    if (leaf === 'stats') return json(res, COINBASE_STATS);
    if (leaf === 'candles') return json(res, COINBASE_CANDLES);
  }

  /* ---- kalshi ---- */
  if (path === '/kalshi/events') {
    const series = q.get('series_ticker');
    if (series === 'KXBTCD') return json(res, { events: [KALSHI_BTC_LADDER], cursor: '' });
    if (series) return json(res, { events: [], cursor: '' });
    // The corpus crawl: one page, no cursor.
    return json(res, { events: [KALSHI_FED_EVENT, KALSHI_RT_EVENT], cursor: '' });
  }
  if (path.startsWith('/kalshi/events/')) {
    const ticker = decodeURIComponent(path.slice('/kalshi/events/'.length));
    if (ticker === KALSHI_BTC_LADDER.event_ticker) return json(res, { event: KALSHI_BTC_LADDER });
    if (ticker === KALSHI_SHORT_LADDER.event_ticker) return json(res, { event: KALSHI_SHORT_LADDER });
    if (ticker === KALSHI_FED_EVENT.event_ticker) return json(res, { event: KALSHI_FED_EVENT });
    res.writeHead(404, { 'content-type': 'application/json' }).end(JSON.stringify({ error: 'not found' }));
    return;
  }
  if (path.startsWith('/kalshi/series/') && path.endsWith('/candlesticks')) {
    const ticker = decodeURIComponent(path.split('/markets/')[1]?.replace('/candlesticks', '') ?? '');
    const market = KALSHI_BTC_LADDER.markets.find((m) => m.ticker === ticker);
    // One rung of the ladder publishes no history at all, which is the normal
    // state of a wing strike nobody has traded.
    if (!market || market.ticker.endsWith('T65000')) {
      res.writeHead(404, { 'content-type': 'application/json' }).end(JSON.stringify({ error: 'no candles' }));
      return;
    }
    return json(res, kalshiCandles(market.last_price_dollars));
  }
  if (path.startsWith('/kalshi/markets/')) {
    res.writeHead(404, { 'content-type': 'application/json' }).end(JSON.stringify({ error: 'not found' }));
    return;
  }

  /* ---- polymarket international ---- */
  if (path === '/pmgamma/events') {
    if (gammaDown) {
      res.writeHead(404, { 'content-type': 'application/json' }).end(JSON.stringify({ error: 'gone' }));
      return;
    }
    return json(res, []);
  }

  /* ---- polymarket us ---- */
  if (path === '/pmus/v1/events') {
    const category = q.get('categories');
    const offset = Number(q.get('offset') ?? '0');
    if (offset > 0) return json(res, { events: [] });
    if (!category || category === 'macro') return json(res, { events: [PMUS_FED_EVENT] });
    return json(res, { events: [] });
  }
  if (path.startsWith('/pmus/v1/events/slug/')) {
    if (pmusEventDown) {
      res.writeHead(404, { 'content-type': 'application/json' }).end(JSON.stringify({ error: 'gone' }));
      return;
    }
    return json(res, { event: PMUS_FED_EVENT });
  }

  /* ---- fred ---- */
  if (path === '/fred/graph/fredgraph.csv') {
    if (q.get('id') === 'NOSUCH') return html(res, FRED_NOT_FOUND_HTML);
    res.writeHead(200, { 'content-type': 'text/csv' }).end(FRED_CSV);
    return;
  }
  if (path.startsWith('/fred/series/')) return html(res, FRED_SERIES_HTML);
  if (path === '/fred/searchresults/') return html(res, FRED_SEARCH_HTML);

  /* ---- alpaca ---- */
  if (path === '/alpaca/news') {
    if ((q.get('symbols') ?? '').includes('BADKEY')) {
      res
        .writeHead(401, { 'content-type': 'application/json' })
        .end(JSON.stringify({ message: 'access key verification failed' }));
      return;
    }
    return json(res, ALPACA_NEWS);
  }

  res.writeHead(404, { 'content-type': 'text/plain' }).end('no fixture');
}

interface ApiResult<T = any> {
  status: number;
  body: T;
}

async function get<T = any>(path: string): Promise<ApiResult<T>> {
  const response = await fetch(`${apiBase}${path}`);
  const text = await response.text();
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    body = text;
  }
  return { status: response.status, body: body as T };
}

before(async () => {
  upstream = createServer((req, res) => {
    handle(req.url ?? '/', res);
  });
  await new Promise<void>((resolve) => upstream.listen(0, '127.0.0.1', resolve));
  const fixture = `http://127.0.0.1:${(upstream.address() as AddressInfo).port}`;

  process.env['YAHOO_API_BASE'] = `${fixture}/yahoo`;
  process.env['NASDAQ_API_BASE'] = `${fixture}/nasdaq`;
  process.env['COINBASE_API_BASE'] = `${fixture}/coinbase`;
  process.env['KALSHI_API_BASE'] = `${fixture}/kalshi`;
  process.env['POLYMARKET_GAMMA_BASE'] = `${fixture}/pmgamma`;
  process.env['POLYMARKET_CLOB_BASE'] = `${fixture}/pmclob`;
  process.env['POLYMARKET_DATA_BASE'] = `${fixture}/pmdata`;
  process.env['POLYMARKET_US_API_BASE'] = `${fixture}/pmus`;
  process.env['FRED_WEB_BASE'] = `${fixture}/fred`;
  process.env['ALPACA_DATA_BASE'] = `${fixture}/alpaca`;
  process.env['ALPACA_API_KEY_ID'] = ALPACA_KEY;
  process.env['ALPACA_API_SECRET_KEY'] = ALPACA_SECRET;
  delete process.env['FRED_API_KEY'];

  // Imported only now: every source module reads its base URL at load time.
  const { createApp } = await import('../src/server/index.js');
  ({ cache } = await import('../src/server/lib/cache.js'));

  apiServer = createApp().listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => apiServer.once('listening', () => resolve()));
  apiBase = `http://127.0.0.1:${(apiServer.address() as AddressInfo).port}`;
});

after(() => {
  apiServer?.close();
  upstream?.close();
});

/* ====================================================================== */
/*  /api/spot                                                             */
/* ====================================================================== */

describe('GET /api/spot/:class/:symbol', () => {
  it('quotes an equity from the Yahoo chart payload', async () => {
    const { status, body } = await get('/api/spot/stock/AAPL');
    assert.equal(status, 200);
    assert.equal(body.symbol, 'AAPL');
    assert.equal(body.assetClass, 'stock');
    assert.equal(body.name, 'Apple Inc.');
    assert.equal(body.currency, 'USD');
    assert.equal(body.venue, 'NasdaqGS');
    assert.equal(body.source, 'yahoo');
    assert.equal(body.price, 305.93);
    // Daily series, so the prior session is the penultimate bar rather than
    // Yahoo's `chartPreviousClose` (which is the close before the window).
    assert.equal(body.previousClose, 305.26);
  });

  it('expands a registry alias before asking the upstream', async () => {
    // Somebody types SPX; Yahoo only knows the index as ^GSPC.
    seen = [];
    await get('/api/spot/stock/SPX');
    assert.ok(
      seen.some((r) => r.includes('/yahoo/v8/finance/chart/%5EGSPC')),
      `expected a ^GSPC chart request, saw ${JSON.stringify(seen)}`,
    );
  });

  it('leaves an alias alone when it names another asset class', async () => {
    // `SPX` is a stock underlying; asked for as crypto it must not silently
    // become the S&P 500 quoted off Coinbase.
    seen = [];
    const { status } = await get('/api/spot/crypto/SPX');
    assert.equal(status, 404);
    assert.ok(seen.some((r) => r.includes('/coinbase/products/SPX-USD/ticker')));
  });

  it('quotes a crypto pair, expanding the bare asset to a USD product', async () => {
    const { status, body } = await get('/api/spot/crypto/btc');
    assert.equal(status, 200);
    assert.equal(body.symbol, 'BTC');
    assert.equal(body.assetClass, 'crypto');
    assert.equal(body.name, 'BTC / USD');
    assert.equal(body.venue, 'Coinbase');
    assert.equal(body.source, 'coinbase');
    assert.equal(body.price, 63039.67);
    // Crypto has no session close, so the 24h open is the comparison.
    assert.equal(body.previousClose, 62000);
    assert.ok(Math.abs(body.changePercent - 1.6769) < 0.001, `changePercent ${body.changePercent}`);
  });

  it('rejects an asset class that names no provider family', async () => {
    const { status, body } = await get('/api/spot/bond/TLT');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
    assert.match(body.error, /Unknown asset class "bond"/);
    assert.match(body.hint, /stock/);
  });

  it('falls through to Nasdaq when Yahoo answers with a page instead of JSON', async () => {
    const { status, body } = await get('/api/spot/stock/FALLBK');
    assert.equal(status, 200);
    // `source` names whichever provider actually answered, which is the whole
    // point of saying it at all.
    assert.equal(body.source, 'nasdaq');
    assert.equal(body.price, 41.2);
    assert.equal(body.previousClose, 40.85);
    assert.equal(body.changePercent, 0.86);
    assert.equal(body.venue, 'NASDAQ-GS');
  });

  it('turns a delisted symbol into a 404, not a bad gateway', async () => {
    const { status, body } = await get('/api/spot/stock/NOSUCHSYM');
    assert.equal(status, 404);
    assert.equal(body.code, 'not_found');
  });

  it('turns an unlisted Coinbase pair into a 404 naming the pair', async () => {
    const { status, body } = await get('/api/spot/crypto/NOTACOIN');
    assert.equal(status, 404);
    assert.equal(body.code, 'not_found');
    assert.match(body.error, /NOTACOIN-USD/);
  });
});

describe('GET /api/spot/:class/:symbol/candles', () => {
  it('returns the bars with the window and provider stated', async () => {
    const { status, body } = await get(
      '/api/spot/stock/AAPL/candles?interval=1440&start=1786000000&end=1787000000',
    );
    assert.equal(status, 200);
    assert.equal(body.symbol, 'AAPL');
    assert.equal(body.assetClass, 'stock');
    assert.equal(body.interval, 1440);
    assert.equal(body.currency, 'USD');
    assert.equal(body.source, 'yahoo');
    assert.equal(body.candles.length, 5);
    assert.deepEqual(body.candles[0], {
      time: 1786368600,
      open: 306.83,
      high: 308.26,
      low: 304.61,
      close: 308.26,
      volume: 44812500,
    });
  });

  it('drops bars outside the requested window rather than the upstream one', async () => {
    // Yahoo serves whole periods; the route asked for a window that ends
    // mid-series, and a chart must not draw past its own axis.
    const { body } = await get(
      '/api/spot/stock/AAPL/candles?interval=1440&start=1786455000&end=1786627800',
    );
    assert.deepEqual(
      body.candles.map((c: { time: number }) => c.time),
      [1786455000, 1786541400, 1786627800],
    );
  });

  it('walks Coinbase back through its 300-bucket pages and sorts oldest first', async () => {
    const { status, body } = await get(
      '/api/spot/crypto/BTC/candles?interval=1440&start=1786800000&end=1786900000',
    );
    assert.equal(status, 200);
    assert.equal(body.symbol, 'BTC');
    assert.equal(body.assetClass, 'crypto');
    assert.equal(body.source, 'coinbase');
    // Coinbase serves [time, low, high, open, close, volume], newest first.
    assert.deepEqual(body.candles, [
      { time: 1786800000, open: 61900, high: 62400, low: 61750, close: 62000, volume: 1104.552 },
      { time: 1786886400, open: 62950.5, high: 63600, low: 62800.1, close: 63039.67, volume: 812.3311 },
    ]);
  });

  it('refuses an interval no venue buckets on', async () => {
    const { status, body } = await get('/api/spot/stock/AAPL/candles?interval=5');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
    assert.match(body.error, /Unsupported candle interval "5"/);
    assert.match(body.hint, /1440/);
  });

  it('falls back to the hourly default for a non-numeric interval', async () => {
    seen = [];
    const { status, body } = await get('/api/spot/stock/AAPL/candles?interval=notanumber');
    assert.equal(status, 200);
    assert.equal(body.interval, 60);
    assert.ok(seen.some((r) => r.includes('interval=1h')));
  });

  it('refuses a window whose start is not before its end', async () => {
    // `start` is clamped to `end`, so asking for a start in the future collapses
    // the window rather than inverting it.
    const { status, body } = await get(
      '/api/spot/stock/AAPL/candles?interval=1440&start=1799999999&end=1786900000',
    );
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
    assert.match(body.error, /start must be before end/);
  });

  it('snaps the window to a 15-second grid so a polling panel keeps hitting the cache', async () => {
    seen = [];
    await get('/api/spot/stock/AAPL/candles?interval=1440&start=1786000007&end=1786999999');
    const chart = seen.find((r) => r.includes('/yahoo/v8/finance/chart/AAPL'));
    assert.ok(chart, `expected a chart request, saw ${JSON.stringify(seen)}`);
    assert.match(chart, /period1=1786000005/);
    assert.match(chart, /period2=1786999995/);
  });
});

describe('GET /api/spot/search', () => {
  it('needs something to search for', async () => {
    const { status, body } = await get('/api/spot/search');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
    assert.match(body.hint, /SSRCH/);
  });

  it('flags the rows an implied overlay can be drawn on', async () => {
    const { status, body } = await get('/api/spot/search?q=apple');
    assert.equal(status, 200);
    assert.equal(body.query, 'apple');
    const symbols = body.results.map((r: { symbol: string }) => r.symbol);
    // The row Yahoo marks `isYahooFinance: false` is a news entity, not a symbol.
    assert.deepEqual(symbols, ['AAPL', '^GSPC']);
    assert.equal(body.results[0].hasImplied, false);
    assert.equal(body.results[1].hasImplied, true);
    assert.equal(body.results[0].assetClass, 'stock');
  });

  it('answers a crypto query from Coinbase when Yahoo declines it', async () => {
    // One arm of the fan-out failing must not take the other's results down.
    const { status, body } = await get('/api/spot/search?q=btc');
    assert.equal(status, 200);
    assert.deepEqual(body.results, [
      { symbol: 'BTC', name: 'BTC/USD', assetClass: 'crypto', venue: 'Coinbase', hasImplied: true },
    ]);
  });

  it('restricts the fan-out to one side when a class is named', async () => {
    seen = [];
    const { status, body } = await get('/api/spot/search?q=apple&class=crypto');
    assert.equal(status, 200);
    assert.deepEqual(body.results, []);
    assert.ok(!seen.some((r) => r.startsWith('/yahoo/v1/finance/search')));
  });

  it('rejects an unknown class filter instead of quietly searching everything', async () => {
    const { status, body } = await get('/api/spot/search?q=apple&class=bond');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
  });

  it('caps the result count at the requested limit', async () => {
    const { body } = await get('/api/spot/search?q=apple&limit=1');
    assert.equal(body.results.length, 1);
  });

  it('answers 200 with an empty list when *every* arm of the fan-out fails', async () => {
    // Recorded rather than endorsed: both arms swallow their own failure, so a
    // total outage reaches the panel as "no such symbol". `/api/xv/series`,
    // the other fan-out here, names its casualties instead.
    cache.clear();
    coinbaseDown = true;
    try {
      const { status, body } = await get('/api/spot/search?q=btc');
      assert.equal(status, 200);
      assert.deepEqual(body, { query: 'btc', results: [] });
    } finally {
      coinbaseDown = false;
      cache.clear();
    }
  });
});

describe('upstream request coalescing, as the routes shape it', () => {
  it('serves a repeated candle poll from cache, because the route quantised the window', async () => {
    // Two panels polling the same chart a second apart must not be two Yahoo
    // calls. The 15-second grid is what makes their cache keys identical.
    cache.clear();
    seen = [];
    await get('/api/spot/stock/AAPL/candles?interval=1440');
    const first = seen.filter((r) => r.startsWith('/yahoo/v8/finance/chart/')).length;
    seen = [];
    await get('/api/spot/stock/AAPL/candles?interval=1440');
    assert.equal(first, 1);
    assert.deepEqual(seen, []);
  });

  it('serves a repeated crypto quote from cache, because its upstream URL carries no clock', async () => {
    cache.clear();
    seen = [];
    await get('/api/spot/crypto/BTC');
    assert.deepEqual(seen, [
      '/coinbase/products/BTC-USD/ticker',
      '/coinbase/products/BTC-USD/stats',
    ]);
    seen = [];
    await get('/api/spot/crypto/BTC');
    assert.deepEqual(seen, []);
  });
});

/* ====================================================================== */
/*  /api/implied                                                          */
/* ====================================================================== */

describe('GET /api/implied/underlyings', () => {
  it('lists every symbol with a mapped ladder, aliases included', async () => {
    const { status, body } = await get('/api/implied/underlyings');
    assert.equal(status, 200);
    const btc = body.underlyings.find((u: { symbol: string }) => u.symbol === 'BTC');
    assert.deepEqual(btc, {
      symbol: 'BTC',
      name: 'Bitcoin',
      assetClass: 'crypto',
      aliases: ['BITCOIN', 'XBT'],
    });
    // An entry with no aliases reports an empty list rather than omitting the key.
    const bnb = body.underlyings.find((u: { symbol: string }) => u.symbol === 'BNB');
    assert.deepEqual(bnb.aliases, []);
  });
});

describe('GET /api/implied/candidates', () => {
  it('needs an underlying', async () => {
    const { status, body } = await get('/api/implied/candidates');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
    assert.match(body.hint, /IMP/);
  });

  it('answers an unmapped symbol with an empty list and a note, not a 404', async () => {
    // Answering 404 made every plain equity chart log a failed request while
    // rendering perfectly well.
    const { status, body } = await get('/api/implied/candidates?symbol=aapl');
    assert.equal(status, 200);
    assert.equal(body.symbol, 'AAPL');
    assert.deepEqual(body.candidates, []);
    assert.match(body.note, /not for individual equities/);
  });

  it('describes each ladder with its live implied price', async () => {
    const { status, body } = await get('/api/implied/candidates?symbol=BTC');
    assert.equal(status, 200);
    assert.equal(body.symbol, 'BTC');
    assert.equal(body.name, 'Bitcoin');
    assert.equal(body.assetClass, 'crypto');
    assert.equal(body.candidates.length, 1);

    const [candidate] = body.candidates;
    assert.equal(candidate.eventTicker, 'KXBTCD-26AUG1617');
    assert.equal(candidate.seriesTicker, 'KXBTCD');
    assert.equal(candidate.strikes, 4);
    assert.equal(candidate.quoted, 4);
    assert.equal(candidate.strikeLow, 62000);
    assert.equal(candidate.strikeHigh, 65000);
    assert.equal(candidate.strikeDate, '2026-08-16T21:00:00Z');
    assert.equal(candidate.volume24h, 18500);
    // The 50% crossing sits between the 63k rung (0.535) and the 64k rung (0.13).
    assert.ok(candidate.implied > 63000 && candidate.implied < 64000, `implied ${candidate.implied}`);
  });

  it('queries one Kalshi series per mapped ladder', async () => {
    cache.clear();
    seen = [];
    await get('/api/implied/candidates?symbol=ETH');
    const asked = seen
      .filter((r) => r.startsWith('/kalshi/events?'))
      .map((r) => new URL(r, 'http://x').searchParams.get('series_ticker'));
    assert.deepEqual(asked.sort(), ['KXETH', 'KXETHD', 'KXETHW', 'KXETHY']);
  });

  it('says so when a mapped underlying has no open ladder', async () => {
    const { status, body } = await get('/api/implied/candidates?symbol=ETH');
    assert.equal(status, 200);
    assert.deepEqual(body.candidates, []);
    assert.match(body.note, /none has an open event right now/);
  });
});

describe('GET /api/implied/series', () => {
  it('needs an event ticker', async () => {
    const { status, body } = await get('/api/implied/series');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
    assert.match(body.hint, /event=/);
  });

  it('refuses an interval Kalshi does not bucket on', async () => {
    const { status, body } = await get('/api/implied/series?event=KXBTCD-26AUG1617&interval=15');
    assert.equal(status, 400);
    assert.match(body.error, /Unsupported candle interval "15"/);
  });

  it('refuses a pricing method it cannot justify', async () => {
    const { status, body } = await get('/api/implied/series?event=KXBTCD-26AUG1617&method=mode');
    assert.equal(status, 400);
    assert.match(body.error, /Unknown method "mode"/);
    assert.match(body.hint, /median/);
  });

  it('re-prices the ladder at every bucket and names the rungs that contributed', async () => {
    const { status, body } = await get(
      '/api/implied/series?event=kxbtcd-26aug1617&interval=1440&start=1786700000&end=1786900000',
    );
    assert.equal(status, 200);
    assert.equal(body.eventTicker, 'KXBTCD-26AUG1617');
    assert.equal(body.title, 'Bitcoin price today at 5pm EDT?');
    assert.equal(body.method, 'median');
    assert.equal(body.interval, 1440);
    // The 65k wing publishes no candles at all; it is counted as skipped rather
    // than silently shrinking the ladder.
    assert.deepEqual(body.contributors, [
      'KXBTCD-26AUG1617-T62000',
      'KXBTCD-26AUG1617-T63000',
      'KXBTCD-26AUG1617-T64000',
    ]);
    assert.equal(body.skipped, 1);
    assert.deepEqual(
      body.points.map((p: { time: number }) => p.time),
      [1786800000, 1786886400],
    );
    assert.equal(body.points[0].strikes, 3);
    assert.ok(body.points[0].value > 63000 && body.points[0].value < 64000);
  });

  it('upper-cases the event ticker before asking Kalshi', async () => {
    cache.clear();
    seen = [];
    await get('/api/implied/series?event=kxbtcd-26aug1617&interval=1440');
    assert.ok(seen.some((r) => r.startsWith('/kalshi/events/KXBTCD-26AUG1617')));
  });

  it('refuses an event that is not a price ladder', async () => {
    const { status, body } = await get('/api/implied/series?event=KXBTCD-26AUG1618');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
    assert.match(body.error, /is not a price ladder/);
    assert.match(body.hint, /at least 3 contracts/);
  });

  it('refuses a window that has collapsed to a point', async () => {
    const { status, body } = await get(
      '/api/implied/series?event=KXBTCD-26AUG1617&interval=1440&start=1786886400&end=1786886400',
    );
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
    assert.match(body.error, /Window start must be before end/);
  });

  it('passes an unknown event through as a 404', async () => {
    const { status, body } = await get('/api/implied/series?event=KXNOSUCH-26AUG');
    assert.equal(status, 404);
    assert.equal(body.code, 'not_found');
  });
});

/* ====================================================================== */
/*  /api/xv — the fan-out over three catalogues                           */
/* ====================================================================== */

describe('GET /api/xv/series', () => {
  it('names the venue whose catalogue failed and still serves the ones that answered', async () => {
    cache.clear();
    gammaDown = true;
    try {
      const { status, body } = await get('/api/xv/series');
      assert.equal(status, 200);
      // A dead venue is reported, not omitted: a board silently missing a
      // column reads as "nobody else lists this".
      assert.deepEqual(
        body.unavailable.map((u: { venue: string }) => u.venue),
        ['polymarket'],
      );
      assert.ok(body.unavailable[0].error.length > 0);
      assert.deepEqual(body.scanned, { kalshi: 2, 'polymarket-us': 1 });

      // The curated Fed link still resolves across the two survivors.
      const fed = body.series.find((s: { key: string }) => s.key === 'fed-decision');
      assert.ok(fed, `expected the curated fed link, saw ${JSON.stringify(body.series)}`);
      assert.equal(fed.confidence, 'linked');
      assert.equal(fed.score, 1);
      assert.deepEqual(
        fed.legs.map((l: { venue: string; seriesTicker: string }) => [l.venue, l.seriesTicker]),
        [
          ['kalshi', 'KXFEDDECISION'],
          ['polymarket-us', 'usfed-fomc'],
        ],
      );
      assert.equal(typeof body.snapshotAgeSeconds, 'number');
    } finally {
      gammaDown = false;
    }
  });

  it('reports every venue healthy once they all answer', async () => {
    cache.clear();
    const { status, body } = await get('/api/xv/series');
    assert.equal(status, 200);
    assert.deepEqual(body.unavailable, []);
    assert.deepEqual(body.scanned, { kalshi: 2, polymarket: 0, 'polymarket-us': 1 });
  });

  it('filters the board on every word of the query', async () => {
    const { body } = await get('/api/xv/series?q=fed%20decision');
    assert.deepEqual(
      body.series.map((s: { key: string }) => s.key),
      ['fed-decision'],
    );
    assert.equal(body.query, 'fed decision');

    const none = await get('/api/xv/series?q=fed%20zzzznotaword');
    assert.deepEqual(none.body.series, []);
  });

  it('caps the board at the requested limit', async () => {
    const { body } = await get('/api/xv/series?limit=0');
    // `limit` is clamped to at least 1 rather than read as "none".
    assert.equal(body.series.length, 1);
  });
});

describe('GET /api/xv/compare', () => {
  it('needs an event', async () => {
    const { status, body } = await get('/api/xv/compare');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
    assert.match(body.hint, /XV /);
  });

  it('rejects a venue filter that names no venue', async () => {
    const { status, body } = await get('/api/xv/compare?event=KXFEDDECISION-26OCT&venue=betfair');
    assert.equal(status, 400);
    assert.match(body.error, /Unknown venue "betfair"/);
    assert.match(body.hint, /polymarket-us/);
  });

  it('404s an event no venue has open', async () => {
    const { status, body } = await get('/api/xv/compare?event=KXNOTHING-26DEC');
    assert.equal(status, 404);
    assert.equal(body.code, 'not_found');
  });

  it('quotes one question at every broker that lists it', async () => {
    const { status, body } = await get('/api/xv/compare?event=KXFEDDECISION-26OCT');
    assert.equal(status, 200);
    assert.equal(body.title, 'Fed decision in October 2026?');
    // The anchor is stated; the partner carries the reading that found it. A
    // curated link picks the *series*, but the expiry within it is still a
    // wording-plus-close-date match, and it is labelled as one.
    assert.deepEqual(
      body.events.map((e: { venue: string; confidence: string }) => [e.venue, e.confidence]),
      [
        ['kalshi', 'linked'],
        ['polymarket-us', 'strong'],
      ],
    );
    assert.match(body.events[1].reason, /closes the same day/);

    const cut = body.rows.find((r: { label: string }) => r.label === '25 bps decrease');
    assert.ok(cut, `expected a paired row, saw ${JSON.stringify(body.rows)}`);
    assert.deepEqual(
      cut.legs.map((l: { venue: string; mid: number }) => [l.venue, l.mid]),
      [
        ['kalshi', 0.64],
        ['polymarket-us', 0.67],
      ],
    );
    // Divergence is the gap between the mids; edge is the cheapest ask against
    // the richest bid, which the spreads make negative here.
    assert.equal(cut.divergence, 0.03);
    assert.equal(cut.edge, 0.01);
    assert.deepEqual(cut.edgeVenues, ['kalshi', 'polymarket-us']);
  });

  it('falls back to the catalogue snapshot when a live re-quote fails', async () => {
    // One venue failing must not take the comparison down — the stale row is
    // better than no row, and the anchor is unaffected.
    cache.clear();
    pmusEventDown = true;
    try {
      const { status, body } = await get('/api/xv/compare?event=KXFEDDECISION-26OCT');
      assert.equal(status, 200);
      assert.deepEqual(
        body.events.map((e: { venue: string }) => e.venue),
        ['kalshi', 'polymarket-us'],
      );
      assert.equal(body.rows.length, 2);
    } finally {
      pmusEventDown = false;
    }
  });

  it('reads an empty venue filter as no filter rather than as an unknown venue', async () => {
    const { status, body } = await get('/api/xv/compare?event=KXFEDDECISION-26OCT&venue=');
    assert.equal(status, 200);
    assert.equal(body.events[0].venue, 'kalshi');
  });

  it('honours a venue filter when locating the anchor', async () => {
    cache.clear();
    const { status, body } = await get(
      '/api/xv/compare?event=usfed-fomc-2026-10-28&venue=polymarket-us',
    );
    assert.equal(status, 200);
    assert.equal(body.events[0].venue, 'polymarket-us');
    assert.equal(body.events[0].reason, 'anchor');
  });
});

/* ====================================================================== */
/*  /api/fred                                                             */
/* ====================================================================== */

describe('GET /api/fred/series/:id', () => {
  it('returns the observations with FRED’s missing marker preserved as null', async () => {
    const { status, body } = await get('/api/fred/series/UNRATE');
    assert.equal(status, 200);
    assert.deepEqual(body.observations, [
      { date: '2024-01-01', value: 3.7 },
      { date: '2024-02-01', value: 3.9 },
      { date: '2024-03-01', value: null },
      { date: '2024-04-01', value: 3.9 },
    ]);
    assert.equal(body.series.id, 'UNRATE');
    assert.equal(body.series.title, 'Unemployment Rate');
    assert.equal(body.series.frequency, 'Monthly');
    assert.equal(body.series.source, 'scrape');
  });

  it('passes the date window through to the CSV endpoint', async () => {
    seen = [];
    const { status } = await get('/api/fred/series/GDPC1?start=2020-01-01&end=2021-01-01');
    assert.equal(status, 200);
    const csv = seen.find((r) => r.startsWith('/fred/graph/fredgraph.csv'));
    assert.ok(csv);
    assert.match(csv, /cosd=2020-01-01/);
    assert.match(csv, /coed=2021-01-01/);
  });

  it('rejects a date that is not YYYY-MM-DD, naming which one', async () => {
    const { status, body } = await get('/api/fred/series/UNRATE?start=01%2F01%2F2020');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
    assert.match(body.error, /is not a valid start date/);

    const end = await get('/api/fred/series/UNRATE?end=2020');
    assert.equal(end.status, 400);
    assert.match(end.body.error, /is not a valid end date/);
  });

  it('rejects a series id that could not name a series', async () => {
    const { status, body } = await get('/api/fred/series/UN%20RATE');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
    assert.match(body.error, /not a valid FRED series id/);
  });

  it('reads FRED’s not-found page as a 404 rather than a parse failure', async () => {
    const { status, body } = await get('/api/fred/series/NOSUCH');
    assert.equal(status, 404);
    assert.equal(body.code, 'not_found');
  });
});

describe('GET /api/fred/search', () => {
  it('parses the search page into series rows', async () => {
    const { status, body } = await get('/api/fred/search?q=unemployment');
    assert.equal(status, 200);
    assert.equal(body.query, 'unemployment');
    assert.equal(body.source, 'scrape');
    assert.deepEqual(
      body.results.map((r: { id: string }) => r.id),
      ['UNRATE', 'U6RATE'],
    );
    assert.equal(body.results[0].frequency, 'Monthly');
  });

  it('honours the limit', async () => {
    const { body } = await get('/api/fred/search?q=unemployment&limit=1');
    assert.equal(body.results.length, 1);
  });

  it('refuses an empty query rather than searching for nothing', async () => {
    const { status, body } = await get('/api/fred/search?q=%20%20');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
    assert.match(body.error, /at least one word/);
  });
});

/* ====================================================================== */
/*  /api/news                                                             */
/* ====================================================================== */

describe('GET /api/news', () => {
  it('returns the normalised wire for the symbols asked for', async () => {
    const { status, body } = await get('/api/news?symbols=nvda');
    assert.equal(status, 200);
    assert.deepEqual(body.symbols, ['NVDA']);
    assert.equal(body.days, 7);
    assert.equal(body.source, 'Alpaca / Benzinga');
    assert.equal(body.articles.length, 1);
    assert.equal(
      body.articles[0].headline,
      "Nvidia Q3 Beat: Data Center Revenue Tops Street's $30B View",
    );
    assert.equal(body.articles[0].summary, 'Shares of NVIDIA rose in after-hours trade.');
    assert.equal(body.articles[0].time, 1786739467);
  });

  it('reads no symbols as the whole wire rather than as an error', async () => {
    seen = [];
    const { status, body } = await get('/api/news');
    assert.equal(status, 200);
    assert.deepEqual(body.symbols, []);
    const call = seen.find((r) => r.startsWith('/alpaca/news'));
    assert.ok(call);
    assert.ok(!call.includes('symbols='));
  });

  it('clamps a limit and a lookback the upstream would reject', async () => {
    seen = [];
    await get('/api/news?symbols=AMD&limit=99999&days=99999');
    const call = seen.find((r) => r.startsWith('/alpaca/news'));
    assert.ok(call);
    const query = new URL(call, 'http://x').searchParams;
    assert.equal(query.get('limit'), '50');
    // `days` is clamped before it becomes a `start` timestamp.
    const start = Date.parse(query.get('start') ?? '');
    assert.ok(Date.now() - start < 400 * 86_400_000, `start was ${query.get('start')}`);
  });

  it('refuses a symbol that would smuggle query parameters upstream', async () => {
    seen = [];
    const { status, body } = await get('/api/news?symbols=NVDA%26limit%3D9999');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
    assert.match(body.error, /not a symbol/);
    assert.ok(!seen.some((r) => r.startsWith('/alpaca/news')));
  });

  it('surfaces a rejected key as a bad gateway, not as an empty feed', async () => {
    const { status, body } = await get('/api/news?symbols=BADKEY');
    assert.equal(status, 502);
    assert.equal(body.code, 'bad_credentials');
    assert.equal(body.status, 401);
    assert.match(body.hint, /ALPACA_API_SECRET_KEY/);
  });
});

/* ====================================================================== */
/*  /api/ent                                                              */
/* ====================================================================== */

describe('GET /api/ent/markets', () => {
  it('reads the shared Kalshi snapshot and tags each event with its genres', async () => {
    const { status, body } = await get('/api/ent/markets');
    assert.equal(status, 200);
    assert.equal(body.genre, 'all');
    // Only the Entertainment-category event is in scope; the Fed event is not.
    assert.deepEqual(
      body.events.map((e: { eventTicker: string }) => e.eventTicker),
      ['KXRT-26DUNE3'],
    );
    assert.deepEqual(body.events[0].genres, ['film']);
    assert.equal(body.counts.all, 1);
    assert.equal(body.counts.film, 1);
    assert.equal(body.counts.music, 0);
    assert.equal(body.scanned, 2);
  });

  it('filters to one genre', async () => {
    const film = await get('/api/ent/markets?genre=film');
    assert.equal(film.body.events.length, 1);
    const music = await get('/api/ent/markets?genre=music');
    assert.equal(music.status, 200);
    assert.deepEqual(music.body.events, []);
  });

  it('rejects a genre that is not one', async () => {
    const { status, body } = await get('/api/ent/markets?genre=opera');
    assert.equal(status, 400);
    assert.equal(body.code, 'bad_request');
    assert.match(body.error, /is not an entertainment genre/);
  });
});

describe('GET /api/ent/charts', () => {
  it('lists both mirrors when no source is named', async () => {
    const { status, body } = await get('/api/ent/charts');
    assert.equal(status, 200);
    assert.ok(body.charts.length >= 7);
    assert.ok(body.charts.some((c: { source: string }) => c.source === 'spotify'));
    assert.ok(body.charts.some((c: { source: string }) => c.source === 'youtube'));
    assert.deepEqual(Object.keys(body.charts[0]).sort(), ['name', 'slug', 'source']);
  });

  it('filters to one source, and ignores a source it does not mirror', async () => {
    const spotify = await get('/api/ent/charts?source=SPOTIFY');
    assert.ok(spotify.body.charts.every((c: { source: string }) => c.source === 'spotify'));

    const bogus = await get('/api/ent/charts?source=tidal');
    const all = await get('/api/ent/charts');
    assert.equal(bogus.body.charts.length, all.body.charts.length);
  });
});

describe('entertainment parameter validation', () => {
  // These are the feeds whose upstreams are not overridable, so what is
  // reachable is the guard in front of the network — which is also the half a
  // user hits by mistyping.
  const cases: [string, string, RegExp][] = [
    ['/api/ent/rt/search', 'a search with no text', /Missing search text/],
    ['/api/ent/rt', 'a title lookup with no title', /Missing title/],
    ['/api/ent/netflix?category=documentaries', 'an unknown Netflix category', /not a Netflix category/],
    ['/api/ent/netflix?scope=usa', 'a scope that is not a country code', /not a valid Netflix scope/],
    ['/api/ent/spotify?scope=usa', 'a Spotify country that is not two letters', /not a country code/],
    ['/api/ent/youtube?view=yesterday', 'a YouTube chart that does not exist', /not a YouTube chart/],
    ['/api/ent/boxoffice?date=08%2F14%2F2026', 'a US-formatted box office date', /not a valid box office date/],
    ['/api/ent/tv?date=2026-8-1', 'an unpadded schedule date', /not a valid schedule date/],
    ['/api/ent/tv?country=USA', 'a three-letter country', /not a country code/],
    ['/api/ent/steam?q=0', 'app id zero', /not a valid Steam app id/],
  ];

  for (const [path, what, message] of cases) {
    it(`rejects ${what} before making a request`, async () => {
      seen = [];
      const { status, body } = await get(path);
      assert.equal(status, 400, `${path} answered ${status}`);
      assert.equal(body.code, 'bad_request');
      assert.match(body.error, message);
      assert.deepEqual(seen, []);
    });
  }

  it('defaults the Netflix category to tv when the parameter is blank', async () => {
    // `strParam(..., 'tv') || 'tv'` has to survive an explicitly empty value,
    // not just an absent one — a panel that clears its input sends `?category=`.
    seen = [];
    const { status } = await get('/api/ent/netflix?category=');
    // The upstream is netflix.com, which this test cannot reach; what matters
    // is that it got as far as trying rather than being rejected as invalid.
    assert.notEqual(status, 400);
  });
});

/* ====================================================================== */
/*  the API surface itself                                                */
/* ====================================================================== */

describe('unknown API routes', () => {
  it('answer 404 as JSON rather than falling through to the client shell', async () => {
    const { status, body } = await get('/api/spot');
    assert.equal(status, 404);
    assert.equal(body.code, 'not_found');
  });
});
