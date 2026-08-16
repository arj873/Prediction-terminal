/**
 * In-memory TTL cache with single-flight.
 *
 * Panels poll — several of them, on their own timers, often for the same
 * ticker. Without coalescing, four panels showing one market means four
 * upstream requests every tick. `cached()` collapses concurrent misses for the
 * same key onto one in-flight promise.
 */

interface Entry<T> {
  value: T;
  expiresAt: number;
}

export interface CacheStats {
  hits: number;
  misses: number;
  entries: number;
  evictions: number;
}

export class TtlCache {
  readonly #store = new Map<string, Entry<unknown>>();
  readonly #inflight = new Map<string, Promise<unknown>>();
  readonly #maxEntries: number;
  #hits = 0;
  #misses = 0;
  #evictions = 0;

  constructor(maxEntries = 1000) {
    this.#maxEntries = maxEntries;
  }

  get<T>(key: string): T | undefined {
    const entry = this.#store.get(key) as Entry<T> | undefined;
    if (!entry) return undefined;
    if (entry.expiresAt <= Date.now()) {
      this.#store.delete(key);
      return undefined;
    }
    // Refresh insertion order so the LRU eviction below drops cold keys first.
    this.#store.delete(key);
    this.#store.set(key, entry);
    return entry.value;
  }

  set<T>(key: string, value: T, ttlMs: number): void {
    if (this.#store.size >= this.#maxEntries && !this.#store.has(key)) {
      const oldest = this.#store.keys().next();
      if (!oldest.done) {
        this.#store.delete(oldest.value);
        this.#evictions++;
      }
    }
    this.#store.set(key, { value, expiresAt: Date.now() + ttlMs });
  }

  /**
   * Return the cached value for `key`, or run `produce` to fill it.
   *
   * Concurrent callers that miss share a single `produce()` call. A rejection
   * is not cached — the next caller retries.
   */
  async cached<T>(key: string, ttlMs: number, produce: () => Promise<T>): Promise<T> {
    const hit = this.get<T>(key);
    if (hit !== undefined) {
      this.#hits++;
      return hit;
    }

    const pending = this.#inflight.get(key) as Promise<T> | undefined;
    if (pending) {
      this.#hits++;
      return pending;
    }

    this.#misses++;
    const promise = produce()
      .then((value) => {
        this.set(key, value, ttlMs);
        return value;
      })
      .finally(() => {
        this.#inflight.delete(key);
      });

    this.#inflight.set(key, promise);
    return promise;
  }

  delete(key: string): void {
    this.#store.delete(key);
  }

  clear(): void {
    this.#store.clear();
  }

  stats(): CacheStats {
    return {
      hits: this.#hits,
      misses: this.#misses,
      entries: this.#store.size,
      evictions: this.#evictions,
    };
  }
}

export const cache = new TtlCache();

/** TTLs, tuned to how fast each source actually moves. */
export const TTL = {
  /** Quotes and books: short, but long enough to absorb a panel refresh burst. */
  quote: 3_000,
  /** Candles: the newest bucket is still forming, so don't hold it long. */
  candles: 20_000,
  /** Market/event metadata: titles and rules effectively never change. */
  meta: 60_000,
  /** Series catalogue: static for the life of a session. */
  catalogue: 15 * 60_000,
  /** FRED observations: revised on a release schedule, never intraday. */
  fred: 30 * 60_000,
  /** Billboard: refreshes once a week. */
  billboard: 60 * 60_000,
} as const;
