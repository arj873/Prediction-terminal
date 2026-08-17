/**
 * Regression tests for the audit findings.
 *
 * Each of these encodes a defect that was live and reproducible, so the test is
 * written against the exploit rather than against the fix: what a caller could
 * do, and what the server must now answer. A fix whose test only asserts the
 * new code path is one refactor away from being silently undone.
 *
 * The SSRF allowlist on the artwork proxy is covered in `billboard.test.ts`,
 * where it already was.
 */

import assert from 'node:assert/strict';
import { after, describe, it } from 'node:test';
import type { AddressInfo } from 'node:net';
import type { Market, VenueEvent } from '../src/shared/types.js';
import { isValidIdentifier } from '../src/shared/venue.js';
import { createApp } from '../src/server/index.js';
import { TtlCache, catalogue, cache, keyPart, TTL } from '../src/server/lib/cache.js';
import {
  MAX_QUERY_LENGTH,
  MAX_QUERY_TERMS,
  queryTerms,
  searchCorpus,
  type Corpus,
} from '../src/server/sources/corpus.js';
import { assertTicker } from '../src/server/sources/kalshi.js';
import { assertSlug } from '../src/server/sources/polymarketus.js';

/* ------------------------------------------------------------------ fixtures */

function market(i: number): Market {
  return {
    venue: 'kalshi',
    ticker: `KXTEST-${i}`,
    eventTicker: `KXTEST-${i}`,
    seriesTicker: 'KXTEST',
    title: `Test market ${i}`,
    yesSubTitle: `above ${i} dollars a share today`,
    noSubTitle: '',
    status: 'open',
    marketType: 'binary',
    yesBid: 0.5,
    yesAsk: 0.51,
    noBid: 0.49,
    noAsk: 0.5,
    mid: 0.505,
    lastPrice: 0.5,
    previousPrice: 0.5,
    change: 0,
    volume: 10,
    volume24h: 10,
    openInterest: 5,
    liquidity: 100,
    openTime: '',
    closeTime: '',
    expirationTime: '',
    result: '',
    rulesPrimary: '',
    strikeType: null,
    floorStrike: null,
    capStrike: null,
  } as Market;
}

function corpusOf(count: number): Corpus {
  const events: VenueEvent[] = [];
  for (let i = 0; i < count; i++) {
    events.push({
      venue: 'kalshi',
      eventTicker: `KXTEST-${i}`,
      seriesTicker: 'KXTEST',
      title: `A market about a topic number ${i}`,
      subTitle: 'a sub title with a few words in it',
      category: 'Economics',
      mutuallyExclusive: false,
      markets: [market(i)],
    } as VenueEvent);
  }
  return { venue: 'kalshi', events, markets: [], builtAt: Date.now(), truncated: false };
}

/* ------------------------------------------------------- 1. search-term DoS */

