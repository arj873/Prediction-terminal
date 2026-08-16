/**
 * Release dates — the music calendar, from Apple's iTunes Search API.
 *
 * A whole cluster of Kalshi series asks *when*, not *how well*:
 * `KXALBUMRELEASEDATE*` ("When will Tate McRae release a new album?"),
 * `KXSONGRELEASE*`, `KXNEWTAYLOR`, `KXALBUMRELEASE*`, `KXCATALOGUE`. They settle
 * on `open.spotify.com`, and the terminal's existing music feeds cannot answer
 * them: `BB` and `SPOT` rank what is already out, so a market about an
 * unreleased record has nothing to read.
 *
 * Spotify's own API needs an OAuth client, which would be the first credential
 * in this codebase. Apple's Search API needs nothing, covers the same
 * catalogue, and — the part that matters here — **lists pre-orders with their
 * announced future release date**. An album dated three weeks out is the single
 * most direct piece of evidence a "will they release by X" market has.
 *
 * The endpoint's `sort=recent` parameter is accepted and then ignored; results
 * come back by relevance whatever you pass. Sorting is therefore done here, and
 * the tests pin that, because a "latest release" panel that quietly shows the
 * most *popular* release would be wrong in a way nobody would notice.
 */

import type { Release, ReleaseList } from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';

const BASE = process.env['ITUNES_API_BASE'] ?? 'https://itunes.apple.com';

/**
 * What to look for. Apple calls these entities; the terminal calls them kinds.
 *
 * Music only. Apple's Search API still documents a `movie` entity and still
 * answers `200 OK` for one — with `resultCount: 0` for every title tried,
 * including Avatar and Inception, while the same request for music returns
 * normally. The film catalogue is gone from this endpoint, so `REL` does not
 * offer a film kind: an argument that always resolves to "not found" is worse
 * than not having the argument, and it would have made the film release-date
 * markets look covered when they are not.
 */
const KINDS: Record<string, { entity: string; label: string }> = {
  album: { entity: 'album', label: 'Albums' },
  song: { entity: 'song', label: 'Songs' },
};

export function assertKind(raw: string): string {
  const kind = (raw.trim() || 'album').toLowerCase();
  const normalised = kind.replace(/s$/, '');
  if (!(normalised in KINDS)) {
    throw new UpstreamError(`"${raw}" is not a release kind`, {
      code: 'bad_request',
      hint: `Kinds are: ${Object.keys(KINDS).join(', ')}. Usage: \`REL <artist> [album|song|movie]\`.`,
    });
  }
  return normalised;
}

interface RawResult {
  wrapperType?: string;
  collectionName?: string;
  trackName?: string;
  artistName?: string;
  releaseDate?: string;
  trackCount?: number;
  primaryGenreName?: string;
  collectionPrice?: number;
  currency?: string;
  collectionViewUrl?: string;
  trackViewUrl?: string;
  collectionExplicitness?: string;
  country?: string;
}

interface RawBody {
  resultCount?: number;
  results?: RawResult[];
}

/**
 * A release before the calendar is consulted.
 *
 * `upcoming` is deliberately not set here. It is a comparison against *now*, and
 * this value goes into a TTL cache — baking it in at parse time would leave a
 * record that shipped this morning still flagged as unreleased until the entry
 * expired. It is filled in on the way out instead.
 */
type ParsedRelease = Omit<Release, 'upcoming'>;

/** The cached shape: everything but the clock-dependent field. */
type CachedList = Omit<ReleaseList, 'releases'> & { releases: ParsedRelease[] };

/**
 * Shape one Apple result.
 *
 * `collectionName` is the album, `trackName` the song, and a song result carries
 * both — so the title is chosen by what was asked for rather than by whichever
 * field happens to be populated.
 */
function toRelease(raw: RawResult, kind: string): ParsedRelease | null {
  const title = kind === 'song' ? raw.trackName : (raw.collectionName ?? raw.trackName);
  if (!title) return null;

  const iso = raw.releaseDate ?? '';
  const date = /^\d{4}-\d{2}-\d{2}/.test(iso) ? iso.slice(0, 10) : '';
  if (!date) return null;

  return {
    title,
    artist: raw.artistName ?? '',
    date,
    kind,
    trackCount: typeof raw.trackCount === 'number' ? raw.trackCount : null,
    genre: raw.primaryGenreName ?? '',
    url: raw.collectionViewUrl ?? raw.trackViewUrl ?? '',
  };
}

