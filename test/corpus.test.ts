/**
 * Ranking and search over a venue snapshot.
 *
 * `rankMarkets` had no test, and shipped inverted: `TOP volume` returned the
 * quietest markets on the exchange rather than the busiest. Nothing downstream
 * caught it, because the panel re-sorts whatever it is handed — so the thirty
 * wrong markets arrived in a convincingly correct order. These tests assert the
 * direction of every board, which is the part that was wrong and the part no
 * amount of eyeballing the panel would have revealed.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  rankMarkets,
  searchCorpus,
  sumOrNull,
  type Corpus,
} from '../src/server/sources/corpus.js';
import type { Market, VenueEvent } from '../src/shared/types.js';

/** A market carrying only the figures a leaderboard reads. */
function market(ticker: string, fields: Partial<Market> = {}): Market {
  return {
    venue: 'kalshi',
    ticker,
    eventTicker: '',
    seriesTicker: '',
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
    volume: null,
    volume24h: null,
    openInterest: null,
    liquidity: null,
    openTime: '',
    closeTime: '',
    expirationTime: '',
    result: '',
    rulesPrimary: '',
    strikeType: null,
    floorStrike: null,
    capStrike: null,
    ...fields,
  };
}

const tickers = (markets: Market[]): string[] => markets.map((m) => m.ticker);

/* ---------------------------------------------------------------- ranking */

describe('rankMarkets', () => {
  const book = [
    market('QUIET', { volume24h: 10, openInterest: 10, liquidity: 10, change: -0.05 }),
    market('BUSY', { volume24h: 30, openInterest: 30, liquidity: 30, change: 0.09 }),
    market('MIDDLING', { volume24h: 20, openInterest: 20, liquidity: 20, change: 0.02 }),
  ];

  it('puts the busiest market at the top of the volume board', () => {
    assert.deepEqual(tickers(rankMarkets(book, 'volume')), ['BUSY', 'MIDDLING', 'QUIET']);
  });

  it('ranks open interest and resting depth the same way round', () => {
    assert.deepEqual(tickers(rankMarkets(book, 'open_interest')), ['BUSY', 'MIDDLING', 'QUIET']);
    assert.deepEqual(tickers(rankMarkets(book, 'liquidity')), ['BUSY', 'MIDDLING', 'QUIET']);
  });

  it('leads the gainers with the biggest rise, not the biggest fall', () => {
    assert.deepEqual(tickers(rankMarkets(book, 'gainers')), ['BUSY', 'MIDDLING', 'QUIET']);
  });

  it('leads the losers with the most negative move', () => {
    assert.deepEqual(tickers(rankMarkets(book, 'losers')), ['QUIET', 'MIDDLING', 'BUSY']);
  });

  it('takes the top of the board, not the bottom, when limited', () => {
    // The bug this pins down: `limit` slices after the sort, so an inverted
    // comparator does not merely reorder the answer, it returns other markets.
    assert.deepEqual(tickers(rankMarkets(book, 'volume', 1)), ['BUSY']);
    assert.deepEqual(tickers(rankMarkets(book, 'losers', 1)), ['QUIET']);
  });

  it('leaves out a market whose venue does not publish the sorted figure', () => {
    const mixed = [...book, market('UNPUBLISHED', { volume24h: null })];
    assert.deepEqual(tickers(rankMarkets(mixed, 'volume')), ['BUSY', 'MIDDLING', 'QUIET']);
  });

  it('keeps a stated zero on the board — a dead market is a fact, not a gap', () => {
    const withZero = [...book, market('DEAD', { volume24h: 0 })];
    assert.deepEqual(tickers(rankMarkets(withZero, 'volume')), [
      'BUSY',
      'MIDDLING',
      'QUIET',
      'DEAD',
    ]);
  });

  it('drops a mover with no turnover behind it, because that is a stale print', () => {
    const stale = [...book, market('STALE', { change: 0.4, volume24h: 0 })];
    assert.deepEqual(tickers(rankMarkets(stale, 'gainers')), ['BUSY', 'MIDDLING', 'QUIET']);
  });
});

/* ------------------------------------------------------------------- sums */

describe('sumOrNull', () => {
  it('reports null when no member published the figure', () => {
    assert.equal(sumOrNull([market('A'), market('B')], (m) => m.volume24h), null);
  });

  it('sums the members that did, ignoring the ones that did not', () => {
    const markets = [market('A', { volume24h: 5 }), market('B'), market('C', { volume24h: 7 })];
    assert.equal(sumOrNull(markets, (m) => m.volume24h), 12);
  });

  it('keeps a stated zero, which is not the same as unpublished', () => {
    assert.equal(sumOrNull([market('A', { volume24h: 0 })], (m) => m.volume24h), 0);
  });
});

/* ----------------------------------------------------------------- search */

function event(eventTicker: string, title: string, markets: Market[] = []): VenueEvent {
  return {
    venue: 'kalshi',
    eventTicker,
    seriesTicker: '',
    title,
    subTitle: '',
    category: '',
    mutuallyExclusive: false,
    markets,
  };
}

function corpus(events: VenueEvent[]): Corpus {
  return {
    venue: 'kalshi',
    events,
    markets: events.flatMap((e) => e.markets),
    builtAt: Date.now(),
    truncated: false,
  };
}

describe('searchCorpus', () => {
  const snapshot = corpus([
    event('KXFEDDECISION-26OCT', 'Fed decision in Oct 2026?', [
      market('KXFEDDECISION-26OCT-T3.75', { volume24h: 100, yesSubTitle: 'Cut 25bps' }),
    ]),
    event('KXHIGHNY-26AUG18', 'Highest temperature in NYC today?', [
      market('KXHIGHNY-26AUG18-B80', { volume24h: 5, yesSubTitle: '80° or above' }),
    ]),
  ]);

  it('requires every term to appear, so an extra word narrows rather than widens', () => {
    assert.equal(searchCorpus(snapshot, 'fed').hits.length, 1);
    assert.equal(searchCorpus(snapshot, 'fed temperature').hits.length, 0);
  });

  it('matches a strike label, not only the event title', () => {
    assert.equal(searchCorpus(snapshot, '25bps').hits[0]?.event.eventTicker, 'KXFEDDECISION-26OCT');
  });

  it('treats a term with regex punctuation as literal text', () => {
    // The query reaches a `RegExp`, so an unescaped `?` or `(` would throw.
    assert.doesNotThrow(() => searchCorpus(snapshot, 'decision?'));
    assert.equal(searchCorpus(snapshot, 'nyc?').hits.length, 0);
  });

  it('reports how many events were scanned, so a miss can be told from an empty book', () => {
    assert.equal(searchCorpus(snapshot, 'nothing').scanned, 2);
  });
});
