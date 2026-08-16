/**
 * Billboard charts — scraped from billboard.com/charts/<slug>[/<date>].
 *
 * Billboard's markup is Tailwind-ish utility soup, but the handful of
 * *semantic* class names carry structure and have been stable for years:
 *
 *   .o-chart-results-list-row-container   one entry (100 per page)
 *     li[0] > .c-label                    rank
 *     img.c-lazy-image__img               artwork
 *     h3.c-title                          title
 *     h3.c-title + .c-label               artist
 *     .c-span (LW | PEAK | WEEKS)         stat labels, value in the next .c-label
 *
 * The parser anchors on those and reads stats by their *label text* rather than
 * by position, so Billboard reordering or adding a column does not silently
 * shift "peak" into "weeks on chart".
 */

import * as cheerio from 'cheerio';
import type {
  BillboardChart,
  BillboardChartListItem,
  BillboardEntry,
} from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchText } from '../lib/http.js';

const BASE = 'https://www.billboard.com/charts';

/** Charts worth putting one keystroke away. `BB CHARTS` lists these. */
export const KNOWN_CHARTS: BillboardChartListItem[] = [
  { slug: 'hot-100', name: 'Billboard Hot 100' },
  { slug: 'billboard-200', name: 'Billboard 200 (albums)' },
  { slug: 'artist-100', name: 'Billboard Artist 100' },
  { slug: 'streaming-songs', name: 'Streaming Songs' },
  { slug: 'radio-songs', name: 'Radio Songs' },
  { slug: 'digital-song-sales', name: 'Digital Song Sales' },
  { slug: 'billboard-global-200', name: 'Billboard Global 200' },
  { slug: 'billboard-global-excl-us', name: 'Billboard Global Excl. US' },
  { slug: 'country-songs', name: 'Hot Country Songs' },
  { slug: 'rock-songs', name: 'Hot Rock Songs' },
  { slug: 'r-b-hip-hop-songs', name: 'Hot R&B/Hip-Hop Songs' },
  { slug: 'latin-songs', name: 'Hot Latin Songs' },
  { slug: 'dance-electronic-songs', name: 'Hot Dance/Electronic Songs' },
  { slug: 'pop-songs', name: 'Pop Airplay' },
  { slug: 'tiktok-billboard-top-50', name: 'TikTok Billboard Top 50' },
];

const SLUG = /^[a-z0-9][a-z0-9-]{0,60}$/;
const DATE = /^\d{4}-\d{2}-\d{2}$/;

export function assertSlug(slug: string): string {
  const s = slug.trim().toLowerCase();
  if (!SLUG.test(s)) {
    throw new UpstreamError(`"${slug}" is not a valid Billboard chart slug`, {
      code: 'bad_request',
      hint: 'Run `BB CHARTS` to list the charts this terminal knows about.',
    });
  }
  return s;
}

export function assertDate(date: string): string {
  const d = date.trim();
  if (!DATE.test(d) || Number.isNaN(Date.parse(d))) {
    throw new UpstreamError(`"${date}" is not a valid chart date`, {
      code: 'bad_request',
      hint: 'Chart dates are YYYY-MM-DD, e.g. `BB hot-100 2025-06-14`.',
    });
  }
  return d;
}

/** `"3"` → 3; `"-"`, `""`, `"NEW"` → null. */
function intOrNull(text: string | undefined): number | null {
  if (!text) return null;
  const cleaned = text.replace(/\s+/g, ' ').trim().replace(/,/g, '');
  if (!cleaned || cleaned === '-' || cleaned === '–' || cleaned === '—') return null;
  const n = Number.parseInt(cleaned, 10);
  return Number.isFinite(n) ? n : null;
}

