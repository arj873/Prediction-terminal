/**
 * Spotify and YouTube charts — read from kworb.net.
 *
 * Kalshi runs a deep bench of streaming markets: `KXTOPARTIST`, `KXTOPSONGSPOTIFY`
 * and `KXARTISTSTREAMSY` (61 open events, ~1,350 contracts) settle on Spotify
 * figures, and `KXYTVIEWSW`, `KXYTTOPSONGW` and `KXYTDAILYTOPVIDEO` settle on
 * YouTube view counts.
 *
 * Neither platform publishes those numbers in a form a server can read:
 * charts.spotify.com moved behind a login, and charts.youtube.com renders
 * client-side. kworb.net has mirrored both daily for years and is the reference
 * the trading community actually quotes, so it is what this reads — with the
 * honest caveat that it is a third-party mirror, surfaced in the panel.
 *
 * Both sites share one table idiom: a header row naming the columns, a rank, a
 * `P+` movement cell (`=`, `+3`, `-1`, `NEW`), and an `Artist - Title` string.
 * Columns differ between charts — the daily Spotify table has `Days` and `7Day`
 * where the weekly has `Wks` and neither — so the parser resolves every column
 * by its *header text* rather than by position.
 */

import * as cheerio from 'cheerio';
import type { StreamChart, StreamChartListItem, StreamEntry } from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchText } from '../lib/http.js';

const BASE = 'https://kworb.net';

interface ChartSpec {
  slug: string;
  name: string;
  source: 'spotify' | 'youtube';
  path: string;
}

/** The charts worth a keystroke. `SPOT CHARTS` / `YT CHARTS` list these. */
export const KNOWN_CHARTS: ChartSpec[] = [
  // Spotify — `<country>_<daily|weekly>`; kworb carries ~70 countries.
  { slug: 'us-daily', name: 'Spotify Daily — United States', source: 'spotify', path: '/spotify/country/us_daily.html' },
  { slug: 'us-weekly', name: 'Spotify Weekly — United States', source: 'spotify', path: '/spotify/country/us_weekly.html' },
  { slug: 'global-daily', name: 'Spotify Daily — Global', source: 'spotify', path: '/spotify/country/global_daily.html' },
  { slug: 'global-weekly', name: 'Spotify Weekly — Global', source: 'spotify', path: '/spotify/country/global_weekly.html' },
  // YouTube.
  { slug: 'today', name: "YouTube — Today's Most Viewed Music Videos", source: 'youtube', path: '/youtube/' },
  { slug: 'alltime', name: 'YouTube — Most Viewed of All Time', source: 'youtube', path: '/youtube/topvideos.html' },
  { slug: 'trending', name: 'YouTube — Trending Worldwide', source: 'youtube', path: '/youtube/trending.html' },
];

export function listCharts(source?: 'spotify' | 'youtube'): StreamChartListItem[] {
  return KNOWN_CHARTS.filter((c) => !source || c.source === source).map((c) => ({
    slug: c.slug,
    name: c.name,
    source: c.source,
  }));
}

/** `us`, `gb`, `global` → the Spotify chart slug for that country. */
const COUNTRY = /^[a-z]{2}$/;

/**
 * Resolve a user-typed chart argument.
 *
 * `SPOT` → us-daily. `SPOT global` → global-daily. `SPOT gb weekly` →
 * gb-weekly. Anything already in the catalogue is used as-is.
 */
export function resolveSpotifyChart(scope: string, period: string): ChartSpec {
  const country = scope.trim().toLowerCase() || 'us';
  const cadence = period.trim().toLowerCase() === 'weekly' ? 'weekly' : 'daily';

  const known = KNOWN_CHARTS.find((c) => c.slug === `${country}-${cadence}` && c.source === 'spotify');
  if (known) return known;

  if (country !== 'global' && !COUNTRY.test(country)) {
    throw new UpstreamError(`"${scope}" is not a country code`, {
      code: 'bad_request',
      hint: 'Use a two-letter code or `global`, e.g. `SPOT gb weekly`.',
    });
  }

  return {
    slug: `${country}-${cadence}`,
    name: `Spotify ${cadence === 'weekly' ? 'Weekly' : 'Daily'} — ${country.toUpperCase()}`,
    source: 'spotify',
    path: `/spotify/country/${country}_${cadence}.html`,
  };
}

