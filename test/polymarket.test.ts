/**
 * Polymarket International normalisation tests.
 *
 * The fixtures are trimmed copies of real `gamma-api.polymarket.com` and
 * `clob.polymarket.com` responses, so the stringified JSON arrays
 * (`"[\"Yes\", \"No\"]"`), the decimal-string book levels and the `{t, p}`
 * price series are exactly what the upstreams send.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  bucketHistory,
  normaliseEvent,
  normaliseMarket,
  normaliseOrderBook,
  normaliseTrades,
  parseStringArray,
  seriesFromEvent,
  seriesFromSlug,
  type RawGammaEvent,
  type RawGammaMarket,
} from '../src/server/sources/polymarket.js';

/* ------------------------------------------------------------------ arrays */

describe('parseStringArray', () => {
  it('parses gamma\'s stringified JSON arrays', () => {
    assert.deepEqual(parseStringArray('["Yes", "No"]'), ['Yes', 'No']);
    assert.deepEqual(parseStringArray('["0.735", "0.265"]'), ['0.735', '0.265']);
  });

  it('accepts a real array, in case the upstream is ever fixed', () => {
    assert.deepEqual(parseStringArray(['Yes', 'No']), ['Yes', 'No']);
  });

  it('returns nothing for absent or unparseable values', () => {
    assert.deepEqual(parseStringArray(undefined), []);
    assert.deepEqual(parseStringArray(''), []);
    assert.deepEqual(parseStringArray('not json'), []);
    assert.deepEqual(parseStringArray('{"a":1}'), []);
  });
});

/* ------------------------------------------------------------------ series */

describe('seriesFromEvent', () => {
  it('prefers the series gamma states', () => {
    assert.equal(seriesFromEvent({ slug: 'fed-decision-in-september-762', seriesSlug: 'fomc' }), 'fomc');
    assert.equal(
      seriesFromEvent({ slug: 'x', series: [{ slug: 'nfl', title: 'NFL' }] }),
      'nfl',
    );
  });

  it('falls back to the slug stem when it states none', () => {
    // Polymarket suffixes a repeat listing with a creation timestamp or a
    // collision counter; neither is part of the series' name.
    assert.equal(seriesFromEvent({ slug: 'fed-decision-in-january-20260729233815502' }), 'fed-decision-in-january');
    assert.equal(seriesFromSlug('fed-decision-in-september-762'), 'fed-decision-in-september');
    assert.equal(seriesFromSlug('nobel-peace-prize-winner'), 'nobel-peace-prize-winner');
  });
});

/* ----------------------------------------------------------------- markets */

describe('normaliseMarket', () => {
  // Real shape, trimmed: one rung of the September FOMC ladder.
  const raw: RawGammaMarket = {
    slug: 'will-there-be-no-change-in-fed-interest-rates-after-the-september-2026-meeting-615',
    question: 'Will there be no change in Fed interest rates after the September 2026 meeting?',
    conditionId: '0xabc',
    groupItemTitle: 'No change',
    outcomes: '["Yes", "No"]',
    outcomePrices: '["0.735", "0.265"]',
    clobTokenIds: '["561528276", "902713122"]',
    bestBid: 0.73,
    bestAsk: 0.74,
    lastTradePrice: 0.74,
    oneDayPriceChange: 0.02,
    volumeNum: 1_234_567.5,
    volume24hr: 77_074.52,
    liquidityNum: 641_943.36,
    startDate: '2026-05-13T21:23:13.517067Z',
    endDate: '2026-09-16T00:00:00Z',
    active: true,
    closed: false,
    acceptingOrders: true,
    events: [{ ticker: 'fed-decision-in-september-762', slug: 'x', seriesSlug: 'fomc' }],
  };

  it('reads the quote and labels the venue', () => {
    const market = normaliseMarket(raw);
    assert.equal(market.venue, 'polymarket');
    assert.equal(market.yesBid, 0.73);
    assert.equal(market.yesAsk, 0.74);
    assert.equal(market.mid, 0.735);
    assert.equal(market.lastPrice, 0.74);
  });

  it('derives the NO side from the YES book', () => {
    const market = normaliseMarket(raw);
    // An offer to sell YES at 0.74 is a bid to buy NO at 0.26 — and the
    // subtraction must not leave 0.26000000000000006 behind.
    assert.equal(market.noBid, 0.26);
    assert.equal(market.noAsk, 0.27);
  });

  it('recovers the previous price from the 24h change', () => {
    const market = normaliseMarket(raw);
    assert.equal(market.change, 0.02);
    assert.equal(market.previousPrice, 0.72);
  });

  it('takes its event and series from the inlined parent', () => {
    const market = normaliseMarket(raw);
    assert.equal(market.eventTicker, 'fed-decision-in-september-762');
    assert.equal(market.seriesTicker, 'fomc');
  });

  it('labels the YES leg with its rung, not the whole question', () => {
    assert.equal(normaliseMarket(raw).yesSubTitle, 'No change');
    // A standalone binary market has no rung, so the outcome name stands in.
    assert.equal(
      normaliseMarket({ ...raw, groupItemTitle: '', outcomes: '["Yes", "No"]' }).yesSubTitle,
      'Yes',
    );
  });

  it('reports an empty book side as absent, not as a zero quote', () => {
    const market = normaliseMarket({ ...raw, bestBid: 0, bestAsk: 0, lastTradePrice: 0 });
    assert.equal(market.yesBid, null);
    assert.equal(market.yesAsk, null);
    assert.equal(market.noBid, null);
    // …but the mark price gamma always publishes still stands in for last.
    assert.equal(market.lastPrice, 0.735);
  });

  it('keeps a price of 1 — a decided market is not an empty one', () => {
    assert.equal(normaliseMarket({ ...raw, bestAsk: 1 }).yesAsk, 1);
  });

  it('leaves open interest unstated rather than zero', () => {
    // Gamma publishes open interest per event only, and an event's figure is
    // not this contract's.
    assert.equal(normaliseMarket(raw).openInterest, null);
  });

  it('states no strike, because Polymarket words them into the question', () => {
    const market = normaliseMarket(raw);
    assert.equal(market.strikeType, null);
    assert.equal(market.floorStrike, null);
    assert.equal(market.capStrike, null);
  });

  it('reads the settled side off the resolved prices, and only when closed', () => {
    assert.equal(normaliseMarket({ ...raw, outcomePrices: '["1", "0"]' }).result, '');
    assert.equal(
      normaliseMarket({ ...raw, closed: true, outcomePrices: '["1", "0"]' }).result,
      'yes',
    );
    assert.equal(
      normaliseMarket({ ...raw, closed: true, outcomePrices: '["0", "1"]' }).result,
      'no',
    );
  });

  it('maps the activity flags onto a status', () => {
    assert.equal(normaliseMarket(raw).status, 'open');
    assert.equal(normaliseMarket({ ...raw, acceptingOrders: false }).status, 'closed');
    assert.equal(normaliseMarket({ ...raw, active: false }).status, 'unopened');
    assert.equal(normaliseMarket({ ...raw, closed: true }).status, 'settled');
  });
});

