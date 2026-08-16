/**
 * Podcast charts — Apple's own ranking.
 *
 * `KXTOPPOD`, `KXROGANGUEST`, `KXPODCASTGUESTCALLHERDADDY`,
 * `KXCALLHERDADDYCANCELED` and the rest of Kalshi's podcast book had no
 * companion feed at all — it was the one entertainment genre the terminal was
 * silent on, despite `ENT` happily listing the markets.
 *
 * Apple publishes the chart itself, as JSON, at `rss.marketingtools.apple.com`.
 * That is a first-party publication rather than a scrape, which puts it in the
 * same category as the Netflix TSVs: no markup to break, no mirror to caveat.
 *
 * Two charts, because they answer different questions. `top` is Apple's ranking
 * of shows overall — what `KXTOPPOD` is about. `episodes` is what is charting
 * right now, which is where a guest booking shows up, and a guest booking is
 * what most of these markets actually trade.
 */

import type { PodcastChart, PodcastEntry } from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';

const BASE = process.env['APPLE_RSS_BASE'] ?? 'https://rss.marketingtools.apple.com/api/v2';

/**
 * The two charts, keyed by the *resource* name.
 *
 * Apple's URL is `/{media}/{feed}/{limit}/{resource}.json`, and shows and
 * episodes differ only in the last segment — both live under `podcasts/top`.
 * The obvious guess, `podcast-episodes/top/…`, 404s.
 */
const VIEWS: Record<string, { resource: string; label: string }> = {
  top: { resource: 'podcasts', label: 'Top Shows' },
  episodes: { resource: 'podcast-episodes', label: 'Trending Episodes' },
};

export function assertView(raw: string): string {
  const view = (raw.trim() || 'top').toLowerCase();
  const normalised = view === 'shows' || view === 'show' ? 'top' : view === 'episode' ? 'episodes' : view;
  if (!(normalised in VIEWS)) {
    throw new UpstreamError(`"${raw}" is not a podcast chart`, {
      code: 'bad_request',
      hint: 'Views are: top, episodes. Usage: `POD [top|episodes] [country]`.',
    });
  }
  return normalised;
}

export function assertCountry(raw: string): string {
  const country = (raw.trim() || 'us').toLowerCase();
  if (!/^[a-z]{2}$/.test(country)) {
    throw new UpstreamError(`"${raw}" is not a country code`, {
      code: 'bad_request',
      hint: 'Pass a two-letter ISO country code, e.g. `POD top gb`.',
    });
  }
  return country;
}

interface RawEntry {
  name?: string;
  artistName?: string;
  releaseDate?: string;
  url?: string;
  genres?: { name?: string }[];
  contentAdvisoryRating?: string;
}

interface RawFeed {
  feed?: {
    title?: string;
    updated?: string;
    country?: string;
    results?: RawEntry[];
  };
}

/**
 * Apple's own advisory rating, which Apple spells wrong.
 *
 * The live episode chart returns `"Explict"` — missing the second `i` — for
 * every explicit row. An `=== 'explicit'` test therefore matches nothing and
 * silently reports a chart of exclusively clean content, which is wrong without
 * ever looking broken. Both spellings are accepted, and the prefix is matched so
 * a third one does not reintroduce the bug.
 */
function isExplicit(rating: string | undefined): boolean {
  return /^expl/i.test((rating ?? '').trim());
}

/**
 * Shape Apple's feed into ranked entries.
 *
 * Exported for tests. The rank is positional — Apple ships the chart in order
 * and never numbers it — and the genre list is flattened to its first entry,
 * because a table column has room for one and Apple orders them by relevance.
 */
export function parseFeed(body: RawFeed, view: string, country: string, sourceUrl: string): PodcastChart {
  const feed = body.feed;
  const results = feed?.results ?? [];

  if (results.length === 0) {
    throw new UpstreamError(`Apple returned an empty ${view} chart for ${country.toUpperCase()}`, {
      code: 'not_found',
      hint:
        'Apple answered but the chart had no entries. Not every country publishes ' +
        'every chart — try `POD top us`.',
    });
  }

  const entries: PodcastEntry[] = [];

  for (const [index, raw] of results.entries()) {
    const name = raw.name?.trim();
    if (!name) continue;

    entries.push({
      rank: index + 1,
      name,
      // On the episode chart this is the show; on the show chart it is the
      // publisher. Same field, and both are the thing you want beside the title.
      publisher: raw.artistName?.trim() ?? '',
      genre: raw.genres?.[0]?.name?.trim() ?? '',
      // Present on the show chart, absent on the episode chart — which is the
      // opposite of what you would guess, and why the panel only shows a date
      // column when the rows actually carry one.
      released: (raw.releaseDate ?? '').slice(0, 10),
      explicit: isExplicit(raw.contentAdvisoryRating),
      url: raw.url ?? '',
    });
  }

  if (entries.length === 0) {
    throw new UpstreamError(`No named entries in Apple's ${view} chart`, {
      code: 'parse_failed',
      hint: 'The feed parsed but every entry was missing a name.',
    });
  }

  return {
    view,
    viewLabel: (VIEWS[view] as { label: string }).label,
    country: country.toUpperCase(),
    title: feed?.title ?? '',
    updated: feed?.updated ?? '',
    entries,
    sourceUrl,
  };
}

export async function getChart(view: string, country: string, limit = 50): Promise<PodcastChart> {
  const resolvedView = assertView(view);
  const resolvedCountry = assertCountry(country);
  const resource = (VIEWS[resolvedView] as { resource: string }).resource;

  // Apple serves fixed sizes; 100 is the largest and is trimmed to `limit` here
  // so one upstream response can satisfy any panel's row count.
  const sourceUrl = `${BASE}/${resolvedCountry}/podcasts/top/100/${resource}.json`;

  const chart = await cache.cached(
    `podcasts:${resolvedView}:${resolvedCountry}`,
    TTL.podcasts,
    async () => {
      const body = await fetchJson<RawFeed>(sourceUrl, { timeoutMs: 20_000, retries: 2 });
      return parseFeed(body, resolvedView, resolvedCountry, sourceUrl);
    },
  );

  return { ...chart, entries: chart.entries.slice(0, limit) };
}
