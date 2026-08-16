/**
 * Google Trends — what is actually being searched, right now.
 *
 * `KXRANKLISTGOOGLESEARCH` ("#1 searched person on Google in 2026"), its top-5
 * and #2 variants and `KXGOOGLESEARCH` carry ~568k open interest between them,
 * and every one of them names `trends.google.com` as its settlement source. The
 * terminal had no reading of it at all.
 *
 * Google publishes the trending list as RSS — `/trending/rss?geo=US` — which is
 * the one Trends surface that needs neither a key nor a rendered browser. The
 * JSON endpoints behind trends.google.com are batchexecute RPCs that require a
 * session token; the RSS feed is a stable public document, so that is what this
 * reads.
 *
 * **This is the daily list, not the annual one those markets settle on.** Google
 * publishes "Year in Search" once, in December. What the feed shows is who is
 * being searched today, which is the evidence a trader has in August for a
 * market that resolves in December — the same relationship `BO` has to a
 * total-gross market. The panel says so rather than implying it is the ranking.
 *
 * Each item carries an approximate traffic figure (`500+`, `2M+`) and the news
 * stories Google attributes the spike to. Both are kept: the headline is usually
 * the whole explanation for why a name appeared.
 */

import * as cheerio from 'cheerio';
import type { TrendEntry, TrendList } from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchText } from '../lib/http.js';

const BASE = process.env['GOOGLE_TRENDS_BASE'] ?? 'https://trends.google.com';

/**
 * Geographies worth naming. Google accepts any ISO-3166 alpha-2, and
 * {@link assertGeo} lets any of them through — this is the menu, not the limit.
 */
export const GEOS: { code: string; name: string }[] = [
  { code: 'US', name: 'United States' },
  { code: 'GB', name: 'United Kingdom' },
  { code: 'CA', name: 'Canada' },
  { code: 'AU', name: 'Australia' },
  { code: 'IN', name: 'India' },
  { code: 'JP', name: 'Japan' },
  { code: 'DE', name: 'Germany' },
  { code: 'FR', name: 'France' },
  { code: 'BR', name: 'Brazil' },
  { code: 'MX', name: 'Mexico' },
];

export function assertGeo(raw: string): string {
  const geo = (raw.trim() || 'US').toUpperCase();
  if (!/^[A-Z]{2}$/.test(geo)) {
    throw new UpstreamError(`"${raw}" is not a country code`, {
      code: 'bad_request',
      hint: 'Pass a two-letter ISO country code, e.g. `TRND US` or `TRND GB`.',
    });
  }
  return geo;
}

/**
 * `500+` → 500, `2M+` → 2000000, `20K+` → 20000.
 *
 * Google states these as lower bounds and never as exact counts, which is why
 * the field is named `trafficFloor` downstream. An unparseable value is `null`
 * rather than 0 — "Google did not say" and "nobody searched it" are different
 * facts, and only one of them is ever true here.
 */
export function parseTraffic(raw: string): number | null {
  const text = raw.trim().replace(/[+,\s]/g, '');
  const match = /^(\d+(?:\.\d+)?)([KMB]?)$/i.exec(text);
  if (!match) return null;

  const value = Number.parseFloat(match[1] as string);
  if (!Number.isFinite(value)) return null;

  const scale = { K: 1e3, M: 1e6, B: 1e9 }[(match[2] ?? '').toUpperCase()] ?? 1;
  return Math.round(value * scale);
}

/**
 * Parse the trending RSS.
 *
 * Cheerio is loaded in XML mode: the feed's payload lives in namespaced elements
 * (`ht:approx_traffic`, `ht:news_item_title`), and the HTML parser lowercases
 * and mangles the prefixes so that `ht:picture` and `ht:picture_source` both
 * collapse onto the same lookup. Selectors escape the colon for the same reason.
 */
export function parseTrendsRss(xml: string, geo: string, sourceUrl: string): TrendList {
  const $ = cheerio.load(xml, { xml: true });

  const items = $('item');
  if (items.length === 0) {
    throw new UpstreamError(`No trending items in the feed for ${geo}`, {
      code: 'parse_failed',
      hint:
        'Google answered but the feed carried no <item> elements. Either the geography ' +
        'has no trending list, or the feed format changed.',
    });
  }

  const entries: TrendEntry[] = [];

  items.each((index, node) => {
    const $item = $(node);
    const title = $item.find('title').first().text().trim();
    if (!title) return;

    const $news = $item.find('ht\\:news_item').first();

    entries.push({
      // The feed is ordered but unnumbered; position in the document *is* the
      // rank, which is why it is taken from the index rather than looked up.
      rank: index + 1,
      query: title,
      trafficFloor: parseTraffic($item.find('ht\\:approx_traffic').first().text()),
      startedAt: $item.find('pubDate').first().text().trim(),
      headline: $news.find('ht\\:news_item_title').first().text().trim(),
      headlineSource: $news.find('ht\\:news_item_source').first().text().trim(),
      headlineUrl: $news.find('ht\\:news_item_url').first().text().trim(),
      articles: $item.find('ht\\:news_item').length,
    });
  });

  if (entries.length === 0) {
    throw new UpstreamError(`No trending searches found for ${geo}`, {
      code: 'parse_failed',
      hint: 'The feed parsed but every item was missing a title.',
    });
  }

  return {
    geo,
    geoLabel: GEOS.find((g) => g.code === geo)?.name ?? geo,
    entries,
    sourceUrl,
  };
}

export async function getTrending(geo: string, limit = 25): Promise<TrendList> {
  const code = assertGeo(geo);
  const sourceUrl = `${BASE}/trending/rss?geo=${code}`;

  const list = await cache.cached(`trends:${code}`, TTL.trends, async () => {
    const xml = await fetchText(sourceUrl, { timeoutMs: 20_000, retries: 2 });
    return parseTrendsRss(xml, code, sourceUrl);
  });

  return { ...list, entries: list.entries.slice(0, limit) };
}
