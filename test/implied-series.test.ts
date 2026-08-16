/**
 * Historical implied-series assembly tests.
 *
 * `buildPoints` is where N per-strike candle series become one price series,
 * and its two rules are easy to get wrong and invisible when you do: a rung
 * that has stopped printing must carry its last quote forward, and a rung must
 * never be priced with a candle from the future. Both are tested here against
 * ladders with deliberately ragged coverage.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import type { Candle, Market } from '../src/shared/types.js';
import { buildPoints, findUnderlying, selectStrikes } from '../src/server/sources/implied.js';

/** A ladder rung. Only the fields the maths reads are filled in. */
function rung(ticker: string, floorStrike: number, extra: Partial<Market> = {}): Market {
  return {
    ticker,
    eventTicker: 'EVT',
    seriesTicker: 'SER',
    title: ticker,
    yesSubTitle: '',
    noSubTitle: '',
    status: 'open',
    marketType: 'binary',
    yesBid: null,
    yesAsk: null,
    noBid: null,
    noAsk: null,
    mid: null,
    lastPrice: null,
    previousPrice: null,
    change: null,
    volume: 0,
    volume24h: 0,
    openInterest: 0,
    liquidity: 0,
    openTime: '',
    closeTime: '',
    expirationTime: '',
    result: '',
    rulesPrimary: '',
    strikeType: 'greater',
    floorStrike,
    capStrike: null,
    ...extra,
  };
}

/** A candle carrying a two-sided book, which is what the maths prefers. */
function candle(time: number, bid: number, ask: number): Candle {
  const mid = (bid + ask) / 2;
  return {
    time,
    open: mid,
    high: mid,
    low: mid,
    close: mid,
    volume: 0,
    openInterest: 0,
    traded: true,
    bid,
    ask,
  };
}

describe('buildPoints', () => {
  it('prices the ladder at every bucket on the union timeline', () => {
    const series = [
      { market: rung('A', 100), candles: [candle(10, 0.89, 0.91), candle(20, 0.79, 0.81)] },
      { market: rung('B', 110), candles: [candle(10, 0.49, 0.51), candle(20, 0.39, 0.41)] },
      { market: rung('C', 120), candles: [candle(10, 0.09, 0.11), candle(20, 0.04, 0.06)] },
    ];

    const points = buildPoints(series, 'median');
    assert.deepEqual(points.map((p) => p.time), [10, 20]);
    // At t=10 the 50% crossing sits on strike 110 exactly.
    assert.equal(points[0]!.value, 110);
    assert.equal(points[0]!.strikes, 3);
    // At t=20 survival falls 0.8 -> 0.4 across [100, 110]: crossing at 107.5.
    assert.equal(points[1]!.value, 107.5);
  });

  it('carries a rung that stopped printing forward, rather than dropping it', () => {
    // A resting book does not vanish because no candle closed. Dropping the
    // rung would silently shrink the ladder and move the crossing.
    const series = [
      { market: rung('A', 100), candles: [candle(10, 0.89, 0.91), candle(20, 0.89, 0.91)] },
      { market: rung('B', 110), candles: [candle(10, 0.49, 0.51)] },
      { market: rung('C', 120), candles: [candle(10, 0.09, 0.11), candle(20, 0.09, 0.11)] },
    ];

    const points = buildPoints(series, 'median');
    assert.equal(points.length, 2);
    assert.equal(points[1]!.strikes, 3, 'the stale rung should still contribute');
    assert.equal(points[1]!.value, 110);
  });

  it('never prices a rung with a candle it did not yet have', () => {
    // Rung C only starts at t=20. At t=10 the ladder is two rungs, not three,
    // and must not borrow C's later quote.
    const series = [
      { market: rung('A', 100), candles: [candle(10, 0.89, 0.91), candle(20, 0.89, 0.91)] },
      { market: rung('B', 110), candles: [candle(10, 0.49, 0.51), candle(20, 0.49, 0.51)] },
      { market: rung('C', 120), candles: [candle(20, 0.09, 0.11)] },
    ];

    const points = buildPoints(series, 'median');
    assert.equal(points[0]!.strikes, 2);
    assert.equal(points[1]!.strikes, 3);
  });

  it('emits a null point rather than a guess when too few rungs are quoted', () => {
    const series = [
      { market: rung('A', 100), candles: [candle(10, 0.89, 0.91)] },
      { market: rung('B', 110), candles: [candle(20, 0.49, 0.51)] },
    ];

    const points = buildPoints(series, 'median');
    assert.equal(points[0]!.value, null);
    assert.equal(points[0]!.strikes, 1);
  });

  it('returns nothing for an empty ladder', () => {
    assert.deepEqual(buildPoints([], 'median'), []);
  });

  it('ends the line at expiry, because a settled ladder implies nothing', () => {
    // Kalshi keeps printing candles after settlement. By then every rung is
    // worth exactly 0 or 1 and most books have emptied, so the crossing lands
    // on whichever rung still quotes — on a settled S&P ladder that read 7575
    // against a 7785.76 close, as the last point of the line and the number the
    // legend reports.
    const series = [
      { market: rung('A', 100), candles: [candle(10, 0.89, 0.91), candle(20, 0.99, 1)] },
      { market: rung('B', 110), candles: [candle(10, 0.49, 0.51), candle(20, 0, 0.01)] },
      { market: rung('C', 120), candles: [candle(10, 0.09, 0.11), candle(20, 0, 0.01)] },
    ];

    assert.deepEqual(
      buildPoints(series, 'median', 15).map((p) => p.time),
      [10],
    );
    // The bucket closing exactly on the bell is the last real one, not the
    // first dead one — the book at that instant is the final pre-settlement
    // book, and it is the most informative point on the whole line.
    assert.deepEqual(
      buildPoints(series, 'median', 20).map((p) => p.time),
      [10, 20],
    );
    // Kalshi gives no close time for a handful of legacy events; those keep the
    // old behaviour rather than losing their series entirely.
    assert.deepEqual(
      buildPoints(series, 'median').map((p) => p.time),
      [10, 20],
    );
  });
});

