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
  /** News: a wire. Short enough to feel live, long enough that N panels are 1 call. */
  news: 30_000,
  /** Rotten Tomatoes: reviews trickle in, and a score can move mid-day. */
  rottenTomatoes: 15 * 60_000,
  /** Netflix Top 10: published once a week, on Tuesdays. */
  netflix: 6 * 60 * 60_000,
  /** Spotify and YouTube chart mirrors: rebuilt once a day. */
  streamCharts: 30 * 60_000,
  /** Box office: estimates are revised through the day, then finalised. */
  boxOffice: 30 * 60_000,
  /** Steam concurrents: a live number, and the whole point of the panel. */
  steam: 120_000,
  /** TV schedules: fixed a day ahead, occasionally amended. */
  tvSchedule: 30 * 60_000,
  /** Commitments of Traders: published once a week, on Friday afternoons. */
  cot: 60 * 60_000,
  /** Congressional actions: a bill moves several times a day when it moves at all. */
  congress: 10 * 60_000,
  /** EDGAR filings: a new 8-K matters within minutes of being accepted. */
  edgar: 5 * 60_000,
  /** XBRL company facts: restated quarterly, and a 15 MB document per issuer. */
  edgarFacts: 6 * 60 * 60_000,
  /** The data.gov catalogue: dataset metadata, revised on a publication schedule. */
  datagov: 60 * 60_000,
} as const;