export function resolveYouTubeChart(view: string): ChartSpec {
  const slug = view.trim().toLowerCase() || 'today';
  const known = KNOWN_CHARTS.find((c) => c.slug === slug && c.source === 'youtube');
  if (known) return known;
  throw new UpstreamError(`"${view}" is not a YouTube chart`, {
    code: 'bad_request',
    hint: 'Try `YT`, `YT alltime` or `YT trending`.',
  });
}

/* ------------------------------------------------------------------ parsing */

/** `"1,466,839"` → 1466839; `"-"`, `""` → null. */
function intOrNull(text: string | undefined): number | null {
  if (!text) return null;
  const cleaned = text.replace(/[,\s+]/g, '');
  if (!cleaned || cleaned === '-' || cleaned === '--') return null;
  const n = Number.parseInt(cleaned, 10);
  return Number.isFinite(n) ? n : null;
}

/** Signed column such as `Streams+`: `"+84,270"` → 84270, `"-116,663"` → -116663. */
function signedOrNull(text: string | undefined): number | null {
  if (!text) return null;
  const cleaned = text.replace(/[,\s]/g, '');
  if (!cleaned || cleaned === '-' || cleaned === '--') return null;
  const n = Number.parseInt(cleaned, 10);
  return Number.isFinite(n) ? n : null;
}

/**
 * kworb's movement cell.
 *
 * `=` held, `+3` climbed three, `-1` slipped one, `NEW` is a debut. Returned in
 * the same convention the Billboard panel uses: positive means moved up.
 */
export function parseMove(text: string | undefined): { move: number | null; isNew: boolean } {
  const value = (text ?? '').trim().toUpperCase();
  if (value === 'NEW' || value === 'RE') return { move: null, isNew: true };
  // No movement column at all (the all-time table has none) is "unknown", which
  // is not the same as "held position" and must not render as `=`.
  if (value === '') return { move: null, isNew: false };
  if (value === '=') return { move: 0, isNew: false };
  const n = Number.parseInt(value.replace(/[^\d-]/g, ''), 10);
  if (!Number.isFinite(n)) return { move: 0, isNew: false };
  return { move: value.startsWith('-') ? -Math.abs(n) : Math.abs(n), isNew: false };
}

/**
 * Split `"Shakira - Dai Dai (w/ Burna Boy)"` into artist and title.
 *
 * Only the *first* separator counts: plenty of titles contain a dash of their
 * own, and splitting on the last one would file "Dai Dai (w/ Burna Boy)" under
 * the wrong artist. YouTube rows are frequently just a video name with no
 * separator at all, which stays whole as the title.
 */
export function splitArtistTitle(text: string): { artist: string; title: string } {
  const cleaned = text.replace(/\s+/g, ' ').trim();
  const index = cleaned.indexOf(' - ');
  if (index === -1) return { artist: '', title: cleaned };
  return {
    artist: cleaned.slice(0, index).trim(),
    title: cleaned.slice(index + 3).trim(),
  };
}

/**
 * Normalise a header cell to a lookup key: `"Artist and Title"` → `artistandtitle`.
 *
 * `+` becomes `plus` rather than being stripped. kworb pairs a value column with
 * its delta — `Streams` beside `Streams+`, `7Day` beside `7Day+` — and dropping
 * the sign collapses both onto one key, so the delta column becomes unreadable
 * and whichever came first silently answers for both.
 */
function headerKey(text: string): string {
  return text.toLowerCase().replace(/\+/g, 'plus').replace(/[^a-z0-9]/g, '');
}

