/**
 * Netflix Top 10 — the official weekly publication behind netflix.com/tudum.
 *
 * Netflix publishes the same data its Top 10 site renders as plain TSV, which
 * is what this reads. That matters for a settlement feed: the TSV is the
 * *stated* source, it carries the exact figures (views, hours viewed, weeks in
 * the top 10) that Kalshi's `KXNETFLIXRANK*` and `KXNETFLIXTOPVIEWS*` markets
 * resolve against, and it does not depend on scraping a JavaScript-rendered
 * page that could change shape any week.
 *
 * Two files, because Netflix splits the data differently by scope:
 *
 *   all-weeks-global.tsv      ~1 MB   ranks *and* views/hours, English + non-English
 *   all-weeks-countries.tsv   ~31 MB  ranks only, ~93 countries
 *
 * The country file is large enough to be worth handling deliberately: it is
 * fetched at most once per {@link TTL.netflix}, reduced immediately to the most
 * recent week for *every* country, and only that reduction is cached. So `NFLX
 * us` and `NFLX gb` share one download, and the 31 MB never lives in the cache.
 */

import type { NetflixEntry, NetflixTop10 } from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchText } from '../lib/http.js';

const BASE = 'https://www.netflix.com/tudum/top10';
const GLOBAL_TSV = `${BASE}/data/all-weeks-global.tsv`;
const COUNTRIES_TSV = `${BASE}/data/all-weeks-countries.tsv`;

/**
 * The country file is ~31 MB; give it room without lifting the global ceiling.
 *
 * Sized to the data plus growth, not to a round number well clear of it. The
 * body is decoded into one JS string before it is parsed, so at UTF-16 this is
 * still the largest single allocation the process can be asked to make — the
 * former 96 MiB put that near 192 MB of heap for one request, for headroom
 * three times wider than the file has ever needed. Making this a streaming
 * parse would remove the class outright; halving the ceiling only bounds it,
 * and is what the trusted, fixed upstream actually warrants.
 */
const COUNTRIES_MAX_BYTES = 48 * 1024 * 1024;

export type NetflixCategory = 'tv' | 'films';

const COUNTRY = /^[A-Za-z]{2}$/;

export function assertCategory(raw: string): NetflixCategory {
  const value = raw.trim().toLowerCase();
  if (value === 'tv' || value === 'shows' || value === 'show' || value === 'series') return 'tv';
  if (value === 'films' || value === 'film' || value === 'movies' || value === 'movie') return 'films';
  throw new UpstreamError(`"${raw}" is not a Netflix category`, {
    code: 'bad_request',
    hint: 'Categories are `tv` and `films`, e.g. `NFLX tv us`.',
  });
}

/** `global`, or a two-letter country code. */
export function assertScope(raw: string): string {
  const value = raw.trim().toLowerCase();
  if (value === '' || value === 'global' || value === 'world') return 'global';
  if (COUNTRY.test(value)) return value;
  throw new UpstreamError(`"${raw}" is not a valid Netflix scope`, {
    code: 'bad_request',
    hint: 'Use `global` or a two-letter country code, e.g. `NFLX tv us`.',
  });
}

function numOrNull(value: string | undefined): number | null {
  if (!value) return null;
  const cleaned = value.replace(/[,\s]/g, '');
  if (!cleaned || cleaned === 'N/A') return null;
  const n = Number(cleaned);
  return Number.isFinite(n) ? n : null;
}

/** Netflix writes an absent season as the literal string `N/A`. */
function textOrEmpty(value: string | undefined): string {
  const v = (value ?? '').trim();
  return v === 'N/A' ? '' : v;
}

/**
 * Parse a TSV into rows keyed by the header line.
 *
 * Exported so the parsers can be tested against a captured fixture without a
 * network round trip, which is the only way this stays covered on a machine
 * that cannot reach netflix.com.
 */
export function parseTsv(tsv: string): Record<string, string>[] {
  const lines = tsv.split('\n').filter((line) => line.trim() !== '');
  const header = lines.shift();
  if (!header) return [];

  const columns = header.split('\t').map((c) => c.trim());
  return lines.map((line) => {
    const cells = line.split('\t');
    const row: Record<string, string> = {};
    columns.forEach((name, i) => {
      row[name] = (cells[i] ?? '').trim();
    });
    return row;
  });
}

/**
 * Netflix repeats the show name inside `season_title`
 * (`"Wednesday: Season 2"`, and sometimes the bare title again). Showing both
 * columns verbatim prints the name twice in every TV row, so the redundant
 * prefix is trimmed down to just the part that says which season.
 */
function seasonLabel(showTitle: string, seasonTitle: string): string {
  const season = textOrEmpty(seasonTitle);
  if (!season || !showTitle) return season;
  if (season === showTitle) return '';
  const prefix = `${showTitle}: `;
  return season.startsWith(prefix) ? season.slice(prefix.length) : season;
}

function toEntry(row: Record<string, string>): NetflixEntry {
  const title = row['show_title'] ?? '';
  return {
    rank: numOrNull(row['weekly_rank']) ?? 0,
    title,
    season: seasonLabel(title, row['season_title'] ?? ''),
    views: numOrNull(row['weekly_views']),
    hoursViewed: numOrNull(row['weekly_hours_viewed']),
    runtime: numOrNull(row['runtime']),
    weeksInTop10: numOrNull(row['cumulative_weeks_in_top_10']),
  };
}

