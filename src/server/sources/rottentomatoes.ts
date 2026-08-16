/**
 * Rotten Tomatoes — scraped from rottentomatoes.com.
 *
 * This is the feed behind Kalshi's single busiest entertainment series. `KXRT`
 * lists a strike ladder per film ("Dune: Part Three ≥ 85%") and settles on the
 * Tomatometer, so the score and — just as importantly — *whether a score exists
 * yet* is the number those markets trade around.
 *
 * The page embeds its own state as JSON rather than only rendering it:
 *
 *   #media-scorecard-json   { criticsScore, audienceScore, description }
 *   ld+json                 { name, dateCreated, @type }
 *
 * Reading those beats scraping the rendered score badges, which are web
 * components whose shadow DOM never appears in the served HTML. A film with no
 * Tomatometer yet returns `score: null` rather than 0 — the distinction is the
 * whole point when the market is "will it score above 85".
 */

import * as cheerio from 'cheerio';
import type {
  RtScore,
  RtSearchResponse,
  RtSearchResult,
  RtTitle,
} from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchText } from '../lib/http.js';

const BASE = 'https://www.rottentomatoes.com';

/** RT slugs are `[a-z0-9_]` with the occasional hyphen, e.g. `dune_part_two`. */
const SLUG = /^[a-z0-9][a-z0-9_-]{0,120}$/;

/**
 * Turn a free-text title into RT's slug form.
 *
 * `Dune: Part Two` → `dune_part_two`. RT disambiguates same-named titles with a
 * year suffix (`dune_2021`), which this cannot know about — that is what the
 * search fallback in {@link getTitle} is for.
 */