export function parseChartTable(html: string, spec: ChartSpec, sourceUrl: string): StreamChart {
  const $ = cheerio.load(html);
  const $table = $('table').first();

  if ($table.length === 0) {
    throw new UpstreamError(`No chart table found on ${sourceUrl}`, {
      code: 'parse_failed',
      hint: 'kworb.net answered but served no chart table. The chart slug may be wrong.',
    });
  }

  // Column index by header name. kworb leaves the rank and movement headers
  // blank on some pages, so those two fall back to their fixed leading
  // positions — a convention that holds across every table on the site.
  const columns = new Map<string, number>();
  $table
    .find('th')
    .each((i, node) => {
      const key = headerKey($(node).text());
      if (key) {
        if (!columns.has(key)) columns.set(key, i);
      } else if (i === 0) columns.set('pos', 0);
      else if (i === 1) columns.set('move', 1);
    });

  const at = (cells: string[], ...names: string[]): string | undefined => {
    for (const name of names) {
      const index = columns.get(name);
      if (index !== undefined && cells[index] !== undefined) return cells[index];
    }
    return undefined;
  };

  const entries: StreamEntry[] = [];

  $table.find('tbody tr').each((_, node) => {
    const cells: string[] = [];
    $(node)
      .find('td')
      .each((__, td) => {
        cells.push($(td).text().replace(/\s+/g, ' ').trim());
      });
    if (cells.length === 0) return;

    const label = at(cells, 'artistandtitle', 'video', 'title', 'artist') ?? '';
    if (!label) return;

    const { artist, title } = splitArtistTitle(label);
    const { move, isNew } = parseMove(at(cells, 'move', 'pplus'));
    const rank = intOrNull(at(cells, 'pos')) ?? entries.length + 1;

    // `Views` is the period figure on the daily charts but the *cumulative*
    // total on the all-time table, where `Yesterday` carries the daily number.
    const hasYesterday = columns.has('yesterday');
    const periodStreams = hasYesterday
      ? intOrNull(at(cells, 'yesterday'))
      : intOrNull(at(cells, 'streams', 'views'));
    const total = hasYesterday
      ? intOrNull(at(cells, 'views'))
      : intOrNull(at(cells, 'total'));

    entries.push({
      rank,
      lastRank: move === null ? null : rank + move,
      title,
      artist,
      streams: periodStreams,
      streamsChange: signedOrNull(at(cells, 'streamsplus')),
      total,
      peak: intOrNull(at(cells, 'pk', 'peak')),
      days: intOrNull(at(cells, 'days', 'wks', 'weeks')),
      move,
      isNew,
    });
  });

  if (entries.length === 0) {
    throw new UpstreamError(`No chart entries found on ${sourceUrl}`, {
      code: 'parse_failed',
      hint:
        'kworb.net returned a page but its rows did not match the expected ' +
        'table shape. The chart may have been renamed, or the layout changed.',
    });
  }

  // kworb states the chart date in the page title on the dated charts.
  const pageTitle = $('title').text().replace(/\s+/g, ' ').trim();
  const date = /(\d{4}-\d{2}-\d{2})/.exec(pageTitle)?.[1] ?? '';

  return {
    source: spec.source,
    chart: spec.slug,
    title: pageTitle || spec.name,
    date,
    entries,
    sourceUrl,
  };
}

/**
 * Rows returned by default.
 *
 * kworb's chart pages are not all the same length: a Spotify country chart is
 * 200 rows, but the YouTube all-time table runs to several thousand. Handing
 * every one of those to the client means a panel that renders thousands of DOM
 * rows nobody scrolls to, so the tail is trimmed here rather than in the panel.
 */
const DEFAULT_LIMIT = 200;

export async function getChart(spec: ChartSpec, limit = DEFAULT_LIMIT): Promise<StreamChart> {
  const sourceUrl = `${BASE}${spec.path}`;
  const chart = await cache.cached(`kworb:${spec.source}:${spec.slug}`, TTL.streamCharts, async () => {
    const html = await fetchText(sourceUrl, { timeoutMs: 30_000, retries: 2 });
    return parseChartTable(html, spec, sourceUrl);
  });
  return { ...chart, entries: chart.entries.slice(0, Math.max(1, limit)) };
}

export type { ChartSpec };