/**
 * The global file splits by language (`TV (English)`, `Films (Non-English)`),
 * which is a distinction the terminal does not need: a Top 10 is a Top 10. Rows
 * are matched on the leading word and re-ranked across both language lists.
 */
function matchesGlobalCategory(category: string, want: NetflixCategory): boolean {
  const head = category.trim().toLowerCase();
  return want === 'tv' ? head.startsWith('tv') : head.startsWith('film');
}

interface GlobalSnapshot {
  week: string;
  rows: Record<string, string>[];
}

async function globalSnapshot(): Promise<GlobalSnapshot> {
  return cache.cached('netflix:global', TTL.netflix, async () => {
    const tsv = await fetchText(GLOBAL_TSV, {
      timeoutMs: 60_000,
      retries: 2,
      maxBytes: 8 * 1024 * 1024,
    });
    const rows = parseTsv(tsv);
    if (rows.length === 0) {
      throw new UpstreamError('Netflix returned an empty Top 10 dataset', { code: 'parse_failed' });
    }
    const week = rows.reduce((latest, r) => ((r['week'] ?? '') > latest ? (r['week'] ?? '') : latest), '');
    return { week, rows: rows.filter((r) => r['week'] === week) };
  });
}

interface CountrySnapshot {
  week: string;
  /** ISO alpha-2 (lowercase) → that country's latest-week rows. */
  byCountry: Record<string, Record<string, string>[]>;
  names: Record<string, string>;
}

/**
 * Latest week for every country, from one download.
 *
 * The reduction happens before anything is cached, so the 31 MB body is
 * transient and what survives is ~1,900 rows.
 */
async function countrySnapshot(): Promise<CountrySnapshot> {
  return cache.cached('netflix:countries', TTL.netflix, async () => {
    const tsv = await fetchText(COUNTRIES_TSV, {
      timeoutMs: 120_000,
      retries: 1,
      maxBytes: COUNTRIES_MAX_BYTES,
    });
    const rows = parseTsv(tsv);
    if (rows.length === 0) {
      throw new UpstreamError('Netflix returned an empty country dataset', { code: 'parse_failed' });
    }

    const week = rows.reduce((latest, r) => ((r['week'] ?? '') > latest ? (r['week'] ?? '') : latest), '');
    const byCountry: Record<string, Record<string, string>[]> = {};
    const names: Record<string, string> = {};

    for (const row of rows) {
      if (row['week'] !== week) continue;
      const code = (row['country_iso2'] ?? '').toLowerCase();
      if (!code) continue;
      (byCountry[code] ??= []).push(row);
      names[code] = row['country_name'] ?? code.toUpperCase();
    }

    return { week, byCountry, names };
  });
}

export async function getTop10(
  category: NetflixCategory,
  scope: string,
): Promise<NetflixTop10> {
  const categoryLabel = category === 'tv' ? 'TV' : 'Films';

  if (scope === 'global') {
    const snapshot = await globalSnapshot();
    const entries = snapshot.rows
      .filter((r) => matchesGlobalCategory(r['category'] ?? '', category))
      .map(toEntry)
      // Both language lists rank 1..10 independently; ordering by views and
      // re-ranking produces the single combined Top 10 people expect.
      .sort((a, b) => (b.views ?? 0) - (a.views ?? 0))
      .slice(0, 10)
      .map((entry, i) => ({ ...entry, rank: i + 1 }));

    if (entries.length === 0) {
      throw new UpstreamError(`Netflix published no global ${categoryLabel} rows for ${snapshot.week}`, {
        code: 'parse_failed',
      });
    }

    return {
      scope: 'global',
      scopeLabel: 'Global',
      category,
      categoryLabel,
      week: snapshot.week,
      entries,
      sourceUrl: GLOBAL_TSV,
    };
  }

  const snapshot = await countrySnapshot();
  const rows = snapshot.byCountry[scope];
  if (!rows) {
    throw new UpstreamError(`Netflix does not publish a Top 10 for "${scope.toUpperCase()}"`, {
      code: 'not_found',
      hint: 'Netflix reports about 93 countries. Try `NFLX tv us` or `NFLX films global`.',
    });
  }

  // The country file spells the categories plainly as `TV` and `Films`.
  const want = category === 'tv' ? 'tv' : 'films';
  const entries = rows
    .filter((r) => (r['category'] ?? '').trim().toLowerCase() === want)
    .map(toEntry)
    .sort((a, b) => a.rank - b.rank)
    .slice(0, 10);

  if (entries.length === 0) {
    throw new UpstreamError(
      `Netflix published no ${categoryLabel} rows for ${scope.toUpperCase()} in ${snapshot.week}`,
      { code: 'parse_failed' },
    );
  }

  return {
    scope,
    scopeLabel: snapshot.names[scope] ?? scope.toUpperCase(),
    category,
    categoryLabel,
    week: snapshot.week,
    entries,
    sourceUrl: COUNTRIES_TSV,
  };
}
