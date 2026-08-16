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
import type { KalshiEvent, Market } from '../src/shared/types.js';
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

  /**
   * The award tickers are the densest prefix family in the table, and the
   * collisions are not obvious: `KXOSCARSUPACTO` starts with `KXOSCARS`, so a
   * rule keyed on that would quietly claim every supporting-actor market for
   * Original Screenplay. Longest-prefix resolution is what prevents it, and
   * these are the pairs that would silently swap.
   */
  it('resolves each award category to its own AWRD lookup', () => {
    assert.equal(feedFor('KXOSCARPIC-27')?.command, 'AWRD best picture');
    assert.equal(feedFor('KXOSCARDIR')?.command, 'AWRD best director');
    assert.equal(feedFor('KXOSCARACTO')?.command, 'AWRD best actor');
    assert.equal(feedFor('KXOSCARACTR')?.command, 'AWRD best actress');
    assert.equal(feedFor('KXOSCARSUPACTO')?.command, 'AWRD supporting actor');
    assert.equal(feedFor('KXOSCARSUPACTR')?.command, 'AWRD supporting actress');
    assert.equal(feedFor('KXOSCARSPLAY')?.command, 'AWRD original screenplay');
    assert.equal(feedFor('KXOSCARASPLAY')?.command, 'AWRD adapted screenplay');
    assert.equal(feedFor('KXGAMEAWARDS')?.command, 'AWRD game of the year');
    assert.equal(feedFor('KXEMMYCSERIES')?.command, 'AWRD comedy series');
    assert.equal(feedFor('KXEMMYDSERIES')?.command, 'AWRD drama series');
  });

  it('keeps a nominee series on the same category as its winner series', () => {
    assert.equal(feedFor('KXOSCARNOMPIC')?.command, feedFor('KXOSCARPIC')?.command);
    assert.equal(feedFor('KXOSCARNOMSUPACTR')?.command, feedFor('KXOSCARSUPACTR')?.command);
  });

  it('falls back to the family rule for a category the table has not seen', () => {
    assert.equal(feedFor('KXOSCARSOMETHINGNEW')?.command, 'AWRD best picture');
    assert.equal(feedFor('KXEMMYSOMETHINGNEW')?.command, 'AWRD drama series');
  });

  it('points the newer feeds at their own commands', () => {
    assert.equal(feedFor('KXRANKLISTGOOGLESEARCHTOP5')?.command, 'TRND');
    assert.equal(feedFor('KXALBUMRELEASEDATEBEY')?.command, 'REL');
    assert.equal(feedFor('KXNEWTAYLOR-TS')?.command, 'REL taylor swift');
    assert.equal(feedFor('KXTOPPOD')?.command, 'POD');
    assert.equal(feedFor('KXROGANGUEST')?.command, 'POD episodes');
  });

  /**
   * A debut-position market settles on the Billboard chart, not on the release
   * feed, even though its ticker sits in the `KXALBUM*` family — the mapping
   * follows each series' own `settlement_sources`, not the ticker's shape.
   */
  it('keeps chart-position series on Billboard rather than the release feed', () => {
    assert.equal(feedFor('KXALBUMDEBUT')?.command, 'BB billboard-200');
    assert.equal(feedFor('KXALBUMVS')?.command, 'BB billboard-200');
    assert.equal(feedFor('KXALBUMEQUIV')?.command, 'BB billboard-200');
  });

  /**
   * Apple's Search API no longer returns films, so `REL` is music-only and the
   * film release-date series stay unmapped on purpose. A feed that opens a panel
   * which cannot answer is worse than no feed column at all.
   */
  it('leaves film release-date series unmapped', () => {
    assert.equal(feedFor('KXMOVIERELEASEDATE'), undefined);
    assert.equal(feedFor('KXMEDIARELEASEST'), undefined);
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

const EVENT: KalshiEvent = {
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
