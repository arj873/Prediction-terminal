/**
 * data.gov — the US government's dataset catalogue.
 *
 * This is a finding aid rather than a feed: 300,000+ datasets from every
 * federal agency, plus states and cities, each with a description, a publisher
 * and links to the actual files. When a market settles on something obscure —
 * a state's unemployment insurance recipiency rate, county-level crop yields —
 * this is where the series is *named*, and the other eleven sources here are
 * where it is read.
 *
 * data.gov retired its CKAN Action API in 2026. The replacement is the Catalog
 * API at api.gsa.gov, which returns DCAT-US 3 records and paginates with an
 * opaque cursor rather than an offset. It is served through api.data.gov, so
 * the shared `DEMO_KEY` works at a throttled rate and a free key removes the
 * throttle.
 */

import type { DataGovDataset, DataGovSearchResponse } from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';

const BASE = process.env.DATAGOV_API_BASE ?? 'https://api.gsa.gov/technology/datagov/v4';

const DEMO_KEY = 'DEMO_KEY';

const apiKey = (): string => process.env.DATAGOV_API_KEY?.trim() || DEMO_KEY;

export function usingDemoKey(): boolean {
  return apiKey() === DEMO_KEY;
}

/* ------------------------------------------------------------ dcat shapes */

interface DcatDistribution {
  '@type'?: string;
  title?: string;
  format?: string;
  mediaType?: string;
  downloadURL?: string;
  accessURL?: string;
}

interface DcatDataset {
  identifier?: string;
  title?: string;
  description?: string;
  modified?: string;
  accrualPeriodicity?: string;
  landingPage?: string;
  theme?: string[];
  keyword?: string[];
  publisher?: { name?: string } | string;
  distribution?: DcatDistribution[];
}

interface SearchResult {
  dcat?: DcatDataset;
  id?: string;
}

interface SearchPayload {
  results?: SearchResult[];
  after?: string | null;
  total?: number;
}

/**
 * ISO-8601 periodicity as words.
 *
 * DCAT states update frequency as an ISO-8601 repeating interval — `R/P1Y` —
 * which is precise and unreadable. Anything unrecognised passes through rather
 * than being blanked: an odd code is still more informative than nothing.
 */
export function readPeriodicity(raw: string | undefined): string {
  if (!raw) return '';
  const known: Record<string, string> = {
    'R/P1D': 'Daily',
    'R/P1W': 'Weekly',
    'R/P2W': 'Fortnightly',
    'R/P1M': 'Monthly',
    'R/P3M': 'Quarterly',
    'R/P4M': 'Three times a year',
    'R/P6M': 'Twice a year',
    'R/P1Y': 'Annual',
    'R/P2Y': 'Biennial',
    'R/P3Y': 'Triennial',
    'R/PT1H': 'Hourly',
    irregular: 'Irregular',
    'R/PT1S': 'Continuous',
  };
  return known[raw] ?? known[raw.toLowerCase()] ?? raw;
}

/** DCAT's `modified` is sometimes a date and sometimes a full timestamp. */
function dayOf(raw: string | undefined): string {
  const match = /^(\d{4}-\d{2}-\d{2})/.exec((raw ?? '').trim());
  return match ? match[1]! : '';
}

export function toDataset(result: SearchResult): DataGovDataset | null {
  const dcat = result.dcat;
  if (!dcat?.title) return null;

  const distributions = dcat.distribution ?? [];
  const formats = [
    ...new Set(
      distributions
        .map((d) => (d.format ?? d.mediaType ?? '').trim())
        .filter(Boolean)
        // `text/csv` reads worse than `CSV` in a table column.
        .map((f) => (f.includes('/') ? (f.split('/').pop() ?? f) : f).toUpperCase()),
    ),
  ];

  const publisher =
    typeof dcat.publisher === 'string' ? dcat.publisher : (dcat.publisher?.name ?? '');

  return {
    id: dcat.identifier ?? result.id ?? '',
    title: dcat.title.replace(/\s+/g, ' ').trim(),
    description: (dcat.description ?? '').replace(/\s+/g, ' ').trim().slice(0, 1200),
    publisher,
    themes: [...new Set([...(dcat.theme ?? []), ...(dcat.keyword ?? [])])].slice(0, 8),
    modified: dayOf(dcat.modified),
    frequency: readPeriodicity(dcat.accrualPeriodicity),
    formats,
    url:
      dcat.landingPage ||
      distributions.find((d) => d.accessURL)?.accessURL ||
      distributions.find((d) => d.downloadURL)?.downloadURL ||
      '',
  };
}

/* ---------------------------------------------------------------- public */

export async function searchDatasets(
  query: string,
  limit = 30,
  cursor?: string,
): Promise<DataGovSearchResponse> {
  const q = query.trim();
  if (!q) {
    throw new UpstreamError('Search needs at least one word', {
      code: 'bad_request',
      hint: 'e.g. `DGOV unemployment insurance`, `DGOV crop yields`.',
    });
  }

  const capped = Math.min(Math.max(limit, 1), 100);
  const cacheKey = `datagov:search:${q.toLowerCase()}:${capped}:${cursor ?? ''}`;

  return cache.cached(cacheKey, TTL.datagov, async () => {
    const url = new URL(`${BASE}/search`);
    url.searchParams.set('q', q);
    url.searchParams.set('limit', String(capped));
    if (cursor) url.searchParams.set('after', cursor);

    const payload = await fetchJson<SearchPayload>(url.toString(), {
      timeoutMs: 30_000,
      retries: 1,
      // The key goes in a header rather than the query string, so it stays out
      // of the URL that gets logged on the way through.
      headers: { 'X-Api-Key': apiKey() },
    }).catch((err: unknown) => {
      throw annotate(err);
    });

    const datasets = (payload.results ?? [])
      .map(toDataset)
      .filter((d): d is DataGovDataset => d !== null);

    return {
      query: q,
      datasets,
      cursor: payload.after ?? null,
      source: usingDemoKey() ? 'data.gov Catalog API (DEMO_KEY)' : 'data.gov Catalog API',
    };
  });
}

function annotate(err: unknown): unknown {
  if (!(err instanceof UpstreamError)) return err;
  if (err.status !== 429 || !usingDemoKey()) return err;
  return new UpstreamError('data.gov is rate-limiting the shared demonstration key', {
    code: 'rate_limited',
    status: 429,
    hint:
      "This terminal is using api.data.gov's DEMO_KEY, which is throttled per IP. " +
      'A free key from https://api.data.gov/signup/ removes the throttle — set ' +
      'DATAGOV_API_KEY.',
  });
}
