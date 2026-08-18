/**
 * Search and ranking over a venue's open-event snapshot.
 *
 * Every venue answers the same three questions — what is open, what matches
 * these words, what is busiest — and none of them offers an endpoint that does
 * it well. Kalshi has no search at all; Polymarket International's ranks by its
 * own relevance and cannot be filtered; Polymarket US's matched "Seeman Jan vs
 * Dufek Jakub Jr" for the query `fed`, and Gemini's answered it with baseball.
 * So each source crawls its catalogue into a {@link Corpus} once per TTL and the
 * ranking lives here, identical for every venue, which is also what makes
 * cross-venue results comparable: the same query scores the same way whoever is
 * listing the market.
 */

import type { Market, Venue, VenueEvent } from '../../shared/types.js';
import type { MoverSort } from '../../shared/venue.js';

export interface Corpus {
  venue: Venue;
  events: VenueEvent[];
  markets: Market[];
  builtAt: number;
  /**
   * True when the crawl hit its page cap before the catalogue ran out. The
   * panel says so rather than presenting a partial universe as the whole one.
   */
  truncated: boolean;
}

export interface EventSearchHit {
  event: Omit<VenueEvent, 'markets'>;
  /** Markets in the event, most liquid first. */
  markets: Market[];
  /** Summed 24h volume across the event's markets, `null` when unpublished. */
  volume24h: number | null;
  score: number;
}

export interface SearchResponse {
  query: string;
  hits: EventSearchHit[];
  /** How many events were searched. */
  scanned: number;
  /** Age of the snapshot in seconds — surfaced so the UI can say so. */
  snapshotAgeSeconds: number;
  truncated: boolean;
}

/**
 * Add up a figure across a ladder, keeping "unpublished" distinct from "zero".
 *
 * A Polymarket US event has no volume anywhere in its public catalogue. Summing
 * that to `0` would rank it below a genuinely dead Kalshi market and print a
 * confident zero in a column that should read `--`.
 */
export function sumOrNull(
  markets: Market[],
  pick: (market: Market) => number | null,
): number | null {
  let total = 0;
  let seen = false;
  for (const market of markets) {
    const value = pick(market);
    if (value === null) continue;
    total += value;
    seen = true;
  }
  return seen ? total : null;
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * Rank open events against a free-text query.
 *
 * Every whitespace-separated term must appear somewhere in the haystack
 * (event ticker, title, sub-title, category, and its markets' strike labels).
 * Scoring rewards ticker hits, title-prefix hits and word-boundary hits, and a
 * whole-phrase match outranks scattered terms. Ties break on 24h volume so the
 * liquid event wins.
 */
export function searchCorpus(snapshot: Corpus, query: string, limit = 25): SearchResponse {
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
  const hits: EventSearchHit[] = [];

  for (const event of snapshot.events) {
    const ticker = event.eventTicker.toLowerCase();
    const title = event.title.toLowerCase();
    const strikes = event.markets
      .map((m) => m.yesSubTitle)
      .join(' ')
      .toLowerCase();
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

    const volume24h = sumOrNull(event.markets, (m) => m.volume24h);
    if (volume24h !== null && volume24h > 0) score += 5;

    const { markets, ...meta } = event;
    hits.push({
      event: meta,
      markets: [...markets].sort((a, b) => (b.volume24h ?? 0) - (a.volume24h ?? 0)),
      volume24h,
      score,
    });
  }

  hits.sort((a, b) => b.score - a.score || (b.volume24h ?? 0) - (a.volume24h ?? 0));

  return {
    query,
    hits: hits.slice(0, limit),
    scanned: snapshot.events.length,
    snapshotAgeSeconds: Math.round((Date.now() - snapshot.builtAt) / 1000),
    truncated: snapshot.truncated,
  };
}

/**
 * Re-exported from the venue registry, where each venue declares which of
 * these rankings it can actually serve. Kept exported here so the source
 * modules that rank a corpus keep importing it from the module they rank with.
 */
export type { MoverSort } from '../../shared/venue.js';

const FIELD: Record<MoverSort, (market: Market) => number | null> = {
  volume: (m) => m.volume24h,
  open_interest: (m) => m.openInterest,
  liquidity: (m) => m.liquidity,
  gainers: (m) => m.change,
  losers: (m) => m.change,
};

/**
 * Leaderboard over a snapshot. Powers the `TOP` command.
 *
 * A market whose venue does not publish the sorted figure is left out of that
 * board entirely. Ranking it as a zero would seat every Polymarket US contract
 * at the bottom of `TOP volume` and imply nothing trades there, which is a
 * statement about the venue's API, not its book.
 */
export function rankMarkets(markets: Market[], sort: MoverSort, limit = 25): Market[] {
  const field = FIELD[sort];

  const eligible = markets.filter((m) => {
    if (field(m) === null) return false;
    // A mover with no turnover behind it is a stale print, not a move.
    if (sort === 'gainers' || sort === 'losers') return (m.volume24h ?? 0) > 0;
    return true;
  });

  // Every board but `losers` wants the largest figure first; `losers` wants the
  // most negative change first. Written as a plain ascending comparator with an
  // explicit flip, because the previous form — a `direction` multiplied into an
  // already-descending subtraction — cancelled itself out and inverted all five
  // boards. Nothing caught it downstream: the panel re-sorts what it is given,
  // so `TOP volume` looked correctly ordered while listing the thirty quietest
  // markets on the exchange.
  const ascending = sort === 'losers';
  return eligible
    .sort((a, b) => ((field(a) ?? 0) - (field(b) ?? 0)) * (ascending ? 1 : -1))
    .slice(0, limit);
}
