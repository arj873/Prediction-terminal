/**
 * FRED (Federal Reserve Economic Data) — scraped from fred.stlouisfed.org.
 *
 * Two surfaces, deliberately different in how much they can be trusted:
 *
 *  - **Observations** come from `/graph/fredgraph.csv?id=<ID>`, the same
 *    endpoint the "Download → CSV" button on every FRED graph page uses. It is
 *    a two-column CSV and has been stable for years. This is the load-bearing
 *    path.
 *  - **Metadata** (units, frequency, seasonal adjustment, notes) is parsed out
 *    of the `/series/<ID>` HTML page, whose markup is not a contract. The
 *    parser therefore tries several strategies and degrades to empty strings
 *    rather than throwing — a missing "Units:" line must never cost you the
 *    data itself.
 *
 * If `FRED_API_KEY` is set, the official API is used as a fallback whenever a
 * scrape fails. That matters in practice: fred.stlouisfed.org resets
 * connections from datacentre IP ranges, so a cloud-hosted deployment often
 * cannot scrape it at all even though `api.stlouisfed.org` answers fine.
 */

import * as cheerio from 'cheerio';
import type {
  DataObservation as FredObservation,
  DataSearchResult as FredSearchResult,
  DataSeries as FredSeries,
  DataSeriesResponse as FredSeriesResponse,
} from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson, fetchText } from '../lib/http.js';
import {
  firstAnswer,
  type Attributed,
  type ChainOptions,
  type Provider,
} from '../lib/providers.js';

/**
 * Overridable so the scrape path can be exercised against a fixture server.
 * The live host refuses connections from datacentre IPs, which would otherwise
 * leave the most important code path in this file untested in CI.
 */
const WEB_BASE = process.env.FRED_WEB_BASE ?? 'https://fred.stlouisfed.org';
const API_BASE = process.env.FRED_API_BASE ?? 'https://api.stlouisfed.org/fred';

const apiKey = (): string | undefined => process.env.FRED_API_KEY?.trim() || undefined;

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/** FRED series IDs are alphanumerics plus `_` and `.` — reject anything else. */
const SERIES_ID = /^[A-Za-z0-9_.\-]{1,64}$/;

/** The descriptive half of a series: everything not decided by which arm answered. */
type FredMeta = Omit<FredSeries, 'provider' | 'source' | 'sourceUrl'>;

/** FRED's own page for a series — the thing a reader clicks to check a number. */
export function seriesUrl(id: string): string {
  return `https://fred.stlouisfed.org/series/${encodeURIComponent(id)}`;
}

export function assertSeriesId(id: string): string {
  const trimmed = id.trim().toUpperCase();
  if (!SERIES_ID.test(trimmed)) {
    throw new UpstreamError(`"${id}" is not a valid FRED series id`, {
      code: 'bad_request',
      hint: 'Series ids look like UNRATE, GDPC1 or DGS10. Try `FSRCH <words>` to find one.',
    });
  }
  return trimmed;
}

/* ------------------------------------------------------------ observations */

/**
 * Parse the fredgraph CSV.
 *
 * Header is `observation_date,<SERIES_ID>` on current FRED and `DATE,<ID>` on
 * older exports; both are handled by ignoring the header names entirely and
 * taking column 0 as the date and column 1 as the value. A `.` is FRED's
 * missing-value marker and becomes `null`.
 */
export function parseFredCsv(csv: string): FredObservation[] {
  const out: FredObservation[] = [];
  const lines = csv.split(/\r?\n/);

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!.trim();
    if (!line) continue;

    const comma = line.indexOf(',');
    if (comma === -1) continue;

    const date = line.slice(0, comma).trim().replace(/^"|"$/g, '');
    const rawValue = line.slice(comma + 1).trim().replace(/^"|"$/g, '');

    // Skip the header row (and any repeat of it) by requiring a real date.
    if (!/^\d{4}-\d{2}-\d{2}$/.test(date)) continue;

    if (rawValue === '.' || rawValue === '') {
      out.push({ date, value: null });
      continue;
    }
    const value = Number(rawValue.replace(/,/g, ''));
    out.push({ date, value: Number.isFinite(value) ? value : null });
  }

  return out;
}

async function scrapeObservations(id: string, start?: string, end?: string): Promise<FredObservation[]> {
  const url = new URL(`${WEB_BASE}/graph/fredgraph.csv`);
  url.searchParams.set('id', id);
  if (start) url.searchParams.set('cosd', start);
  if (end) url.searchParams.set('coed', end);

  const csv = await fetchText(url.toString(), {
    timeoutMs: 30_000,
    retries: 2,
    headers: { Accept: 'text/csv,text/plain,*/*', Referer: `${WEB_BASE}/series/${id}` },
  });

  // A wrong id yields an HTML error page rather than a 404.
  if (/^\s*</.test(csv)) {
    throw new UpstreamError(`FRED returned a page instead of CSV for ${id}`, {
      code: 'not_found',
      hint: `No FRED series called ${id}. Try \`FSRCH <words>\` to search.`,
    });
  }

  const observations = parseFredCsv(csv);
  if (observations.length === 0) {
    throw new UpstreamError(`FRED returned no observations for ${id}`, { code: 'empty_upstream' });
  }
  return observations;
}