describe('search query ceilings', () => {
  it('refuses a query with more terms than it will match', () => {
    // The attack: repeat a term that matches everything, so the `break` that
    // normally ends the scan early never fires. 4,000 of these against a
    // 10,000-event corpus held the event loop for 17.8 seconds.
    assert.throws(
      () => queryTerms(Array(MAX_QUERY_TERMS + 1).fill('a').join(' ')),
      /too many words/,
    );
  });

  it('refuses a query longer than the cap', () => {
    assert.throws(() => queryTerms('x'.repeat(MAX_QUERY_LENGTH + 1)), /too long/);
  });

  it('accepts a query at exactly the ceilings', () => {
    assert.equal(queryTerms(Array(MAX_QUERY_TERMS).fill('a').join(' ')).length, MAX_QUERY_TERMS);
    assert.equal(queryTerms('x'.repeat(MAX_QUERY_LENGTH)).length, 1);
  });

  it('leaves an ordinary search untouched', () => {
    assert.deepEqual(queryTerms('  Fed   rate CUT '), ['fed', 'rate', 'cut']);
    assert.deepEqual(queryTerms(''), []);
  });

  it('answers the worst permitted query well inside a request budget', () => {
    const started = performance.now();
    searchCorpus(corpusOf(10_000), Array(MAX_QUERY_TERMS).fill('a').join(' '), 25);
    const elapsed = performance.now() - started;
    // Generous: the point is orders of magnitude, not a stopwatch. The same
    // shape of query used to be bounded only by the URL length.
    assert.ok(elapsed < 3_000, `worst permitted query took ${elapsed.toFixed(0)}ms`);
  });

  it('scores exactly as it did before the regex was hoisted', () => {
    // The optimisation moved the per-term regex out of the per-event loop. That
    // is only safe if it changed nothing, so this scores the same corpus with
    // the original algorithm — transcribed, regex construction and all — and
    // demands the two agree event for event, point for point.
    const escapeRegExp = (s: string): string => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

    const original = (snapshot: Corpus, query: string): [string, number][] => {
      const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
      const out: [string, number][] = [];

      for (const event of snapshot.events) {
        const ticker = event.eventTicker.toLowerCase();
        const title = event.title.toLowerCase();
        const strikes = event.markets.map((m) => m.yesSubTitle).join(' ').toLowerCase();
        const haystack = `${ticker} ${title} ${event.subTitle.toLowerCase()} ${event.category.toLowerCase()} ${strikes}`;

        let score = 0;
        let matchedAll = true;
        for (const term of terms) {
          if (!haystack.includes(term)) {
            matchedAll = false;
            break;
          }
          score += 10;
          if (ticker.includes(term)) score += 12;
          if (title.startsWith(term)) score += 8;
          if (new RegExp(`\\b${escapeRegExp(term)}`).test(haystack)) score += 6;
        }
        if (!matchedAll) continue;

        if (terms.length > 1 && haystack.includes(terms.join(' '))) score += 25;
        const volume = event.markets.reduce((sum, m) => sum + (m.volume24h ?? 0), 0);
        if (volume > 0) score += 5;

        out.push([event.eventTicker, score]);
      }
      out.sort((a, b) => b[1] - a[1]);
      return out;
    };

    const snapshot = corpusOf(200);

    for (const query of [
      'topic',
      'topic 7',
      'a market about',
      'KXTEST',
      'economics dollars',
      'above 42 dollars',
      'nonexistentword',
      'topic nonexistentword',
      'c++', // regex metacharacters must still be escaped, not interpreted
    ]) {
      const now = searchCorpus(snapshot, query, 1_000).hits.map(
        (h): [string, number] => [h.event.eventTicker, h.score],
      );
      const then = original(snapshot, query);

      assert.equal(now.length, then.length, `hit count changed for "${query}"`);
      assert.deepEqual(
        new Map(now),
        new Map(then),
        `scores changed for "${query}"`,
      );
    }
  });
});

/* --------------------------------------------- 3. cache eviction, key bounds */

describe('cache partitioning', () => {
  it('keeps catalogue snapshots out of the per-query eviction queue', async () => {
    // The attack: 1,200 requests varying one unvalidated parameter evicted the
    // fifteen-second corpus crawl, so every reader paid the cold start again.
    await catalogue.cached('test:corpus', TTL.catalogue, async () => ({ crawl: 'expensive' }));
    for (let i = 0; i < 5_000; i++) {
      cache.set(`kalshi:/markets?cursor=${i}`, { markets: [] }, TTL.quote);
    }
    assert.notEqual(catalogue.get('test:corpus'), undefined);
    catalogue.delete('test:corpus');
  });

  it('bounds a user-supplied key fragment without collapsing distinct queries', () => {
    assert.equal(keyPart('fed rate cut'), 'fed rate cut');

    const long = keyPart('x'.repeat(5_000));
    assert.ok(long.length < 200, `key fragment was ${long.length} characters`);
    // The length is kept in the key, so two different over-long queries that
    // share a prefix do not silently become one entry.
    assert.notEqual(keyPart('y'.repeat(5_000)), keyPart('y'.repeat(4_000)));
  });

  it('still evicts least-recently-used within a pool', () => {
    const small = new TtlCache(2);
    small.set('a', 1, 60_000);
    small.set('b', 2, 60_000);
    small.get('a'); // refresh, so `b` is now coldest
    small.set('c', 3, 60_000);
    assert.equal(small.get('a'), 1);
    assert.equal(small.get('b'), undefined);
  });
});

/* ------------------------------------------------------ 9. dot-segment ids */

describe('identifiers spliced into an upstream path', () => {
  it('refuses the segments URL normalisation would collapse', () => {
    // `encodeURIComponent` leaves `.` alone, so `..` reached the upstream as a
    // dot-segment and resolved `…/v2/markets/..` to `…/v2/`.
    for (const bad of ['.', '..', '...', '.hidden', '-lead', '']) {
      assert.equal(isValidIdentifier(bad), false, `${bad} should be refused`);
      assert.throws(() => assertTicker(bad), /not a valid Kalshi/);
      assert.throws(() => assertSlug(bad), /not a valid Polymarket US/);
    }
  });

  it('accepts the identifiers the venues actually use', () => {
    assert.equal(assertTicker('KXFEDDECISION-26SEP-T3.75'), 'KXFEDDECISION-26SEP-T3.75');
    assert.equal(assertTicker('KXMVECROSSCATEGORY-SHARD1'), 'KXMVECROSSCATEGORY-SHARD1');
    assert.equal(assertSlug('tec-mlb-champ-2026-09-27-lad'), 'tec-mlb-champ-2026-09-27-lad');
    assert.equal(assertSlug('fed_decision_2026'), 'fed_decision_2026');
  });

  it('refuses an identifier long enough to be a payload', () => {
    assert.equal(isValidIdentifier(`K${'X'.repeat(200)}`), false);
  });
});

