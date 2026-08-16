/**
 * Entertainment genre classification.
 *
 * The classifier is the whole value of the ENT command, and its failure mode is
 * silent: an Oscar market filed under `music` still renders perfectly, it is
 * just in the wrong list. So the cases asserted here are the ones where the
 * ticker table could plausibly be read the wrong way — overlapping prefixes,
 * multi-genre series, and the title fallback.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import type { Market, VenueEvent } from '../src/shared/types.js';
import {
  assertGenre,
  classify,
  feedFor,
  toEntEvent,
} from '../src/server/sources/entertainment.js';

describe('classify', () => {
  it('files the core music series under music', () => {
    for (const ticker of ['KXTOPARTIST', 'KXARTISTSTREAMSY', 'KX1ALBUM', 'KXBILLBOARDRUNNERUPSONG']) {
      assert.deepEqual(classify(ticker, ''), ['music'], ticker);
    }
  });

  it('files Rotten Tomatoes and casting series under film', () => {
    assert.deepEqual(classify('KXRT', ''), ['film']);
    assert.deepEqual(classify('KXROLEINPRODUCTIONDOOMSDAY', ''), ['film']);
  });

  it('files Netflix and reality series under tv', () => {
    assert.deepEqual(classify('KXNETFLIXRANKSHOW', ''), ['tv']);
    assert.deepEqual(classify('KXBIGBROTHERELIMINATION', ''), ['tv']);
  });

  it('tags award series with both their medium and awards', () => {
    assert.deepEqual(classify('KXOSCARPIC', ''), ['film', 'awards']);
    assert.deepEqual(classify('KXEMMYCSERIES', ''), ['tv', 'awards']);
    assert.deepEqual(classify('KXGRAMAOTY', ''), ['music', 'awards']);
  });

  it('prefers the longest matching prefix', () => {
    // `KXGAMEAWARDS` also starts with `KXGAME`; stopping at the shorter rule
    // would drop the awards tag and hide it from `ENT awards`.
    assert.deepEqual(classify('KXGAMEAWARDS', ''), ['games', 'awards']);
    assert.deepEqual(classify('KXGAMERELEASE', ''), ['games']);
  });

  it('falls back to the title when the ticker is unknown', () => {
    assert.deepEqual(classify('ZZUNKNOWN', 'Who will win Album of the Year'), ['music']);
    assert.ok(classify('ZZUNKNOWN', 'Rotten Tomatoes score for the film').includes('film'));
  });

  it('never returns an empty tag list', () => {
    // Everything under Kalshi's Entertainment category belongs somewhere; an
    // unclassifiable event must still be listable rather than silently dropped.
    assert.deepEqual(classify('ZZZ', 'something entirely unparseable'), ['celeb']);
  });

  it('is case-insensitive on the ticker', () => {
    assert.deepEqual(classify('kxoscarpic', ''), ['film', 'awards']);
  });
});

describe('feedFor', () => {
  it('points each series at the command that shows its settlement data', () => {
    assert.equal(feedFor('KXRT')?.command, 'RT');
    assert.equal(feedFor('KXNETFLIXRANKSHOW')?.command, 'NFLX');
    assert.equal(feedFor('KXARTISTSTREAMSY')?.command, 'SPOT');
    assert.equal(feedFor('KXYTVIEWSW')?.command, 'YT');
    assert.equal(feedFor('KXSTEAMGOTY')?.command, 'STEAM');
  });

  it('resolves Billboard series to the right chart', () => {
    assert.equal(feedFor('KXTOPSONG')?.command, 'BB hot-100');
    assert.equal(feedFor('KXTOPALBUM')?.command, 'BB billboard-200');
  });

  it('returns undefined for a series with no terminal feed', () => {
    assert.equal(feedFor('KXSEXYMAN'), undefined);
  });
});

describe('assertGenre', () => {
  it('accepts the known genres and normalises case', () => {
    assert.equal(assertGenre('MUSIC'), 'music');
    assert.equal(assertGenre('games'), 'games');
  });

  it('treats empty and ALL as everything', () => {
    assert.equal(assertGenre(''), 'all');
    assert.equal(assertGenre('ALL'), 'all');
  });

  it('rejects anything else with a usable hint', () => {
    assert.throws(() => assertGenre('sports'), /not an entertainment genre/);
  });
});

/* ------------------------------------------------------------- toEntEvent */

function market(overrides: Partial<Market>): Market {
  return {
    ticker: 'X',
    eventTicker: 'E',
    seriesTicker: 'S',
    title: 't',
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
    ...overrides,
  };
}

const EVENT: VenueEvent = {
  eventTicker: 'KXRT-DUNE',
  seriesTicker: 'KXRT',
  title: 'Dune: Part Three · Rotten Tomatoes score',
  subTitle: '',
  category: 'Entertainment',
  mutuallyExclusive: true,
  markets: [
    market({ ticker: 'A', volume24h: 10, openInterest: 5, closeTime: '2026-12-20T00:00:00Z' }),
    market({ ticker: 'B', volume24h: 90, openInterest: 15, closeTime: '2026-09-01T00:00:00Z' }),
  ],
};

describe('toEntEvent', () => {
  it('sorts markets most liquid first', () => {
    assert.deepEqual(
      toEntEvent(EVENT).markets.map((m) => m.ticker),
      ['B', 'A'],
    );
  });

  it('sums volume and open interest across the event', () => {
    const entEvent = toEntEvent(EVENT);
    assert.equal(entEvent.volume24h, 100);
    assert.equal(entEvent.openInterest, 20);
  });

  it('reports the soonest close, not an arbitrary one', () => {
    // The nearest deadline is the one a trader is racing.
    assert.equal(toEntEvent(EVENT).closeTime, '2026-09-01T00:00:00Z');
  });

  it('attaches the settlement feed for the series', () => {
    assert.equal(toEntEvent(EVENT).feed?.source, 'Rotten Tomatoes');
  });

  it('omits the feed entirely when there is none', () => {
    const orphan = { ...EVENT, seriesTicker: 'KXSEXYMAN', eventTicker: 'KXSEXYMAN-26' };
    assert.equal(toEntEvent(orphan).feed, undefined);
  });

  it('survives an event with no markets', () => {
    const empty = toEntEvent({ ...EVENT, markets: [] });
    assert.equal(empty.volume24h, 0);
    assert.equal(empty.closeTime, '');
  });
});
