/**
 * Spot price normalisation tests.
 *
 * The fixtures are trimmed copies of real responses — a Yahoo Finance chart
 * payload for AAPL, a Nasdaq historical table, and a Coinbase candles array —
 * so the quirks under test are the upstreams' actual quirks: Yahoo's parallel
 * arrays with nulls punched through them and its missing `previousClose`,
 * Nasdaq's money-as-strings, and Coinbase's positional, newest-first rows.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  parseNasdaqDate,
  parseNasdaqHistorical,
  parseNasdaqNumber,
  parseYahooChart,
} from '../src/server/sources/stocks.js';
import { baseSymbol, normaliseCandles, productId } from '../src/server/sources/crypto.js';

/* ------------------------------------------------------------------ yahoo */

/** Real `query1.finance.yahoo.com/v8/finance/chart/AAPL?interval=1d` response. */
const YAHOO_AAPL = {
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
          // Note: no `previousClose`. Yahoo omits it here and offers only
          // `chartPreviousClose`, which is the close before the *window*.
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
              open: [306.8299865722656, 307.75, 305.1000061035156, 304.2099914550781, 306],
              high: [308.260009765625, 309.9700012207031, 305.6600036621094, 306, 307.489990234375],
              low: [
                304.6099853515625, 302.7900085449219, 300.57000732421875, 302.04998779296875,
                304.29998779296875,
              ],
              close: [
                308.260009765625, 304.9100036621094, 302.25, 305.260009765625, 305.92999267578125,
              ],
              volume: [44812500, 37476700, 41657800, 40349300, 28186700],
            },
          ],
        },
      },
    ],
    error: null,
  },
};

describe('parseYahooChart', () => {
  it('reads the bars out of the parallel arrays', () => {
    const { candles } = parseYahooChart(YAHOO_AAPL, 'AAPL', 1440);
    assert.equal(candles.length, 5);
    assert.deepEqual(candles[0], {
      time: 1786368600,
      open: 306.8299865722656,
      high: 308.260009765625,
      low: 304.6099853515625,
      close: 308.260009765625,
      volume: 44812500,
    });
  });

  it('takes the previous close from the penultimate daily bar', () => {
    // The bug this guards: `chartPreviousClose` (313.33) is the close before
    // the requested week, so trusting it quoted AAPL at -2.4% on a day it
    // closed +0.22%. The bar before the last one is the real prior session.
    const { quote } = parseYahooChart(YAHOO_AAPL, 'AAPL', 1440);
    assert.equal(quote.previousClose, 305.260009765625);
    assert.ok(quote.change !== null && Math.abs(quote.change - 0.67) < 0.01);
    assert.ok(quote.changePercent !== null && Math.abs(quote.changePercent - 0.219) < 0.01);
  });

  it('falls back to chartPreviousClose when the series is not daily', () => {
    // On an intraday series the penultimate bar is a minute ago, not a session.
    const { quote } = parseYahooChart(YAHOO_AAPL, 'AAPL', 60);
    assert.equal(quote.previousClose, 313.33);
  });

  it('takes the day open from the last bar, not the first', () => {
    // The first bar opens the *window*; only the last one opens today.
    const { quote } = parseYahooChart(YAHOO_AAPL, 'AAPL', 1440);
    assert.equal(quote.dayOpen, 306);
  });

  it('carries the instrument metadata through', () => {
    const { quote } = parseYahooChart(YAHOO_AAPL, 'AAPL', 1440);
    assert.equal(quote.symbol, 'AAPL');
    assert.equal(quote.name, 'Apple Inc.');
    assert.equal(quote.venue, 'NasdaqGS');
    assert.equal(quote.currency, 'USD');
    assert.equal(quote.source, 'yahoo');
    assert.equal(quote.price, 305.93);
  });

  it('skips buckets Yahoo padded with nulls', () => {
    // Halted or not-yet-printed periods come back as null in every array. A bar
    // with no close is not a bar, and interpolating one would invent a price.
    const padded = structuredClone(YAHOO_AAPL);
    const rows = padded.chart.result[0]!.indicators.quote[0]!;
    rows.close[2] = null as never;
    const { candles } = parseYahooChart(padded, 'AAPL', 1440);
    assert.equal(candles.length, 4);
    assert.ok(!candles.some((c) => c.time === 1786541400));
  });

  it('backfills a missing high or low from the bar it does have', () => {
    const partial = structuredClone(YAHOO_AAPL);
    const rows = partial.chart.result[0]!.indicators.quote[0]!;
    rows.high[0] = null as never;
    rows.low[0] = null as never;
    const { candles } = parseYahooChart(partial, 'AAPL', 1440);
    assert.equal(candles[0]!.high, Math.max(candles[0]!.open, candles[0]!.close));
    assert.equal(candles[0]!.low, Math.min(candles[0]!.open, candles[0]!.close));
  });

  it('throws a diagnosable error when Yahoo has no result', () => {
    assert.throws(
      () => parseYahooChart({ chart: { result: [], error: { description: 'No data found' } } }, 'NOPE'),
      /No data found/,
    );
  });
});