/* ------------------------------------------------------------- server-wide */

describe('the running server', () => {
  const servers: ReturnType<ReturnType<typeof createApp>['listen']>[] = [];

  async function start(env: Record<string, string> = {}): Promise<string> {
    const saved = { ...process.env };
    Object.assign(process.env, { RATE_LIMIT: '100000', ...env });
    const server = createApp().listen(0, '127.0.0.1');
    await new Promise((resolve) => server.once('listening', resolve));
    process.env = saved;
    servers.push(server);
    return `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  }

  after(() => {
    for (const server of servers) server.close();
  });

  it('sends the security headers on every response', async () => {
    const res = await fetch(`${await start()}/api/health`);
    const csp = res.headers.get('content-security-policy') ?? '';

    assert.match(csp, /default-src 'self'/);
    assert.match(csp, /frame-ancestors 'none'/);
    assert.match(csp, /object-src 'none'/);
    assert.equal(res.headers.get('x-content-type-options'), 'nosniff');
    assert.equal(res.headers.get('x-frame-options'), 'DENY');
    assert.equal(res.headers.get('referrer-policy'), 'no-referrer');
  });

  it('does not let a client pick its own rate-limit bucket', async () => {
    // The attack: `trust proxy: true` made `req.ip` whatever the caller said,
    // so rotating this header took 12 of 12 requests against a limit of 5.
    const base = await start({ RATE_LIMIT: '5' });

    let allowed = 0;
    for (let i = 0; i < 12; i++) {
      const res = await fetch(`${base}/api/health`, {
        headers: { 'X-Forwarded-For': `10.9.0.${i}` },
      });
      if (res.status !== 429) allowed++;
    }
    assert.ok(allowed <= 5, `${allowed} of 12 spoofed requests were allowed`);
  });

  it('honours X-Forwarded-For when an operator opts in', async () => {
    // The fix must not break the deployment that really is behind a proxy.
    const base = await start({ RATE_LIMIT: '2', TRUST_PROXY: '1' });

    const first = await fetch(`${base}/api/health`, { headers: { 'X-Forwarded-For': '10.1.1.1' } });
    const second = await fetch(`${base}/api/health`, { headers: { 'X-Forwarded-For': '10.1.1.1' } });
    const third = await fetch(`${base}/api/health`, { headers: { 'X-Forwarded-For': '10.1.1.1' } });
    const other = await fetch(`${base}/api/health`, { headers: { 'X-Forwarded-For': '10.2.2.2' } });

    assert.equal(first.status, 200);
    assert.equal(second.status, 200);
    assert.equal(third.status, 429, 'a real client should still be limited');
    assert.equal(other.status, 200, 'a different real client should not be');
  });

  it('withholds cache counters but keeps the capability flags', async () => {
    const body = (await (await fetch(`${await start()}/api/health`)).json()) as Record<
      string,
      unknown
    >;
    // Hit/miss/eviction totals are a live read-out on the cache an attacker is
    // trying to churn.
    assert.equal(body['cache'], undefined);
    assert.equal(body['uptimeSeconds'], undefined);
    // These stay: the client uses them to explain a feed this deployment cannot serve.
    assert.equal(typeof body['fredApiKey'], 'boolean');
    assert.equal(typeof body['alpacaKeys'], 'boolean');
  });

  it('reports operational counters when the operator asks for them', async () => {
    const body = (await (
      await fetch(`${await start({ HEALTH_DETAIL: '1' })}/api/health`)
    ).json()) as Record<string, unknown>;
    assert.equal(typeof body['uptimeSeconds'], 'number');
    assert.ok(body['cache']);
  });

  it('still explains a bad request', async () => {
    // Withholding internal detail must not cost the hints the terminal is built on.
    const res = await fetch(`${await start()}/api/venue/nope/series`);
    const body = (await res.json()) as Record<string, unknown>;

    assert.equal(res.status, 400);
    assert.equal(body['code'], 'bad_request');
    assert.match(String(body['hint']), /kalshi, polymarket, polymarket-us/);
  });

  it('refuses a search the ranking cannot afford', async () => {
    const base = await start();
    const res = await fetch(
      `${base}/api/venue/kalshi/search?q=${Array(4_000).fill('a').join('+')}`,
    );
    const body = (await res.json()) as Record<string, unknown>;

    assert.equal(res.status, 400);
    assert.equal(body['code'], 'bad_request');
  });
});
