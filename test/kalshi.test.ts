/**
 * Kalshi normalisation tests.
 *
 * The fixtures are trimmed copies of real `api.elections.kalshi.com` responses,
 * so the fixed-point string formats (`"0.0900"`, `"39942.93"`) and the shape of
 * a no-trade candlestick are exactly what the upstream sends.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  normaliseCandles,
  normaliseOrderBook,
  seriesFromTicker,
  type RawCandle,
} from '../src/server/sources/kalshi.js';

describe('seriesFromTicker', () => {
  it('takes the segment before the first hyphen', () => {
    assert.equal(seriesFromTicker('KXFEDDECISION-27JAN-H26'), 'KXFEDDECISION');
    assert.equal(seriesFromTicker('KXHIGHNY-26AUG16-B82.5'), 'KXHIGHNY');
  });

  it('returns the whole ticker when there is no hyphen', () => {
    assert.equal(seriesFromTicker('KXELONMARS'), 'KXELONMARS');
  });
});

describe('normaliseOrderBook', () => {
  // Real shape: two *bid* ladders, ascending by price, as decimal strings.
  const raw = {
    orderbook_fp: {
      yes_dollars: [
        ['0.0600', '2392.98'],
        ['0.0700', '4195.84'],
        ['0.0900', '925.60'],
      ] as [string, string][],
      no_dollars: [
        ['0.7800', '264.32'],
        ['0.8800', '338.81'],
        ['0.9000', '10.00'],
      ] as [string, string][],
    },
  };

  it('sorts each bid ladder best-first', () => {
    const book = normaliseOrderBook(raw, 'X');
    assert.deepEqual(
      book.yes.map((l) => l.price),
      [0.09, 0.07, 0.06],
    );
    assert.deepEqual(
      book.no.map((l) => l.price),
      [0.9, 0.88, 0.78],
    );
  });

  it('inverts the NO ladder into YES asks, cheapest first', () => {
    const book = normaliseOrderBook(raw, 'X');
    // A NO bid at 0.90 is an offer to sell YES at 0.10.
    assert.deepEqual(
      book.yesAsks.map((l) => l.price),
      [0.1, 0.12, 0.22],
    );
    assert.equal(book.yesAsks[0]!.size, 10);
  });

  it('produces no floating-point dust from the 1 - q inversion', () => {
    const book = normaliseOrderBook(
      { orderbook_fp: { no_dollars: [['0.7000', '5']] as [string, string][] } },
      'X',
    );
    // 1 - 0.7 is 0.30000000000000004 in binary floating point.
    assert.equal(book.yesAsks[0]!.price, 0.3);
  });

  it('derives best bid, best ask, spread and mid', () => {
    const book = normaliseOrderBook(raw, 'X');
    assert.equal(book.bestYesBid, 0.09);
    assert.equal(book.bestYesAsk, 0.1);
    assert.equal(book.spread, 0.01);
    assert.equal(book.mid, 0.095);
  });

  it('reports nulls rather than a fake quote for an empty book', () => {
    const book = normaliseOrderBook({}, 'X');
    assert.deepEqual(book.yes, []);
    assert.equal(book.bestYesBid, null);
    assert.equal(book.bestYesAsk, null);
    assert.equal(book.spread, null);
    assert.equal(book.mid, null);
  });

  it('leaves spread null when only one side rests', () => {
    const book = normaliseOrderBook(
      { orderbook_fp: { yes_dollars: [['0.4000', '10']] as [string, string][] } },
      'X',
    );
    assert.equal(book.bestYesBid, 0.4);
    assert.equal(book.bestYesAsk, null);
    assert.equal(book.spread, null);
  });

  it('drops zero-price and zero-size rows', () => {
    const book = normaliseOrderBook(
      {
        orderbook_fp: {
          yes_dollars: [
            ['0.0000', '100'],
            ['0.5000', '0.00'],
            ['0.4000', '10'],
          ] as [string, string][],
        },
      },
      'X',
    );
    assert.equal(book.yes.length, 1);
    assert.equal(book.yes[0]!.price, 0.4);
  });

  it('honours the depth cap', () => {
    const rows = Array.from({ length: 40 }, (_, i) => [`0.${String(i + 10)}00`, '5']) as [
      string,
      string,
    ][];
    const book = normaliseOrderBook({ orderbook_fp: { yes_dollars: rows } }, 'X', 5);
    assert.equal(book.yes.length, 5);
  });
});

describe('normaliseCandles', () => {
  const traded: RawCandle = {
    end_period_ts: 1786852800,
    open_interest_fp: '39997.12',
    volume_fp: '90.41',
    price: {
      open_dollars: '0.1000',
      high_dollars: '0.1100',
      low_dollars: '0.1000',
      close_dollars: '0.1000',
      mean_dollars: '0.1050',
      previous_dollars: '0.1100',
    },
    yes_bid: { close_dollars: '0.1000' },
    yes_ask: { close_dollars: '0.1100' },
  };

  // What Kalshi returns for a period in which nothing printed: `price` carries
  // only `previous_dollars`, with no OHLC at all.
  const quiet: RawCandle = {
    end_period_ts: 1786866000,
    open_interest_fp: '39997.12',
    volume_fp: '0.00',
    price: { previous_dollars: '0.1000' },
    yes_bid: { close_dollars: '0.0900' },
    yes_ask: { close_dollars: '0.1100' },
  };

  it('maps a traded period to real OHLC', () => {
    const [c] = normaliseCandles([traded]);
    assert.equal(c!.open, 0.1);
    assert.equal(c!.high, 0.11);
    assert.equal(c!.low, 0.1);
    assert.equal(c!.close, 0.1);
    assert.equal(c!.volume, 90.41);
    assert.equal(c!.openInterest, 39997.12);
    assert.equal(c!.traded, true);
  });

  it('synthesises a flat candle for a period with no prints', () => {
    const [c] = normaliseCandles([quiet]);
    // Charts must stay continuous — a quiet hour is a flat bar, not a gap.
    assert.equal(c!.open, 0.1);
    assert.equal(c!.high, 0.1);
    assert.equal(c!.low, 0.1);
    assert.equal(c!.close, 0.1);
    assert.equal(c!.traded, false);
    assert.equal(c!.volume, 0);
  });

  it('carries the previous close forward when previous_dollars is absent', () => {
    const orphan: RawCandle = { end_period_ts: 1786870000, price: {}, volume_fp: '0' };
    const candles = normaliseCandles([traded, orphan]);
    assert.equal(candles.length, 2);
    assert.equal(candles[1]!.close, candles[0]!.close);
    assert.equal(candles[1]!.traded, false);
  });

  it('falls back to the book mid when there is no price history at all', () => {
    const first: RawCandle = {
      end_period_ts: 1786870000,
      price: {},
      yes_bid: { close_dollars: '0.2000' },
      yes_ask: { close_dollars: '0.3000' },
    };
    const [c] = normaliseCandles([first]);
    assert.equal(c!.close, 0.25);
  });

  it('drops a leading candle with no price information of any kind', () => {
    assert.deepEqual(normaliseCandles([{ end_period_ts: 1, price: {} }]), []);
  });

  it('sorts output ascending by time, as lightweight-charts requires', () => {
    const candles = normaliseCandles([quiet, traded]);
    assert.ok(candles[0]!.time < candles[1]!.time);
  });

  it('exposes the closing bid and ask, with an empty side as null', () => {
    const [c] = normaliseCandles([
      { ...traded, yes_bid: { close_dollars: '0.0000' }, yes_ask: { close_dollars: '0.1100' } },
    ]);
    assert.equal(c!.bid, null);
    assert.equal(c!.ask, 0.11);
  });

  it('returns an empty array for an empty response', () => {
    assert.deepEqual(normaliseCandles([]), []);
  });
});