/** Lowercase alphanumerics only, for comparing two spellings of a name. */
function fold(text: string): string {
  return text.toLowerCase().replace(/[^a-z0-9]/g, '');
}

/**
 * Is this result actually by the artist that was asked for?
 *
 * `attribute=artistTerm` narrows the search but does not close it: a query for
 * "taylor swift" still comes back with five tribute and covers acts in fifty
 * results, and because they release constantly they sort to the *top* of a
 * newest-first list. The panel would then answer "what did Taylor Swift release
 * most recently" with a covers compilation by a band called 8waves.
 *
 * Containment either way, so a collaboration credited to "Drake & Future" still
 * counts as a Drake release.
 */
function byArtist(artist: string, query: string): boolean {
  const a = fold(artist);
  const q = fold(query);
  if (!a || !q) return false;
  return a.includes(q) || q.includes(a);
}

/**
 * Parse a search body into releases, newest first.
 *
 * Exported for tests. Three behaviours are load-bearing here and none of them
 * fails loudly when it breaks: the artist filter above, the sort (Apple returns
 * relevance order whatever you pass for `sort`), and the dedupe.
 *
 * `artist` is the name to hold results to, and is omitted for a film search —
 * there the search term is a title, and filtering titles against the studio
 * would drop everything.
 */
export function parseReleases(body: RawBody, kind: string, artist?: string): ParsedRelease[] {
  const seen = new Set<string>();
  const releases: ParsedRelease[] = [];

  for (const raw of body.results ?? []) {
    const release = toRelease(raw, kind);
    if (!release) continue;
    if (artist !== undefined && !byArtist(release.artist, artist)) continue;

    // Exact duplicates only — same title, same day, which is what a record
    // listed across storefronts looks like.
    //
    // Collapsing near-matches was tried and reverted: stripping the bracketed
    // suffix folds "1989" together with "1989 (Taylor's Version)", and a
    // re-recording is a separate release that separate markets trade. Two rows
    // that look alike are a cosmetic annoyance; a hidden release is a wrong
    // answer to "has it come out yet".
    const key = `${release.title.trim().toLowerCase()}|${release.date}`;
    if (seen.has(key)) continue;
    seen.add(key);
    releases.push(release);
  }

  return releases.sort((a, b) => b.date.localeCompare(a.date));
}

/**
 * An artist's releases, newest first, with anything still ahead flagged.
 *
 * `upcoming` is computed against the server clock rather than trusting Apple to
 * mark a pre-order: the field that would say so is not in the search payload,
 * but a release date in the future means exactly the same thing.
 */
export async function getReleases(query: string, kind: string, limit = 25): Promise<ReleaseList> {
  const q = query.trim();
  if (!q) {
    throw new UpstreamError('Missing artist', {
      code: 'bad_request',
      hint: 'Usage: `REL <artist> [album|song]`, e.g. `REL taylor swift`.',
    });
  }

  const resolved = assertKind(kind);
  const entity = (KINDS[resolved] as { entity: string }).entity;

  // `attribute=artistTerm` is what makes this a discography rather than a
  // keyword search. Without it "taylor swift" matches every covers compilation
  // and piano-tribute album with her name in the title, and the newest of those
  // — not her newest record — is what a "latest release" panel would show.
  const url =
    `${BASE}/search?term=${encodeURIComponent(q)}&entity=${entity}&attribute=artistTerm` +
    `&limit=${Math.min(200, Math.max(limit * 4, 50))}&country=US&media=music`;

  const list = await cache.cached(
    `releases:${resolved}:${q.toLowerCase()}`,
    TTL.releases,
    async () => {
      const body = await fetchJson<RawBody>(url, { timeoutMs: 20_000, retries: 2 });
      const releases = parseReleases(body, resolved, q);

      if (releases.length === 0) {
        throw new UpstreamError(`Apple has no ${resolved} releases credited to "${q}"`, {
          code: 'not_found',
          hint:
            'Check the artist spelling. Results are held to the artist named, so an ' +
            'album title searched by mistake finds nothing: try `REL <artist>`.',
        });
      }

      return {
        query: q,
        kind: resolved,
        kindLabel: (KINDS[resolved] as { label: string }).label,
        releases,
        sourceUrl: `https://music.apple.com/us/search?term=${encodeURIComponent(q)}`,
      } satisfies CachedList;
    },
  );

  const today = new Date().toISOString().slice(0, 10);
  const releases = list.releases
    .slice(0, limit)
    .map((r) => ({ ...r, upcoming: r.date > today }));

  return { ...list, releases };
}