/* --------------------------------------------------------------- metadata */

/**
 * Pull metadata off the series page.
 *
 * FRED renders each attribute as a `<p>` containing a bold/`<span>` label
 * ("Units:") followed by the value. Class names have churned over the years, so
 * the primary strategy is label-text matching over the whole document, with
 * class-based selectors as a backstop.
 */
export function parseSeriesPage(html: string, id: string): FredMeta {
  const $ = cheerio.load(html);

  const clean = (s: string | undefined | null): string =>
    (s ?? '').replace(/\s+/g, ' ').replace(/ /g, ' ').trim();

  // ---- title -------------------------------------------------------------
  let title =
    clean($('#page-title').first().text()) ||
    clean($('h1.series-title, .series-title h1, h1[itemprop="name"]').first().text());

  if (!title) {
    // `<title>Unemployment Rate (UNRATE) | FRED | St. Louis Fed`
    title =
      clean($('meta[property="og:title"]').attr('content')) || clean($('title').first().text());
  }

  // Both the `<h1>` and the `<title>` append the series id in parentheses, and
  // the document title also carries the site suffix. The panel header already
  // shows the id, so strip both.
  title = title
    .replace(/\s*\|\s*FRED.*$/i, '')
    .replace(/\s*\|\s*St\.?\s*Louis\s*Fed.*$/i, '')
    .replace(new RegExp(`\\s*\\(${escapeRegExp(id)}\\)\\s*$`, 'i'), '')
    .trim();

  // ---- labelled attributes ----------------------------------------------
  const attributes = new Map<string, string>();

  const record = (label: string, value: string): void => {
    const key = label.replace(/[:\s]+$/, '').trim().toLowerCase();
    const v = clean(value);
    if (key && v && !attributes.has(key)) attributes.set(key, v);
  };

  // Strategy A: an element whose entire text is a known label, value in the
  // remainder of its parent.
  const LABELS = [
    'units',
    'frequency',
    'seasonal adjustment',
    'source',
    'release',
    'notes',
    'last updated',
    'observation period',
  ];

  $('p, li, div, tr').each((_, el) => {
    const $el = $(el);
    const $label = $el.children('span, b, strong, th').first();
    const labelText = clean($label.text()).toLowerCase().replace(/:$/, '');
    if (!labelText || !LABELS.includes(labelText)) return;
    const whole = clean($el.text());
    const value = whole.slice(clean($label.text()).length).replace(/^[:\s]+/, '');
    record(labelText, value);
  });

  // Strategy B: legacy `.series-meta-label` / `.series-meta-value` pairs.
  $('.series-meta-label, .fg-source-label').each((_, el) => {
    const $el = $(el);
    const label = clean($el.text());
    const value = clean($el.next('.series-meta-value, .fg-source-value').text()) || clean($el.parent().text()).slice(label.length);
    record(label, value);
  });

  // Strategy C: raw-text regex over the page, for markup we did not anticipate.
  if (attributes.size === 0) {
    const text = clean($('body').text());
    for (const label of LABELS) {
      const re = new RegExp(`${label}\\s*:\\s*([^:]{1,120}?)(?=\\s+(?:${LABELS.join('|')})\\s*:|$)`, 'i');
      const m = re.exec(text);
      if (m?.[1]) record(label, m[1]);
    }
  }

  // ---- units, split into long and short ----------------------------------
  // FRED writes "Percent, Seasonally Adjusted" or "Billions of Dollars".
  const unitsRaw = attributes.get('units') ?? '';
  const units = unitsRaw.replace(/,\s*(Seasonally Adjusted.*|Not Seasonally Adjusted.*)$/i, '').trim();

  // ---- observation range -------------------------------------------------
  // Rendered as "1948-01-01 to 2026-07-01" near the header.
  let observationStart = '';
  let observationEnd = '';
  const rangeText =
    clean($('.series-obs-range, #series-obs-range, .fg-obs-range').first().text()) ||
    clean($('body').text());
  const range = /(\d{4}-\d{2}-\d{2})\s*(?:to|–|-|—)\s*(\d{4}-\d{2}-\d{2})/.exec(rangeText);
  if (range) {
    observationStart = range[1]!;
    observationEnd = range[2]!;
  }

  const notes =
    clean($('#notes-container, .series-notes, #series-notes, [itemprop="description"]').first().text()) ||
    attributes.get('notes') ||
    '';

  return {
    id,
    title: title || id,
    units,
    unitsShort: units,
    frequency: attributes.get('frequency') ?? '',
    seasonalAdjustment: attributes.get('seasonal adjustment') ?? '',
    lastUpdated: attributes.get('last updated') ?? '',
    observationStart,
    observationEnd,
    notes: notes.slice(0, 4000),
  };
}