describe('normaliseEvent', () => {
  const raw: RawGammaEvent = {
    ticker: 'fed-decision-in-september-762',
    title: 'Fed Decision in September?',
    seriesSlug: 'fomc',
    series: [{ slug: 'fomc', title: 'FOMC' }],
    negRisk: true,
    tags: [
      { id: '100478', label: 'fomc' },
      { id: '2', label: 'Politics' },
      { id: '100328', label: 'Economy' },
    ],
    markets: [{ slug: 'a', question: 'A?', groupItemTitle: 'No change', bestBid: 0.7, bestAsk: 0.71 }],
  };

  it('takes the broadest tag as the category, not the first one', () => {
    // Tag ids are issued in sequence, so the lowest is the top-level label:
    // `Politics` is 2 and `fomc` is 100478, and gamma lists them in neither order.
    assert.equal(normaliseEvent(raw).category, 'Politics');
  });

  it('reads negRisk as mutual exclusivity', () => {
    assert.equal(normaliseEvent(raw).mutuallyExclusive, true);
    assert.equal(normaliseEvent({ ...raw, negRisk: false }).mutuallyExclusive, false);
  });

  it('stamps its own event and series onto every nested market', () => {
    const event = normaliseEvent(raw);
    assert.equal(event.markets[0]!.eventTicker, 'fed-decision-in-september-762');
    assert.equal(event.markets[0]!.seriesTicker, 'fomc');
    assert.equal(event.markets[0]!.venue, 'polymarket');
  });
});

/* -------------------------------------------------------------------- book */

describe('normaliseOrderBook', () => {
  // The CLOB returns both sides ascending, as decimal strings.
  const raw = {
    bids: [
      { price: '0.700', size: '300' },
      { price: '0.720', size: '150' },
      { price: '0.730', size: '7966.95' },
    ],
    asks: [
      { price: '0.790', size: '40' },
      { price: '0.760', size: '900' },
      { price: '0.740', size: '5000' },
    ],
  };

  it('sorts bids best-first and asks cheapest-first', () => {
    const book = normaliseOrderBook(raw, 'slug');
    assert.deepEqual(book.yes.map((l) => l.price), [0.73, 0.72, 0.7]);
    assert.deepEqual(book.yesAsks.map((l) => l.price), [0.74, 0.76, 0.79]);
  });

  it('derives the NO ladder from the YES offers', () => {
    const book = normaliseOrderBook(raw, 'slug');
    // Selling YES at 0.74 is buying NO at 0.26; best NO bid is the highest.
    assert.deepEqual(book.no.map((l) => l.price), [0.26, 0.24, 0.21]);
    assert.equal(book.no[0]!.size, 5000);
  });

  it('derives best bid, best ask, spread and mid', () => {
    const book = normaliseOrderBook(raw, 'slug');
    assert.equal(book.bestYesBid, 0.73);
    assert.equal(book.bestYesAsk, 0.74);
    assert.equal(book.spread, 0.01);
    assert.equal(book.mid, 0.735);
    assert.equal(book.venue, 'polymarket');
  });

  it('reports nulls rather than a fake quote for an empty book', () => {
    const book = normaliseOrderBook({}, 'slug');
    assert.deepEqual(book.yes, []);
    assert.equal(book.bestYesBid, null);
    assert.equal(book.spread, null);
  });

  it('honours the depth cap', () => {
    const rows = Array.from({ length: 40 }, (_, i) => ({ price: `0.${i + 10}`, size: '5' }));
    assert.equal(normaliseOrderBook({ bids: rows }, 'slug', 5).yes.length, 5);
  });
});

