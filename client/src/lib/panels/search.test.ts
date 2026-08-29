/**
 * What `SRCH` says when a venue does not answer.
 *
 * The case these exist for is the one the panel got wrong: every venue
 * unreachable, nothing searched, and a body that read "Nothing open matches
 * … Try fewer or broader words." A reader following that advice rewrites the
 * query forever, because the query was never the problem.
 */

import { describe, expect, it } from 'vitest';

import type { EventSearchHit, Venue } from '$gen';

import { emptyBody, note, partial, rank, type VenueResult } from './search';

const answered = (venue: Venue, scanned: number, hits: EventSearchHit[] = []): VenueResult => ({
  venue,
  response: { query: 'fed decision', hits, scanned, snapshotAgeSeconds: 12, truncated: false },
  error: null,
});

const down = (venue: Venue, message: string, hint?: string): VenueResult => ({
  venue,
  response: null,
  error: { message, ...(hint ? { hint } : {}) },
});

const hit = (ticker: string, matched: number, total: number, score: number): EventSearchHit => ({
  event: {
    venue: 'kalshi',
    eventTicker: ticker,
    seriesTicker: 'S',
    title: ticker,
    subTitle: '',
    category: '',
    mutuallyExclusive: true,
  },
  markets: [],
  volume24h: null,
  score,
  matchedTerms: matched,
  totalTerms: total,
});

const text = (results: VenueResult[]): string =>
  note(results)
    .map((segment) => segment.text)
    .join('');

describe('note', () => {
  it('names a venue that did not answer with the reason it gave', () => {
    const line = text([
      answered('kalshi', 4000),
      down('polymarket', 'gamma-api.polymarket.com refused the connection'),
    ]);

    expect(line).toContain('PM gamma-api.polymarket.com refused the connection');
    // The word that replaced six different faults with one shrug.
    expect(line).not.toContain('unavailable');
  });

  it('carries the upstream hint once, however many venues share it', () => {
    const reason = 'Cannot reach the terminal API server';
    const advice = 'Is the API server running? `npm run dev` starts both halves.';
    const line = text([
      down('kalshi', reason, advice),
      down('polymarket', reason, advice),
      down('gemini', reason, advice),
    ]);

    expect(line.split(advice)).toHaveLength(2);
  });

  it('reports a snapshot age only when something answered', () => {
    expect(text([answered('kalshi', 10)])).toContain('snapshot 12s old');
    expect(text([down('kalshi', 'nope')])).not.toContain('snapshot');
  });
});

describe('emptyBody', () => {
  it('does not report an outage as a query that matched nothing', () => {
    const body = emptyBody('fed decision', [
      down('kalshi', 'api.elections.kalshi.com did not respond in time'),
      down('polymarket', 'gamma-api.polymarket.com refused the connection'),
    ]);

    expect(body.kind).toBe('outage');
    expect(body.message).toBe('No venue answered, so nothing was searched.');
    // Both of the sentences that sent a reader to rewrite a query that never ran.
    expect(JSON.stringify(body)).not.toContain('Nothing open matches');
    expect(JSON.stringify(body)).not.toContain('Try fewer or broader words');
  });

  it('lists every reason, and the first hint, when nothing answered', () => {
    const body = emptyBody('fed', [
      down('kalshi', 'Cannot reach the terminal API server', 'Is the API server running?'),
      down('polymarket', 'Cannot reach the terminal API server'),
    ]);

    expect(body).toMatchObject({
      kind: 'outage',
      reasons: [
        'KAL — Cannot reach the terminal API server',
        'PM — Cannot reach the terminal API server',
      ],
      hint: 'Is the API server running?',
    });
  });

  it('names the single venue when only one was asked', () => {
    const body = emptyBody('fed', [down('gemini', 'www.gemini.com refused the connection')]);
    expect(body.message).toBe('Gemini did not answer, so nothing was searched.');
  });

  it('says the book is short when only some venues answered', () => {
    const body = emptyBody('fed', [answered('kalshi', 4000), down('polymarket', 'nope')]);

    expect(body.kind).toBe('miss');
    expect(body.hint).toContain('the 1 of 2 venues that answered');
    expect(body.hint).toContain('PM did not, so this is not the whole book');
  });

  it('keeps the plain advice when every venue answered and nothing matched', () => {
    const body = emptyBody('zzz', [answered('kalshi', 4000), answered('polymarket', 2000)]);

    expect(body).toEqual({
      kind: 'miss',
      message: 'Nothing open matches "zzz".',
      hint: 'Searched 6,000 open events across 2 venues. Try fewer or broader words.',
    });
  });
});

describe('rank', () => {
  it('puts a full match above a partial one that scored higher', () => {
    const ranked = rank([
      answered('kalshi', 1, [hit('PARTIAL', 1, 2, 90)]),
      answered('polymarket', 1, [hit('FULL', 2, 2, 40)]),
    ]);

    expect(ranked.map((entry) => entry.hit.event.eventTicker)).toEqual(['FULL', 'PARTIAL']);
  });
});

describe('partial', () => {
  it('labels a hit that answered only part of the query', () => {
    expect(partial(hit('A', 2, 3, 0))).toBe('2/3 words');
    expect(partial(hit('A', 3, 3, 0))).toBeNull();
    // The empty query asks for nothing, so nothing is missing from the answer.
    expect(partial(hit('A', 0, 0, 0))).toBeNull();
  });
});
