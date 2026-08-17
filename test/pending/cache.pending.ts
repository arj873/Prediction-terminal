/**
 * Suspected defects in the TTL cache. These FAIL against the current
 * `src/server/lib/cache.ts` and are parked outside the `test/*.test.ts` glob.
 *
 * Each one is a case where the cache does not do what its own comments say it
 * does:
 *
 *   1. `clear()` and `delete()` drop the stored entry but leave the in-flight
 *      map alone, so a response produced *before* the invalidation is written
 *      back into the store *after* it — and a caller that arrives after the
 *      flush is handed that pre-flush value instead of its own loader result.
 *      An invalidation that the cache undoes for you is worse than none.
 *   2. `get()` carries a comment saying it refreshes insertion order "so the
 *      LRU eviction below drops cold keys first", but `set()` on a key that is
 *      already live does not, because `Map.set` keeps an existing key in its
 *      original position. Rewriting an entry therefore leaves it at the head of
 *      the queue, and the freshest value in the cache is the first one evicted.
 *   3. A loader that resolves `undefined` gets an entry written for it that
 *      `cached()` can never read back, because `hit !== undefined` cannot tell
 *      a stored `undefined` from a miss. The loader reruns on every call while
 *      the dead entry keeps occupying a slot against `maxEntries`.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { TtlCache } from '../../src/server/lib/cache.js';

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void } {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

/** Trimmed `/products/BTC-USD/ticker`, the shape `crypto.ts` caches for 3s. */
const STALE_TICKER = { price: '63039.67', time: '2026-08-16T21:14:07.482913Z' };
const FRESH_TICKER = { price: '63512.10', time: '2026-08-16T21:31:55.001204Z' };

const KEY = 'coinbase:/products/BTC-USD/ticker';

describe('TtlCache invalidation against an in-flight request', () => {
  it('does not serve a pre-clear response to a caller that arrived after the clear', async () => {
    const cache = new TtlCache();
    const gate = deferred<typeof STALE_TICKER>();

    const before = cache.cached(KEY, 3_000, () => gate.promise);
    cache.clear();
    // A new caller, after the flush, asking for a fresh print with its own loader.
    const after = cache.cached(KEY, 3_000, async () => FRESH_TICKER);
    gate.resolve(STALE_TICKER);

    await before;
    // ACTUAL: { price: '63039.67', … } — the pre-clear value. The loader passed
    // here never ran, because `clear()` left the in-flight entry in place.
    assert.deepEqual(await after, FRESH_TICKER);
  });

  it('does not repopulate the store from work that was in flight when clear() ran', async () => {
    const cache = new TtlCache();
    const gate = deferred<typeof STALE_TICKER>();

    const before = cache.cached(KEY, 3_000, () => gate.promise);
    cache.clear();
    gate.resolve(STALE_TICKER);
    await before;
    await sleep(5);

    // ACTUAL: { price: '63039.67', … } and entries: 1. The cleared cache refilled
    // itself with the very entry that was just flushed.
    assert.equal(cache.get(KEY), undefined);
    assert.equal(cache.stats().entries, 0);
  });

  it('does not repopulate the store from work that was in flight when delete() ran', async () => {
    const cache = new TtlCache();
    const gate = deferred<typeof STALE_TICKER>();

    const before = cache.cached(KEY, 3_000, () => gate.promise);
    cache.delete(KEY);
    gate.resolve(STALE_TICKER);
    await before;
    await sleep(5);

    // ACTUAL: { price: '63039.67', … } — the deleted key is back.
    assert.equal(cache.get(KEY), undefined);
  });
});

describe('TtlCache eviction order after an overwrite', () => {
  it('treats a rewritten entry as the most recent, not the oldest', async () => {
    // `resolveAssetClass` writes `nasdaq:class:AAPL` through `cache.set`, and a
    // busy terminal keeps the store at its bound. Rewriting a live key must not
    // move it to the front of the eviction queue.
    const cache = new TtlCache(3);
    cache.set('nasdaq:class:AAPL', 'stocks', 900_000);
    cache.set('nasdaq:class:MSFT', 'stocks', 900_000);
    cache.set('nasdaq:class:SPY', 'etf', 900_000);

    cache.set('nasdaq:class:AAPL', 'stocks', 900_000); // rewritten, so the hottest
    cache.set('nasdaq:class:COMP', 'index', 900_000); // needs one slot

    // ACTUAL: AAPL is undefined and MSFT survives — the entry written a
    // microsecond ago was evicted ahead of two that have not been touched since.
    assert.equal(cache.get('nasdaq:class:AAPL'), 'stocks');
    assert.equal(cache.get('nasdaq:class:MSFT'), undefined);
  });
});

describe('TtlCache with a loader that resolves undefined', () => {
  it('caches an undefined result instead of refetching it on every call', async () => {
    const cache = new TtlCache();
    let loads = 0;
    const load = async (): Promise<undefined> => {
      loads++;
      return undefined;
    };

    await cache.cached('tvmaze:us:2026-08-16', 1_800_000, load);
    await cache.cached('tvmaze:us:2026-08-16', 1_800_000, load);
    await cache.cached('tvmaze:us:2026-08-16', 1_800_000, load);

    // ACTUAL: loads === 3 and stats() === { hits: 0, misses: 3, entries: 1, … }.
    // The entry exists and counts against maxEntries; it just can never be read.
    assert.equal(loads, 1);
    assert.deepEqual(cache.stats(), { hits: 2, misses: 1, entries: 1, evictions: 0 });
  });
});