export function slugify(input: string): string {
  return input
    .normalize('NFKD')
    .toLowerCase()
    .replace(/['’]/g, '')
    .replace(/&/g, ' and ')
    .replace(/[^a-z0-9]+/g, '_')
    .replace(/^_+|_+$/g, '')
    .slice(0, 120);
}

export function assertSlug(raw: string): string {
  const slug = raw.trim().toLowerCase();
  if (!SLUG.test(slug)) {
    throw new UpstreamError(`"${raw}" is not a valid Rotten Tomatoes slug`, {
      code: 'bad_request',
      hint: 'Try a title instead, e.g. `RT dune part three`.',
    });
  }
  return slug;
}

/** `"92"` → 92; `""`, `"--"`, absent → null. A film awaiting reviews has none. */
function scoreOrNull(value: unknown): number | null {
  if (typeof value === 'number') return Number.isFinite(value) ? value : null;
  if (typeof value !== 'string') return null;
  const cleaned = value.replace(/[%\s]/g, '');
  if (!cleaned) return null;
  const n = Number.parseInt(cleaned, 10);
  return Number.isFinite(n) ? n : null;
}

function intOrNull(value: unknown): number | null {
  if (typeof value === 'number') return Number.isFinite(value) ? value : null;
  if (typeof value !== 'string') return null;
  const n = Number.parseInt(value.replace(/[,\s]/g, ''), 10);
  return Number.isFinite(n) ? n : null;
}

interface RawScore {
  score?: string | number;
  averageRating?: string;
  reviewCount?: number | string;
  ratingCount?: number | string;
  bandedRatingCount?: string;
  sentiment?: string;
  certified?: boolean;
  certifiedFresh?: string;
  scoreType?: string;
}

/**
 * Normalise one of RT's two score blocks.
 *
 * The critic and audience payloads have the same shape but different
 * vocabularies — critics carry `certified`, the audience carries
 * `certifiedFresh: "certified"` — so both spellings are folded into one flag.
 */
function normaliseScore(raw: RawScore | undefined, certifiedLabel: string): RtScore {
  if (!raw) {
    return { score: null, averageRating: '', reviewCount: null, state: 'not yet scored', certified: false };
  }

  const score = scoreOrNull(raw.score);
  const certified = raw.certified === true || raw.certifiedFresh === 'certified';

  let state: string;
  if (score === null) state = 'not yet scored';
  else if (certified) state = certifiedLabel;
  else if (raw.sentiment) state = raw.sentiment.toLowerCase();
  else state = score >= 60 ? 'positive' : 'negative';

  return {
    score,
    averageRating: typeof raw.averageRating === 'string' ? raw.averageRating : '',
    reviewCount: intOrNull(raw.reviewCount) ?? intOrNull(raw.ratingCount),
    state,
    certified,
  };
}

interface LdJson {
  '@type'?: string;
  name?: string;
  dateCreated?: string;
  description?: string;
}

/** Pull the first ld+json block that describes the title itself. */
function readLdJson($: cheerio.CheerioAPI): LdJson {
  let found: LdJson = {};
  $('script[type="application/ld+json"]').each((_, node) => {
    if (found.name) return;
    try {
      const parsed: unknown = JSON.parse($(node).text());
      if (parsed && typeof parsed === 'object' && 'name' in parsed) found = parsed as LdJson;
    } catch {
      // A malformed block is not fatal — og: tags cover the same ground.
    }
  });
  return found;
}

export function parseTitlePage(html: string, slug: string, sourceUrl: string): RtTitle {
  const $ = cheerio.load(html);

  const scorecardRaw = $('#media-scorecard-json').first().text().trim();
  if (!scorecardRaw) {
    throw new UpstreamError(`No score data found on ${sourceUrl}`, {
      code: 'parse_failed',
      hint:
        'Rotten Tomatoes served a page without its score payload. The slug may ' +
        'point at something other than a film or show — try `RT <title>` to search.',
    });
  }

  let scorecard: { criticsScore?: RawScore; audienceScore?: RawScore; description?: string };
  try {
    scorecard = JSON.parse(scorecardRaw) as typeof scorecard;
  } catch {
    throw new UpstreamError(`Score payload on ${sourceUrl} is not valid JSON`, {
      code: 'parse_failed',
    });
  }

  const ld = readLdJson($);

  const ogTitle = $('meta[property="og:title"]').attr('content') ?? '';
  const title =
    ld.name ??
    ogTitle.replace(/\s*\|\s*Rotten Tomatoes\s*$/i, '').trim() ??
    slug.replace(/_/g, ' ');

  // `dateCreated` is a full ISO date; the year is all a terminal column needs.
  const year = /^(\d{4})/.exec(ld.dateCreated ?? '')?.[1] ?? '';

  const ogType = $('meta[property="og:type"]').attr('content') ?? '';
  const mediaType =
    ld['@type'] === 'TVSeries' || ogType.includes('tv_show') || slug.startsWith('tv/')
      ? 'TV'
      : 'Movie';

  return {
    slug,
    title: title || slug,
    year,
    mediaType,
    critics: normaliseScore(scorecard.criticsScore, 'certified fresh'),
    audience: normaliseScore(scorecard.audienceScore, 'verified hot'),
    synopsis: (scorecard.description ?? ld.description ?? '').replace(/\s+/g, ' ').trim(),
    sourceUrl,
  };
}

export function parseSearchPage(html: string, query: string): RtSearchResponse {
  const $ = cheerio.load(html);
  const results: RtSearchResult[] = [];

  // Each hit is a <search-page-media-row> custom element. The scores and years
  // live in its *attributes*, which survive in the served HTML even though the
  // element's own rendering does not.
  $('search-page-media-row').each((_, node) => {
    const $row = $(node);
    const href = $row.find('a[data-qa="thumbnail-link"]').attr('href') ?? $row.find('a').first().attr('href') ?? '';
    const match = /rottentomatoes\.com\/(m|tv)\/([a-z0-9_-]+)/i.exec(href);
    if (!match) return;

    const name =
      $row.find('[data-qa="info-name"]').first().text().trim() ||
      $row.find('img').first().attr('alt') ||
      '';
    if (!name) return;

    results.push({
      slug: match[1] === 'tv' ? `tv/${match[2]}` : (match[2] ?? ''),
      title: name,
      year: $row.attr('release-year') ?? $row.attr('start-year') ?? '',
      mediaType: match[1] === 'tv' ? 'TV' : 'Movie',
      criticsScore: scoreOrNull($row.attr('tomatometer-score')),
    });
  });

  return { query, results };
}

export async function search(query: string, limit = 20): Promise<RtSearchResponse> {
  const q = query.trim();
  if (!q) {
    throw new UpstreamError('Missing search text', {
      code: 'bad_request',
      hint: 'Usage: `RT <title>`, e.g. `RT dune part three`.',
    });
  }

  const sourceUrl = `${BASE}/search?search=${encodeURIComponent(q)}`;
  const found = await cache.cached(`rt:search:${q.toLowerCase()}`, TTL.rottenTomatoes, async () => {
    const html = await fetchText(sourceUrl, { timeoutMs: 30_000, retries: 2 });
    return parseSearchPage(html, q);
  });

  return { query: found.query, results: found.results.slice(0, limit) };
}

/**
 * Fetch a title by slug, falling back to search when the guessed slug 404s.
 *
 * A user types `RT dune part three`, not `RT dune_part_three`, and RT's slugs
 * carry disambiguating suffixes (`dune_2021`) that no slugifier can predict. So
 * the direct guess is tried first because it is one request and usually right,
 * and search is the backstop that makes the natural phrasing work.
 */
export async function getTitle(input: string): Promise<RtTitle> {
  const raw = input.trim();
  if (!raw) {
    throw new UpstreamError('Missing title', {
      code: 'bad_request',
      hint: 'Usage: `RT <title>`, e.g. `RT dune part three`.',
    });
  }

  const looksLikeSlug = /^(tv\/)?[a-z0-9][a-z0-9_-]*$/.test(raw.toLowerCase()) && raw.includes('_');
  const guess = looksLikeSlug ? raw.toLowerCase() : slugify(raw);

  try {
    return await fetchTitle(guess);
  } catch (err) {
    // Only a miss is worth a second request; a block or a timeout would just
    // fail again, more slowly.
    if (!(err instanceof UpstreamError) || err.code !== 'not_found') throw err;
  }

  const { results } = await search(raw, 5);
  const hit = results[0];
  if (!hit) {
    throw new UpstreamError(`Rotten Tomatoes has no title matching "${raw}"`, {
      code: 'not_found',
      hint: 'Check the spelling, or open rottentomatoes.com and use the slug from the URL.',
    });
  }
  return fetchTitle(hit.slug);
}

async function fetchTitle(slugOrPath: string): Promise<RtTitle> {
  // `tv/<slug>` addresses a series; everything else is a film under `/m/`.
  const isTv = slugOrPath.startsWith('tv/');
  const slug = assertSlug(isTv ? slugOrPath.slice(3) : slugOrPath);
  const path = isTv ? `tv/${slug}` : `m/${slug}`;
  const sourceUrl = `${BASE}/${path}`;

  return cache.cached(`rt:title:${path}`, TTL.rottenTomatoes, async () => {
    const html = await fetchText(sourceUrl, { timeoutMs: 30_000, retries: 2 });
    return parseTitlePage(html, isTv ? `tv/${slug}` : slug, sourceUrl);
  });
}
