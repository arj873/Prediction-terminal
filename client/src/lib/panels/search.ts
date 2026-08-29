/**
 * What `SRCH` says about an answer, and about the venues that did not give one.
 *
 * Split out of the panel because the sentence this file picks is the whole
 * defect it exists to fix. `SRCH fed decision` once printed
 *
 *     0 events · 0 scanned · KAL unavailable · PM unavailable · …
 *     Nothing open matches "fed decision".
 *     Searched 0 open events across 6 venues. Try fewer or broader words.
 *
 * while every venue was unreachable and nothing had been searched at all. Both
 * sentences are advice about a query that never ran, and the word `unavailable`
 * replaced six different reasons — one of which was "Is the API server
 * running?" — so a reader spent their time rewriting words instead of starting
 * the server. Two rules follow, and both are tested here:
 *
 * - a venue that did not answer is named *with its reason*, never with a shrug;
 * - nothing was searched is never reported as nothing matched.
 */

import type { EventSearchHit, SearchResponse, Venue } from '$gen';

import type { NoteSegment } from './table';
import { venueInfo } from '../terminal/venue';

/** Why a venue gave no answer. The upstream's own words, not a summary. */
export interface VenueFailure {
  message: string;
  code?: string;
  hint?: string;
}

/** One venue's answer, or the reason it did not give one. */
export interface VenueResult {
  venue: Venue;
  response: SearchResponse | null;
  error: VenueFailure | null;
}

/** The panel body when there are no rows. */
export type EmptyBody =
  /** Nothing was searched. The query is not the story and must not be blamed. */
  | { kind: 'outage'; message: string; reasons: string[]; hint?: string }
  /** Something was searched and matched nothing. */
  | { kind: 'miss'; message: string; hint: string };

const answered = (results: readonly VenueResult[]): VenueResult[] =>
  results.filter((result) => result.response !== null);

const failed = (results: readonly VenueResult[]): VenueResult[] =>
  results.filter((result) => result.error !== null);

const named = (results: readonly VenueResult[]): string =>
  results.map((result) => venueInfo(result.venue).code).join(', ');

export function scannedTotal(results: readonly VenueResult[]): number {
  return results.reduce((sum, result) => sum + (result.response?.scanned ?? 0), 0);
}

/**
 * Interleaved the way the server ranked each venue's own list: how much of the
 * query an event answered first, then relevance, then liquidity. Sorting on
 * score alone would float a one-word match from a high-scoring venue above a
 * full match from another.
 */
export function rank(results: readonly VenueResult[]): { venue: Venue; hit: EventSearchHit }[] {
  return results
    .flatMap((result) => (result.response?.hits ?? []).map((hit) => ({ venue: result.venue, hit })))
    .sort(
      (a, b) =>
        b.hit.matchedTerms - a.hit.matchedTerms ||
        b.hit.score - a.hit.score ||
        (b.hit.volume24h ?? 0) - (a.hit.volume24h ?? 0),
    );
}

/** How much of the query a hit answered, or `null` when it answered all of it. */
export function partial(hit: EventSearchHit): string | null {
  return hit.totalTerms > 0 && hit.matchedTerms < hit.totalTerms
    ? `${hit.matchedTerms}/${hit.totalTerms} words`
    : null;
}

export function note(results: readonly VenueResult[]): NoteSegment[] {
  const replied = answered(results);
  const missing = failed(results);
  const truncated = replied.filter((result) => result.response!.truncated);
  const hits = replied.reduce((sum, result) => sum + result.response!.hits.length, 0);

  const counts = replied
    .map((result) => `${venueInfo(result.venue).code} ${result.response!.hits.length}`)
    .join(' · ');

  const segments: NoteSegment[] = [
    { text: `${hits} events${counts ? ` · ${counts}` : ''}` },
    { text: ` · ${scannedTotal(results).toLocaleString()} scanned`, tone: 'dim' },
  ];

  // The oldest snapshot in the set, because that is the age of the answer.
  if (replied.length > 0) {
    const age = Math.max(...replied.map((result) => result.response!.snapshotAgeSeconds));
    segments.push({ text: ` · snapshot ${age}s old`, tone: 'dim' });
  }

  if (truncated.length > 0) {
    segments.push({
      text: ` · ${named(truncated)} catalogue truncated — this is not the whole book`,
      tone: 'down',
    });
  }

  // When nothing answered at all, `emptyBody` states every reason in full and
  // the note would only say it twice — six venues' worth of message and hint
  // in a summary strip, directly above the same six lines.
  if (replied.length === 0) return segments;

  // Named with the reason, never merely named: a venue that did not answer
  // cannot be read as a venue that lists nothing matching, and six venues
  // reading `unavailable` cannot be told apart from each other either.
  for (const failure of missing) {
    segments.push({
      text: ` · ${venueInfo(failure.venue).code} ${failure.error!.message}`,
      tone: 'down',
    });
  }

  // The upstream's own sentence about what to do, deduplicated: six venues
  // behind one unreachable API server all carry the same one.
  for (const hint of new Set(missing.map((failure) => failure.error!.hint).filter(Boolean))) {
    segments.push({ text: `  ${hint}`, tone: 'dim' });
  }

  return segments;
}

export function emptyBody(query: string, results: readonly VenueResult[]): EmptyBody {
  const replied = answered(results);
  const missing = failed(results);

  if (replied.length === 0 && missing.length > 0) {
    return {
      kind: 'outage',
      message:
        missing.length === 1
          ? `${venueInfo(missing[0]!.venue).label} did not answer, so nothing was searched.`
          : 'No venue answered, so nothing was searched.',
      reasons: missing.map(
        (failure) => `${venueInfo(failure.venue).code} — ${failure.error!.message}`,
      ),
      hint: missing.map((failure) => failure.error!.hint).find(Boolean),
    };
  }

  const scanned = scannedTotal(results).toLocaleString();
  const advice = 'Try fewer or broader words.';

  return {
    kind: 'miss',
    message: `Nothing open matches "${query}".`,
    hint:
      missing.length > 0
        ? `Searched ${scanned} open events across the ${replied.length} of ${results.length} venues that answered; ${named(missing)} did not, so this is not the whole book. ${advice}`
        : `Searched ${scanned} open events across ${results.length} venues. ${advice}`,
  };
}
