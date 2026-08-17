/**
 * TTL cache and single-flight tests.
 *
 * This module is the reason four open panels polling one ticker are not four
 * upstream requests a minute, so the cases that matter are the ones where a
 * cache quietly stops being a cache: an entry served a millisecond past its
 * lifetime, two concurrent misses that each open a socket, a rejection that
 * gets remembered as if it were data, and an in-flight entry left behind by a
 * failure so the retry can never run.
 *
 * The keys and values are the real shapes the sources put through it — the
 * strings `kalshi.ts`, `stocks.ts` and `crypto.ts` actually build, and trimmed
 * copies of the payloads they cache — because two of this cache's sharp edges
 * are properties of the *key*: it is compared as an exact string, so the venue
 * prefixes are the only thing keeping two sources apart, and the TTL is not
 * part of it, so whoever writes an entry fixes its lifetime for every later
 * reader. `stocks.ts` folds `ttlMs` into its Nasdaq key precisely because of
 * the second one, and the test below pins the behaviour that makes that
 * necessary.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { TTL, TtlCache } from '../src/server/lib/cache.js';

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

/** A promise whose settlement the test drives, so nothing here races a timer. */
function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void; reject: (e: unknown) => void } {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/* --------------------------------------------------------------- fixtures */

/** Trimmed `/trade-api/v2/markets/{ticker}` row, as `normaliseMarket` sees it. */
const KALSHI_MARKET = {
  ticker: 'KXBTCD-26AUG16-B63000',
  event_ticker: 'KXBTCD-26AUG16',
  title: 'Bitcoin above $63,000 on Aug 16?',
  status: 'active',
  yes_bid: 62,
  yes_ask: 64,
  last_price: 63,
  volume: 184_233,
  open_interest: 41_002,
};

/** Trimmed `/products/BTC-USD/ticker`, the shape `crypto.ts` caches for 3s. */
const COINBASE_TICKER = {
  trade_id: 812_334_901,
  price: '63039.67',
  size: '0.00174',
  time: '2026-08-16T21:14:07.482913Z',
  bid: '63039.66',
  ask: '63040.71',
  volume: '9184.42117253',
};

/** The keys the sources build, verbatim. */
const KALSHI_KEY = `kalshi:/markets/${KALSHI_MARKET.ticker}`;
const COINBASE_KEY = 'coinbase:/products/BTC-USD/ticker';

/* ------------------------------------------------------------- get / set */

describe('TtlCache get and set', () => {
  it('serves a value inside its lifetime and refuses it once the lifetime is spent', async () => {
    const cache = new TtlCache();
    cache.set(COINBASE_KEY, COINBASE_TICKER, 40);
    assert.deepEqual(cache.get(COINBASE_KEY), COINBASE_TICKER);
    await sleep(70);
    assert.equal(cache.get(COINBASE_KEY), undefined);
  });

  it('treats the expiry instant itself as expired, so a zero ttl caches nothing', () => {
    // `TTL.quote` is 3s and a book is worthless a tick later; the boundary has
    // to fall on the side of refetching, not of serving a dead print.
    const cache = new TtlCache();
    cache.set(COINBASE_KEY, COINBASE_TICKER, 0);
    assert.equal(cache.get(COINBASE_KEY), undefined);
    cache.set(COINBASE_KEY, COINBASE_TICKER, -1000);
    assert.equal(cache.get(COINBASE_KEY), undefined);
  });

  it('drops the expired entry as it reads it, rather than leaving it to rot', async () => {
    const cache = new TtlCache();
    cache.set(COINBASE_KEY, COINBASE_TICKER, 20);
    await sleep(50);
    assert.equal(cache.get(COINBASE_KEY), undefined);
    assert.equal(cache.stats().entries, 0);
  });

  it('tells a stored falsy value apart from a miss', async () => {
    // A book with no bids is `0`, an empty schedule is `[]`, and a market with
    // no candles is `null`. All three are answers, and none should refetch.
    const cache = new TtlCache();
    for (const value of [0, false, '', null] as const) {
      const key = `tvmaze:us:${String(value)}`;
      let loads = 0;
      const load = async (): Promise<typeof value> => {
        loads++;
        return value;
      };
      assert.equal(await cache.cached(key, 1000, load), value);
      assert.equal(await cache.cached(key, 1000, load), value);
      assert.equal(loads, 1, `a cached ${String(value)} was refetched`);
    }
  });

  it('answers undefined for a key nobody has written, and shrugs at deleting one', () => {
    const cache = new TtlCache();
    assert.equal(cache.get('kalshi:/markets/NOSUCH'), undefined);
    cache.delete('kalshi:/markets/NOSUCH');
    assert.deepEqual(cache.stats(), { hits: 0, misses: 0, entries: 0, evictions: 0 });
  });

  it('empties the store on clear but keeps the cumulative counters', async () => {
    // `/health` reads these to show a hit rate over the life of the process;
    // resetting them on a flush would report a cold cache as a broken one.
    const cache = new TtlCache();
    await cache.cached(KALSHI_KEY, 1000, async () => KALSHI_MARKET);
    await cache.cached(KALSHI_KEY, 1000, async () => KALSHI_MARKET);
    cache.clear();
    assert.deepEqual(cache.stats(), { hits: 1, misses: 1, entries: 0, evictions: 0 });
    assert.equal(cache.get(KALSHI_KEY), undefined);
  });

  it('deletes one key without touching its neighbours', () => {
    const cache = new TtlCache();
    cache.set(KALSHI_KEY, KALSHI_MARKET, 1000);
    cache.set(COINBASE_KEY, COINBASE_TICKER, 1000);
    cache.delete(KALSHI_KEY);
    assert.equal(cache.get(KALSHI_KEY), undefined);
    assert.deepEqual(cache.get(COINBASE_KEY), COINBASE_TICKER);
  });
});

/* ---------------------------------------------------------------- cached */

describe('TtlCache cached', () => {
  it('runs the loader once and serves later callers from the entry it wrote', async () => {
    const cache = new TtlCache();
    let loads = 0;
    const load = async (): Promise<typeof KALSHI_MARKET> => {
      loads++;
      return KALSHI_MARKET;
    };
    assert.deepEqual(await cache.cached(KALSHI_KEY, 1000, load), KALSHI_MARKET);
    assert.deepEqual(await cache.cached(KALSHI_KEY, 1000, load), KALSHI_MARKET);
    assert.deepEqual(await cache.cached(KALSHI_KEY, 1000, load), KALSHI_MARKET);
    assert.equal(loads, 1);
  });

  it('collapses concurrent misses for one key onto a single upstream request', async () => {
    // This is the whole point: four panels on one ticker, all missing at once,
    // must be one Kalshi request and not four.
    const cache = new TtlCache();
    const gate = deferred<typeof KALSHI_MARKET>();
    let loads = 0;
    const load = (): Promise<typeof KALSHI_MARKET> => {
      loads++;
      return gate.promise;
    };

    const waiting = Promise.all([
      cache.cached(KALSHI_KEY, 1000, load),
      cache.cached(KALSHI_KEY, 1000, load),
      cache.cached(KALSHI_KEY, 1000, load),
      cache.cached(KALSHI_KEY, 1000, load),
    ]);
    gate.resolve(KALSHI_MARKET);
    const results = await waiting;

    assert.equal(loads, 1);
    assert.equal(results.length, 4);
    assert.ok(results.every((r) => r === KALSHI_MARKET));
  });

  it('gives the joining caller the first caller loader, not its own', async () => {
    // Coalescing is keyed on the string alone, so two callers that share a key
    // share a *result* whatever functions they passed. It is why every source
    // prefixes its keys with its own name.
    const cache = new TtlCache();
    const gate = deferred<string>();
    const first = cache.cached(KALSHI_KEY, 1000, () => gate.promise);
    const second = cache.cached(KALSHI_KEY, 1000, async () => 'never runs');
    gate.resolve('from the first loader');
    assert.equal(await first, 'from the first loader');
    assert.equal(await second, 'from the first loader');
  });

  it('reopens the request once the entry has expired', async () => {
    const cache = new TtlCache();
    let loads = 0;
    const load = async (): Promise<number> => ++loads;
    assert.equal(await cache.cached(COINBASE_KEY, 30, load), 1);
    assert.equal(await cache.cached(COINBASE_KEY, 30, load), 1);
    await sleep(60);
    assert.equal(await cache.cached(COINBASE_KEY, 30, load), 2);
    assert.equal(loads, 2);
  });

  it('clears the in-flight entry after a success, so the key is not pinned forever', async () => {
    const cache = new TtlCache();
    let loads = 0;
    // A zero ttl means the store can never answer, so a second call can only
    // succeed if the in-flight map was cleaned up after the first one settled.
    const load = async (): Promise<number> => ++loads;
    assert.equal(await cache.cached(COINBASE_KEY, 0, load), 1);
    assert.equal(await cache.cached(COINBASE_KEY, 0, load), 2);
    assert.equal(loads, 2);
  });
});

/* --------------------------------------------------------------- failure */

describe('TtlCache cached under failure', () => {
  it('does not remember a rejection, so the next caller retries', async () => {
    // Nasdaq 503s under load. Caching that for TTL.meta would blank a panel for
    // a minute over one bad second.
    const cache = new TtlCache();
    let loads = 0;
    const load = async (): Promise<typeof KALSHI_MARKET> => {
      loads++;
      if (loads === 1) throw new Error('api.nasdaq.com returned HTTP 503');
      return KALSHI_MARKET;
    };

    await assert.rejects(cache.cached(KALSHI_KEY, 60_000, load), /503/);
    assert.deepEqual(await cache.cached(KALSHI_KEY, 60_000, load), KALSHI_MARKET);
    assert.equal(loads, 2);
  });

  it('fails every caller waiting on one failing request, and only asks once', async () => {
    const cache = new TtlCache();
    const gate = deferred<never>();
    let loads = 0;
    const load = (): Promise<never> => {
      loads++;
      return gate.promise;
    };

    const settling = Promise.allSettled([
      cache.cached(KALSHI_KEY, 1000, load),
      cache.cached(KALSHI_KEY, 1000, load),
      cache.cached(KALSHI_KEY, 1000, load),
    ]);
    gate.reject(new Error('trading-api.kalshi.com refused the connection'));
    const settled = await settling;

    assert.equal(loads, 1);
    assert.deepEqual(
      settled.map((s) => s.status),
      ['rejected', 'rejected', 'rejected'],
    );
    assert.match((settled[0] as PromiseRejectedResult).reason.message, /refused the connection/);
  });

  it('leaves nothing in flight after a failure, so the retry actually runs', async () => {
    const cache = new TtlCache();
    await assert.rejects(
      cache.cached(KALSHI_KEY, 1000, async () => {
        throw new Error('timeout');
      }),
    );
    assert.equal(cache.get(KALSHI_KEY), undefined);
    assert.equal(await cache.cached(KALSHI_KEY, 1000, async () => 'recovered'), 'recovered');
  });

  it('surfaces a loader that throws before it returns a promise', async () => {
    // `credentials()` in alpaca.ts throws synchronously when the keys are
    // unset, from inside the loader body.
    const cache = new TtlCache();
    const throwsNow = (): Promise<string> => {
      throw new Error('ALPACA_KEY_ID is not set');
    };
    await assert.rejects(cache.cached('alpaca:news:NVDA:30:7', 30_000, throwsNow), /ALPACA_KEY_ID/);
    assert.equal(await cache.cached('alpaca:news:NVDA:30:7', 30_000, async () => 'ok'), 'ok');
  });
});

/* ------------------------------------------------------------------ keys */

describe('TtlCache key handling', () => {
  it('compares keys as exact strings, so the venue prefix is the whole separation', () => {
    // Kalshi and Polymarket both have a `/markets` path. Nothing in the cache
    // would notice the clash; the prefixes each source writes are what prevent it.
    const cache = new TtlCache();
    cache.set('kalshi:/markets?limit=100', ['kalshi row'], 1000);
    cache.set('polymarket:https://gamma-api.polymarket.com/markets', ['polymarket row'], 1000);
    assert.deepEqual(cache.get('kalshi:/markets?limit=100'), ['kalshi row']);
    assert.deepEqual(cache.get('polymarket:https://gamma-api.polymarket.com/markets'), [
      'polymarket row',
    ]);
    assert.equal(cache.get('/markets?limit=100'), undefined);
    assert.equal(cache.stats().entries, 2);
  });

  it("does not fold the ttl into the key, so the first writer fixes everyone else's lifetime", async () => {
    // The behaviour `stocks.ts` works around by keying `nasdaq:${ttlMs}:${path}`:
    // `resolveAssetClass` asks for the same URL at TTL.meta and `getQuote` at
    // TTL.quote, and on a shared key the 60s entry is what the 3s caller gets.
    const cache = new TtlCache();
    const path = '/api/quote/AAPL/info?assetclass=stocks';
    await cache.cached(`nasdaq:${path}`, 60_000, async () => 'written by the 60s caller');
    assert.equal(
      await cache.cached(`nasdaq:${path}`, 30, async () => 'written by the 3s caller'),
      'written by the 60s caller',
    );
    await sleep(60);
    assert.equal(cache.get(`nasdaq:${path}`), 'written by the 60s caller');

    // With the ttl in the key, as shipped, the two callers get their own entries.
    const fixed = new TtlCache();
    await fixed.cached(`nasdaq:60000:${path}`, 60_000, async () => 'meta');
    assert.equal(await fixed.cached(`nasdaq:30:${path}`, 30, async () => 'quote'), 'quote');
    await sleep(60);
    assert.equal(fixed.get(`nasdaq:30:${path}`), undefined);
    assert.equal(fixed.get(`nasdaq:60000:${path}`), 'meta');
  });
});

/* -------------------------------------------------------------- eviction */

describe('TtlCache eviction', () => {
  it('holds no more than maxEntries and counts what it dropped', () => {
    const cache = new TtlCache(2);
    for (let i = 0; i < 10; i++) cache.set(`billboard:hot-100:2026-08-${i}`, i, 60_000);
    const stats = cache.stats();
    assert.equal(stats.entries, 2);
    assert.equal(stats.evictions, 8);
    assert.equal(cache.get('billboard:hot-100:2026-08-9'), 9);
    assert.equal(cache.get('billboard:hot-100:2026-08-0'), undefined);
  });

  it('drops the coldest key, not the oldest one that is still being read', () => {
    const cache = new TtlCache(3);
    cache.set('a', 1, 60_000);
    cache.set('b', 2, 60_000);
    cache.set('c', 3, 60_000);
    cache.get('a'); // 'a' is the hot key even though it went in first
    cache.set('d', 4, 60_000);
    assert.equal(cache.get('a'), 1);
    assert.equal(cache.get('b'), undefined);
    assert.equal(cache.get('c'), 3);
    assert.equal(cache.get('d'), 4);
    assert.equal(cache.stats().evictions, 1);
  });

  it('counts a cached() hit as a read for eviction purposes', async () => {
    const cache = new TtlCache(3);
    await cache.cached('a', 60_000, async () => 1);
    await cache.cached('b', 60_000, async () => 2);
    await cache.cached('c', 60_000, async () => 3);
    await cache.cached('a', 60_000, async () => 99); // a hit, and a refresh
    await cache.cached('d', 60_000, async () => 4);
    assert.equal(cache.get('a'), 1);
    assert.equal(cache.get('b'), undefined);
  });

  it('does not evict to make room for a key it is merely overwriting', () => {
    const cache = new TtlCache(2);
    cache.set('a', 1, 60_000);
    cache.set('b', 2, 60_000);
    cache.set('b', 22, 60_000);
    assert.equal(cache.stats().evictions, 0);
    assert.equal(cache.get('a'), 1);
    assert.equal(cache.get('b'), 22);
  });
});

/* ----------------------------------------------------------------- stats */

describe('TtlCache stats', () => {
  it('scores the request that opened the socket a miss and every other one a hit', async () => {
    const cache = new TtlCache();
    const gate = deferred<string>();
    const joined = Promise.all([
      cache.cached(KALSHI_KEY, 1000, () => gate.promise),
      cache.cached(KALSHI_KEY, 1000, () => gate.promise),
    ]);
    gate.resolve('v');
    await joined;
    await cache.cached(KALSHI_KEY, 1000, async () => 'v');

    assert.deepEqual(cache.stats(), { hits: 2, misses: 1, entries: 1, evictions: 0 });
  });

  it('counts a refetch after expiry as a fresh miss', async () => {
    const cache = new TtlCache();
    await cache.cached(COINBASE_KEY, 20, async () => COINBASE_TICKER);
    await sleep(50);
    await cache.cached(COINBASE_KEY, 20, async () => COINBASE_TICKER);
    const stats = cache.stats();
    assert.equal(stats.misses, 2);
    assert.equal(stats.hits, 0);
  });
});

/* -------------------------------------------------------------- identity */

describe('TtlCache value identity', () => {
  it('hands every reader the same object, so a cached payload is read-only by contract', () => {
    // No copy is made on the way in or out. Two panels reading one Kalshi
    // market hold one object between them, and a source that sorted or spliced
    // a cached array in place would rewrite what the next reader sees.
    const cache = new TtlCache();
    const rows = [{ ticker: 'KXBTCD-26AUG16-B63000', volume: 184_233 }];
    cache.set('kalshi:corpus', rows, 60_000);
    const first = cache.get<typeof rows>('kalshi:corpus');
    const second = cache.get<typeof rows>('kalshi:corpus');
    assert.equal(first, rows);
    assert.equal(first, second);
  });
});

/* ------------------------------------------------------------------ ttls */

describe('TTL table', () => {
  it('gives every source a positive, finite lifetime', () => {
    for (const [name, ms] of Object.entries(TTL)) {
      assert.ok(Number.isFinite(ms) && ms > 0, `TTL.${name} is ${ms}`);
    }
  });

  it('orders the lifetimes the way the sources move', () => {
    // A quote must not outlive a candle bucket, and neither may outlive the
    // metadata that names them. A transposed digit here shows up as a stale
    // panel, not as an error.
    assert.ok(TTL.quote < TTL.candles);
    assert.ok(TTL.candles < TTL.meta);
    assert.ok(TTL.meta < TTL.catalogue);
    assert.equal(TTL.quote, 3_000);
    assert.equal(TTL.billboard, 60 * 60_000);
    assert.equal(TTL.netflix, 6 * 60 * 60_000);
  });
});