async function scrapeSeriesMeta(id: string): Promise<FredMeta> {
  const html = await fetchText(`${WEB_BASE}/series/${encodeURIComponent(id)}`, {
    timeoutMs: 30_000,
    retries: 1,
  });
  return parseSeriesPage(html, id);
}

/* ------------------------------------------------------------ api fallback */

interface ApiSeries {
  id: string;
  title: string;
  units: string;
  units_short: string;
  frequency: string;
  seasonal_adjustment: string;
  last_updated: string;
  observation_start: string;
  observation_end: string;
  notes?: string;
}

function fromApiSeries(s: ApiSeries): FredMeta {
  return {
    id: s.id,
    title: s.title,
    units: s.units,
    unitsShort: s.units_short,
    frequency: s.frequency,
    seasonalAdjustment: s.seasonal_adjustment,
    lastUpdated: s.last_updated,
    observationStart: s.observation_start,
    observationEnd: s.observation_end,
    notes: (s.notes ?? '').slice(0, 4000),
  };
}

function apiUrl(path: string, params: Record<string, string | undefined>): string {
  const url = new URL(`${API_BASE}${path}`);
  url.searchParams.set('api_key', apiKey()!);
  url.searchParams.set('file_type', 'json');
  for (const [k, v] of Object.entries(params)) if (v) url.searchParams.set(k, v);
  return url.toString();
}

async function apiSeries(id: string, start?: string, end?: string): Promise<FredSeriesResponse> {
  const [meta, obs] = await Promise.all([
    fetchJson<{ seriess?: ApiSeries[] }>(apiUrl('/series', { series_id: id })),
    fetchJson<{ observations?: { date: string; value: string }[] }>(
      apiUrl('/series/observations', {
        series_id: id,
        observation_start: start,
        observation_end: end,
      }),
    ),
  ]);

  const s = meta.seriess?.[0];
  if (!s) throw new UpstreamError(`No FRED series called ${id}`, { code: 'not_found' });

  return {
    series: { ...fromApiSeries(s), provider: 'fred', source: 'api', sourceUrl: seriesUrl(id) },
    observations: (obs.observations ?? []).map((o) => ({
      date: o.date,
      value: o.value === '.' || o.value === '' ? null : Number(o.value),
    })),
  };
}

/* ----------------------------------------------------------------- search */

/**
 * Parse FRED search results.
 *
 * Rather than depend on result-card classes, this collects every anchor whose
 * href is exactly `/series/<ID>` — the one thing a search result page is
 * guaranteed to contain — and reads the title from the link text.
 */
export function parseSearchPage(html: string): FredSearchResult[] {
  const $ = cheerio.load(html);
  const seen = new Set<string>();
  const results: FredSearchResult[] = [];

  $('a[href*="/series/"]').each((_, el) => {
    const href = $(el).attr('href') ?? '';
    const m = /^(?:https?:\/\/fred\.stlouisfed\.org)?\/series\/([A-Za-z0-9_.\-]+)\/?(?:\?.*)?$/.exec(href);
    if (!m) return;

    const id = m[1]!.toUpperCase();
    if (seen.has(id)) return;

    const title = $(el).text().replace(/\s+/g, ' ').trim();
    // Navigation chrome links to /series/ too, but without a descriptive label.
    if (!title || title.length < 3 || title.toUpperCase() === id) return;

    seen.add(id);

    // The metadata line ("Percent, Monthly, Seasonally Adjusted") usually sits
    // in a sibling of the link's container.
    const meta = $(el)
      .closest('div, li, tr')
      .find('.series-meta, .fred-meta, .search-result-meta')
      .first()
      .text()
      .replace(/\s+/g, ' ')
      .trim();

    const entry: FredSearchResult = { provider: 'fred', id, title };
    if (meta) {
      const parts = meta.split(',').map((p) => p.trim());
      if (parts[0]) entry.units = parts[0];
      if (parts[1]) entry.frequency = parts[1];
      if (parts[2]) entry.seasonalAdjustment = parts[2];
    }
    results.push(entry);
  });

  return results;
}

async function scrapeSearch(query: string, limit: number): Promise<FredSearchResult[]> {
  const url = new URL(`${WEB_BASE}/searchresults/`);
  url.searchParams.set('st', query);
  url.searchParams.set('ob', 'sr'); // order by search rank
  url.searchParams.set('od', 'desc');
  const html = await fetchText(url.toString(), { timeoutMs: 30_000, retries: 1 });
  return parseSearchPage(html).slice(0, limit);
}