function text($el: cheerio.Cheerio<never>): string {
  return $el.text().replace(/\s+/g, ' ').replace(/&#0?39;/g, "'").trim();
}

export function parseChartPage(
  html: string,
  chart: string,
  sourceUrl: string,
): BillboardChart {
  const $ = cheerio.load(html);

  // ---- chart-level metadata ---------------------------------------------
  const date =
    $('.chart-date-picker').attr('data-date') ??
    $('[data-date]').first().attr('data-date') ??
    '';

  const rawTitle =
    $('meta[property="og:title"]').attr('content') ??
    $('h1').first().text() ??
    $('title').first().text() ??
    chart;
  const title = rawTitle
    .replace(/\s*\|\s*Billboard.*$/i, '')
    .replace(/\s+Chart\s*$/i, '')
    .replace(/\s+/g, ' ')
    .trim();

  // ---- entries -----------------------------------------------------------
  const entries: BillboardEntry[] = [];

  $('.o-chart-results-list-row-container').each((_, container) => {
    const $row = $(container);

    // Rank lives in the first .c-label of the row — the oversized number in the
    // leading cell. Falling back to ordinal position keeps a row from being
    // dropped entirely if that cell is ever restyled.
    const rank = intOrNull(text($row.find('.c-label').first() as never)) ?? entries.length + 1;

    const $title = $row.find('h3.c-title, .c-title').first();
    const songTitle = text($title as never);

    // The artist sits in the .c-label directly after the title heading. It is a
    // link on most rows and bare text on some (unlinked artists).
    let artist = text($title.next('.c-label, span') as never);
    if (!artist) artist = text($title.parent().find('.c-label').first() as never);
    if (!artist) artist = text($title.parent().find('a[href*="/artist/"]').first() as never);

    // Some chart types (Artist 100) have no separate title/artist split: the
    // heading *is* the artist.
    if (!songTitle && !artist) return;

    // ---- stats, keyed by their printed label ----------------------------
    const stats = new Map<string, number | null>();
    $row.find('.c-span').each((__, span) => {
      const label = text($(span) as never).toUpperCase().replace(/[^A-Z]/g, '');
      if (!label) return;

      // The value is the next .c-label in document order within this row.
      const $scope = $(span).parent();
      const value = intOrNull(text($scope.find('.c-label').first() as never));
      if (!stats.has(label)) stats.set(label, value);
    });

    const lastWeek = stats.get('LW') ?? null;
    const peak = stats.get('PEAK') ?? null;
    const weeksOnChart = stats.get('WEEKS') ?? stats.get('WKS') ?? null;

    // Rows below the fold are lazy-loaded: `src` is a placeholder GIF and the
    // real artwork is parked in `data-lazy-src`. Prefer the latter.
    const $img = $row.find('img.c-lazy-image__img').first().length
      ? $row.find('img.c-lazy-image__img').first()
      : $row.find('img').first();
    const imageUrl = $img.attr('data-lazy-src') ?? $img.attr('src') ?? null;

    // On artist charts (Artist 100) the heading and the label below it are both
    // the artist name; collapse the duplicate rather than printing it twice.
    const isArtistOnly = !songTitle || songTitle.toLowerCase() === artist.toLowerCase();

    entries.push({
      rank,
      title: songTitle || artist,
      artist: isArtistOnly ? '' : artist,
      lastWeek,
      peak,
      weeksOnChart,
      imageUrl: imageUrl && imageUrl.startsWith('http') ? imageUrl : null,
      move: lastWeek !== null ? lastWeek - rank : null,
      isNew: lastWeek === null,
    });
  });

  if (entries.length === 0) {
    throw new UpstreamError(`No chart entries found on ${sourceUrl}`, {
      code: 'parse_failed',
      hint:
        'Billboard returned a page but its chart rows did not match the expected ' +
        'markup. The chart slug may be wrong, or the page layout changed.',
    });
  }

  entries.sort((a, b) => a.rank - b.rank);

  return { chart, title: title || chart, date, entries, sourceUrl };
}

export async function getChart(rawChart: string, rawDate?: string): Promise<BillboardChart> {
  const chart = assertSlug(rawChart);
  const date = rawDate ? assertDate(rawDate) : undefined;
  const sourceUrl = date ? `${BASE}/${chart}/${date}/` : `${BASE}/${chart}/`;

  return cache.cached(`billboard:${chart}:${date ?? 'latest'}`, TTL.billboard, async () => {
    const html = await fetchText(sourceUrl, { timeoutMs: 40_000, retries: 2, maxBytes: 24 * 1024 * 1024 });
    return parseChartPage(html, chart, sourceUrl);
  });
}