/* ----------------------------------------------------------------- nasdaq */

describe('parseNasdaqNumber', () => {
  it('strips currency symbols and thousands separators', () => {
    assert.equal(parseNasdaqNumber('$305.93'), 305.93);
    assert.equal(parseNasdaqNumber('28,229,611'), 28229611);
    assert.equal(parseNasdaqNumber('+0.22%'), 0.22);
    assert.equal(parseNasdaqNumber('776.34'), 776.34);
  });

  it('returns null for the absent-value markers rather than 0', () => {
    assert.equal(parseNasdaqNumber('N/A'), null);
    assert.equal(parseNasdaqNumber('--'), null);
    assert.equal(parseNasdaqNumber(''), null);
    assert.equal(parseNasdaqNumber(undefined), null);
  });
});

describe('parseNasdaqDate', () => {
  it('reads MM/DD/YYYY as UTC midnight', () => {
    assert.equal(parseNasdaqDate('08/14/2026'), Date.UTC(2026, 7, 14) / 1000);
  });

  it('returns null for anything else', () => {
    assert.equal(parseNasdaqDate('2026-08-14'), null);
    assert.equal(parseNasdaqDate(undefined), null);
  });
});

describe('parseNasdaqHistorical', () => {
  // Real `api.nasdaq.com/api/quote/AAPL/historical` rows, newest first.
  const body = {
    data: {
      tradesTable: {
        rows: [
          { date: '08/14/2026', close: '$305.93', volume: '28,229,380', open: '$306.00', high: '$307.49', low: '$304.30' },
          { date: '08/13/2026', close: '$305.26', volume: '40,349,290', open: '$304.21', high: '$306.00', low: '$302.05' },
          { date: '08/12/2026', close: '$302.25', volume: '41,657,770', open: '$305.10', high: '$305.66', low: '$300.57' },
        ],
      },
    },
  };

  it('sorts oldest-first and parses the money strings', () => {
    const candles = parseNasdaqHistorical(body);
    assert.equal(candles.length, 3);
    assert.deepEqual(candles.map((c) => c.close), [302.25, 305.26, 305.93]);
    assert.deepEqual(candles[2], {
      time: Date.UTC(2026, 7, 14) / 1000,
      open: 306,
      high: 307.49,
      low: 304.3,
      close: 305.93,
      volume: 28229380,
    });
  });

  it('drops rows with no usable date or close', () => {
    const candles = parseNasdaqHistorical({
      data: { tradesTable: { rows: [{ date: 'N/A', close: '$1.00' }, { date: '08/14/2026' }] } },
    });
    assert.equal(candles.length, 0);
  });

  it('survives an empty payload', () => {
    assert.deepEqual(parseNasdaqHistorical({}), []);
    assert.deepEqual(parseNasdaqHistorical({ data: null }), []);
  });
});

/* ----------------------------------------------------------------- crypto */

describe('productId / baseSymbol', () => {
  it('assumes a USD quote when only the asset is given', () => {
    assert.equal(productId('btc'), 'BTC-USD');
    assert.equal(productId('ETH'), 'ETH-USD');
  });

  it('leaves an explicit pair alone', () => {
    assert.equal(productId('BTC-USDC'), 'BTC-USDC');
  });

  it('recovers the bare asset symbol', () => {
    assert.equal(baseSymbol('BTC-USD'), 'BTC');
    assert.equal(baseSymbol('eth'), 'ETH');
  });
});

describe('crypto normaliseCandles', () => {
  // Real Coinbase rows: [time, low, high, open, close, volume], newest first.
  const raw: [number, number, number, number, number, number][] = [
    [1786892400, 63034.7, 63050, 63037.75, 63039.67, 7.35248751],
    [1786888800, 62987.68, 63039, 62993.64, 63037.75, 73.74841128],
  ];

  it('maps the positional array onto named fields in the right order', () => {
    // The trap: the array is low/high/open/close, not open/high/low/close.
    const candles = normaliseCandles(raw);
    assert.deepEqual(candles[0], {
      time: 1786892400,
      open: 63037.75,
      high: 63050,
      low: 63034.7,
      close: 63039.67,
      volume: 7.35248751,
    });
  });

  it('skips malformed rows instead of emitting NaN bars', () => {
    const candles = normaliseCandles([
      ...raw,
      [1786885200, 1, 2, 3] as never,
      null as never,
    ]);
    assert.equal(candles.length, 2);
  });
});