describe('selectStrikes', () => {
  const ladder = Array.from({ length: 120 }, (_, i) =>
    rung(`K${i}`, 1000 + i * 10, { openInterest: 5 }),
  );

  it('keeps the whole ladder when it is small enough to read entirely', () => {
    const small = ladder.slice(0, 12);
    assert.equal(selectStrikes(small, 1050).length, 12);
  });

  it('keeps the rungs nearest the money when the ladder is too long', () => {
    // Ordering by proximity rather than by volume matters: volume clusters at
    // round numbers, but the crossing is located by its bracketing strikes.
    const selected = selectStrikes(ladder, 1600);
    assert.ok(selected.length < ladder.length);
    const strikes = selected.map((m) => m.floorStrike!);
    assert.ok(Math.min(...strikes) < 1600 && Math.max(...strikes) > 1600);
    assert.ok(
      Math.max(...strikes.map((s) => Math.abs(s - 1600))) <
        Math.max(...ladder.map((m) => Math.abs(m.floorStrike! - 1600))),
    );
  });

  it('prefers rungs that have traded or hold open interest', () => {
    const mixed = [
      ...Array.from({ length: 5 }, (_, i) => rung(`DEAD${i}`, 100 + i)),
      ...Array.from({ length: 5 }, (_, i) => rung(`LIVE${i}`, 200 + i, { volume: 10 })),
    ];
    const selected = selectStrikes(mixed, 202);
    assert.equal(selected.length, 5);
    assert.ok(selected.every((m) => m.ticker.startsWith('LIVE')));
  });

  it('falls back to the whole ladder when nothing has traded yet', () => {
    const cold = Array.from({ length: 6 }, (_, i) => rung(`C${i}`, 100 + i));
    assert.equal(selectStrikes(cold, null).length, 6);
  });
});

describe('findUnderlying', () => {
  it('resolves a canonical symbol', () => {
    assert.equal(findUnderlying('BTC')?.name, 'Bitcoin');
    assert.equal(findUnderlying('btc')?.assetClass, 'crypto');
  });

  it('resolves the aliases people actually type', () => {
    assert.equal(findUnderlying('SPX')?.symbol, '^GSPC');
    assert.equal(findUnderlying('NASDAQ')?.symbol, '^NDX');
    assert.equal(findUnderlying('GOLD')?.symbol, 'GC=F');
    assert.equal(findUnderlying('WTI')?.symbol, 'CL=F');
  });

  it('does not claim a symbol it has no ladder for', () => {
    assert.equal(findUnderlying('AAPL'), undefined);
  });

  it('maps only terminal-value ladders, never running-maximum products', () => {
    // `KXWTIMAX` asks how *high* oil gets, not where it settles. Collapsing it
    // into an implied price would produce a confidently wrong number.
    for (const underlying of [findUnderlying('WTI'), findUnderlying('BTC')]) {
      assert.ok(underlying);
      assert.ok(!underlying.series.some((s) => /MAX|MIN/.test(s)), underlying.series.join(','));
    }
  });
});