/* ------------------------------------------------------------------ trades */

describe('normaliseTrades', () => {
  it('restates a NO-token print in YES terms', () => {
    const { trades } = normaliseTrades(
      [
        { transactionHash: '0x1', timestamp: 1786900389, price: 0.26, size: 5, side: 'BUY', outcomeIndex: 1 },
      ],
      'slug',
    );
    // Buying NO at 26¢ is the same print as selling YES at 74¢.
    assert.equal(trades[0]!.yesPrice, 0.74);
    assert.equal(trades[0]!.noPrice, 0.26);
    assert.equal(trades[0]!.takerSide, 'no');
  });

  it('reads a YES-token buy as a YES taker', () => {
    const { trades } = normaliseTrades(
      [{ transactionHash: '0x2', timestamp: 1, price: 0.74, size: 5, side: 'BUY', outcomeIndex: 0 }],
      'slug',
    );
    assert.equal(trades[0]!.yesPrice, 0.74);
    assert.equal(trades[0]!.takerSide, 'yes');
  });

  it('reads a YES-token sell as a NO taker', () => {
    const { trades } = normaliseTrades(
      [{ transactionHash: '0x3', timestamp: 1, price: 0.73, size: 9.99, side: 'SELL', outcomeIndex: 0 }],
      'slug',
    );
    assert.equal(trades[0]!.takerSide, 'no');
    assert.equal(trades[0]!.count, 9.99);
  });

  it('keeps sibling fills of one transaction distinct', () => {
    const { trades } = normaliseTrades(
      [
        { transactionHash: '0x9', timestamp: 1, price: 0.7, size: 1, side: 'BUY', outcomeIndex: 0 },
        { transactionHash: '0x9', timestamp: 1, price: 0.71, size: 2, side: 'BUY', outcomeIndex: 0 },
      ],
      'slug',
    );
    assert.notEqual(trades[0]!.tradeId, trades[1]!.tradeId);
  });
});

/* ----------------------------------------------------------------- candles */

describe('bucketHistory', () => {
  const HOUR = 3600;

  it('buckets samples into OHLC on the period end', () => {
    const [candle] = bucketHistory(
      [
        { t: HOUR * 10 + 60, p: 0.5 },
        { t: HOUR * 10 + 600, p: 0.6 },
        { t: HOUR * 10 + 1200, p: 0.4 },
        { t: HOUR * 10 + 3000, p: 0.55 },
      ],
      60,
    );
    assert.equal(candle!.time, HOUR * 11);
    assert.equal(candle!.open, 0.5);
    assert.equal(candle!.high, 0.6);
    assert.equal(candle!.low, 0.4);
    assert.equal(candle!.close, 0.55);
    assert.equal(candle!.traded, true);
  });

  it('leaves volume and open interest unstated', () => {
    // The upstream is a price series with no size attached to any point;
    // drawing a zero-volume bar would assert something it never said.
    const [candle] = bucketHistory([{ t: HOUR, p: 0.5 }], 60);
    assert.equal(candle!.volume, null);
    assert.equal(candle!.openInterest, null);
  });

  it('carries the last close across a quiet stretch', () => {
    const candles = bucketHistory(
      [
        { t: HOUR * 1 + 10, p: 0.5 },
        { t: HOUR * 4 + 10, p: 0.8 },
      ],
      60,
    );
    // Two observed buckets, two hours of hold in between — a continuous line,
    // not a jump.
    assert.equal(candles.length, 4);
    assert.equal(candles[1]!.close, 0.5);
    assert.equal(candles[1]!.traded, false);
    assert.equal(candles[2]!.traded, false);
    assert.equal(candles[3]!.close, 0.8);
    assert.equal(candles[3]!.traded, true);
  });

  it('sorts ascending, as lightweight-charts requires', () => {
    const candles = bucketHistory(
      [
        { t: HOUR * 5, p: 0.2 },
        { t: HOUR * 2, p: 0.1 },
      ],
      60,
    );
    assert.ok(candles[0]!.time < candles.at(-1)!.time);
  });

  it('returns nothing for an empty history', () => {
    assert.deepEqual(bucketHistory([], 60), []);
  });

  it('skips malformed points instead of emitting NaN bars', () => {
    const candles = bucketHistory(
      [
        { t: HOUR, p: Number.NaN },
        { t: HOUR, p: 0.3 },
      ],
      60,
    );
    assert.equal(candles.length, 1);
    assert.equal(candles[0]!.close, 0.3);
  });
});