async function apiSearch(query: string, limit: number): Promise<FredSearchResult[]> {
  const data = await fetchJson<{ seriess?: ApiSeries[] }>(
    apiUrl('/series/search', {
      search_text: query,
      limit: String(limit),
      order_by: 'search_rank',
    }),
  );
  return (data.seriess ?? []).map((s) => ({
    provider: 'fred',
    id: s.id,
    title: s.title,
    units: s.units_short || s.units,
    frequency: s.frequency,
    seasonalAdjustment: s.seasonal_adjustment,
    observationRange: `${s.observation_start} to ${s.observation_end}`,
  }));
}

/* ------------------------------------------------------------ public entry */

/**
 * Scrape first; fall back to the official API only if a key is configured.
 *
 * When both fail, the *scrape* error is what surfaces — it carries the
 * `upstream_blocked` hint that tells the operator what actually went wrong.
 */
/**
 * The scrape is the source of record; the official API stands behind it.
 *
 * The API arm only exists when a key is configured, and when it is *not*, a
 * blocked scrape is worth annotating: the operator can fix it, and nothing
 * else in the response would tell them how.
 */
function fredProviders<T>(
  what: string,
  scrape: () => Promise<T>,
  viaApi: () => Promise<T>,
): { providers: Provider<T>[]; options: ChainOptions } {
  return {
    providers: [
      { id: 'scrape', label: 'fred.stlouisfed.org', run: scrape },
      {
        id: 'api',
        label: 'the FRED API',
        available: () => Boolean(apiKey()),
        skippedHint: (cause) =>
          cause?.code === 'upstream_blocked'
            ? `${cause.hint ?? ''} Set FRED_API_KEY (free, from ` +
              `https://fred.stlouisfed.org/docs/api/api_key.html) to use the official API instead.`.trim()
            : undefined,
        run: viaApi,
      },
    ],
    // When both fail the scrape's error is what surfaces: it carries the
    // `upstream_blocked` hint that says what actually went wrong.
    options: { what, logPrefix: 'fred' },
  };
}

export async function getSeries(
  rawId: string,
  start?: string,
  end?: string,
): Promise<FredSeriesResponse> {
  const id = assertSeriesId(rawId);
  const key = `fred:series:${id}:${start ?? ''}:${end ?? ''}`;

  return cache.cached(key, TTL.fred, async () => {
    const { providers, options } = fredProviders<FredSeriesResponse>(
      `series ${id}`,
      async () => {
        // Observations are the deliverable; metadata is best-effort alongside.
        const [observations, meta] = await Promise.all([
          scrapeObservations(id, start, end),
          scrapeSeriesMeta(id).catch((err) => {
            console.warn(`[fred] metadata scrape failed for ${id}:`, err?.message ?? err);
            return null;
          }),
        ]);

        const stamp = { provider: 'fred', source: 'scrape', sourceUrl: seriesUrl(id) } as const;
        const series: FredSeries = meta
          ? { ...meta, ...stamp }
          : {
              id,
              title: id,
              units: '',
              unitsShort: '',
              frequency: '',
              seasonalAdjustment: '',
              lastUpdated: '',
              observationStart: observations[0]?.date ?? '',
              observationEnd: observations.at(-1)?.date ?? '',
              notes: '',
              ...stamp,
            };

        if (!series.observationStart) series.observationStart = observations[0]?.date ?? '';
        if (!series.observationEnd) series.observationEnd = observations.at(-1)?.date ?? '';

        return { series, observations };
      },
      () => apiSeries(id, start, end),
    );

    return (await firstAnswer(providers, options)).value;
  });
}

/**
 * Search FRED's catalogue, saying which arm answered.
 *
 * Returns {@link Attributed} rather than the old response envelope. FRED is one
 * of eight publishers `ECOS` asks, and *that* envelope — which providers
 * failed, which were skipped for want of a key — describes the whole fan-out
 * rather than FRED. Which arm answered still describes FRED, so it stays, and
 * `/api/fred/search` rebuilds the shape its callers expect from both halves.
 */
export async function searchSeries(
  query: string,
  limit = 25,
): Promise<Attributed<FredSearchResult[]>> {
  const q = query.trim();
  if (!q) {
    throw new UpstreamError('Search needs at least one word', { code: 'bad_request' });
  }
  const capped = Math.min(Math.max(limit, 1), 100);
  const key = `fred:search:${q.toLowerCase()}:${capped}`;

  return cache.cached(key, TTL.fred, async () => {
    const { providers, options } = fredProviders(
      `search ${q}`,
      () => scrapeSearch(q, capped),
      () => apiSearch(q, capped),
    );
    return firstAnswer(providers, options);
  });
}
