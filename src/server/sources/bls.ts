/**
 * Bureau of Labor Statistics — the US labour statistics of record.
 *
 * CPI, the unemployment rate, nonfarm payrolls and average hourly earnings are
 * published here first; FRED mirrors them hours later. For a market that
 * settles on a print, reading the publisher rather than the mirror is the
 * difference between settling on time and settling on someone else's schedule.
 *
 * Two things shape this module:
 *
 * **The year window.** The API takes a start and end *year*, and caps the span
 * — ten years unregistered, twenty with a free key. A chart of "unemployment
 * since 1948" is therefore eight requests, not one, so {@link windows} splits
 * the range and the pieces are merged back. Without that, asking for history
 * silently returns the last decade of it, which looks like data rather than
 * like a truncation.
 *
 * **Periods are not dates.** The BLS dates a monthly reading `M07`, a quarter
 * `Q02`, and an annual *average* `M13` — a summary row sitting in the same
 * array as the monthlies. Charting `M13` alongside `M01`–`M12` would draw
 * thirteen points a year, one of them the mean of the other twelve, so it is
 * dropped rather than dated.
 */

import type { DataObservation, DataSearchResult, DataSeriesResponse } from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';
import { searchCatalogue, type SdmxCatalogueEntry } from './sdmx.js';

const BASE = process.env.BLS_API_BASE ?? 'https://api.bls.gov/publicAPI';

const apiKey = (): string | undefined => process.env.BLS_API_KEY?.trim() || undefined;

/** Years per request. The API rejects a wider span outright rather than truncating. */
const SPAN_UNREGISTERED = 10;
const SPAN_REGISTERED = 20;

/** BLS series ids are upper-case alphanumerics, 8–24 characters. */
const SERIES_ID = /^[A-Z0-9]{6,32}$/;

export function assertBlsSeriesId(id: string): string {
  const trimmed = id.trim().toUpperCase();
  if (!SERIES_ID.test(trimmed)) {
    throw new UpstreamError(`"${id}" is not a valid BLS series id`, {
      code: 'bad_request',
      hint:
        'BLS ids are letters and digits with no punctuation, e.g. LNS14000000 ' +
        '(unemployment rate) or CUUR0000SA0 (CPI-U). Try `ECOS <words> bls`.',
    });
  }
  return trimmed;
}

/* ---------------------------------------------------------------- periods */

/**
 * A BLS period code as the day its period begins, or `null` to drop the row.
 *
 * `null` is returned for the aggregate codes — `M13`, `Q05`, `S03` — which are
 * annual averages the API interleaves with the readings they average.
 */
export function blsPeriodToDate(year: string, period: string): string | null {
  if (!/^\d{4}$/.test(year)) return null;
  const code = period.trim().toUpperCase();

  const month = /^M(\d{2})$/.exec(code);
  if (month) {
    const m = Number(month[1]);
    // M13 is the annual average, not a thirteenth month.
    return m >= 1 && m <= 12 ? `${year}-${String(m).padStart(2, '0')}-01` : null;
  }

  const quarter = /^Q(\d{2})$/.exec(code);
  if (quarter) {
    const q = Number(quarter[1]);
    return q >= 1 && q <= 4 ? `${year}-${String(1 + (q - 1) * 3).padStart(2, '0')}-01` : null;
  }

  const semi = /^S(\d{2})$/.exec(code);
  if (semi) {
    const s = Number(semi[1]);
    return s === 1 ? `${year}-01-01` : s === 2 ? `${year}-07-01` : null;
  }

  if (/^A\d{2}$/.test(code)) return `${year}-01-01`;

  return null;
}

/* ----------------------------------------------------------------- fetch */

interface BlsDatum {
  year: string;
  period: string;
  periodName?: string;
  value: string;
  latest?: string;
  footnotes?: { code?: string; text?: string }[];
}

interface BlsSeries {
  seriesID: string;
  catalog?: {
    series_title?: string;
    measure_data_type?: string;
    seasonality?: string;
    survey_name?: string;
  };
  data?: BlsDatum[];
}

interface BlsResponse {
  status?: string;
  message?: string[];
  Results?: { series?: BlsSeries[] };
}

/** The year windows covering `[from, to]`, each within the API's span cap. */
export function windows(from: number, to: number, span: number): { start: number; end: number }[] {
  const out: { start: number; end: number }[] = [];
  for (let start = from; start <= to; start += span) {
    out.push({ start, end: Math.min(start + span - 1, to) });
  }
  return out;
}

async function fetchWindow(
  id: string,
  startYear: number,
  endYear: number,
): Promise<BlsSeries | undefined> {
  const key = apiKey();
  const version = key ? 'v2' : 'v1';

  const body: Record<string, unknown> = {
    seriesid: [id],
    startyear: String(startYear),
    endyear: String(endYear),
  };
  if (key) {
    body['registrationkey'] = key;
    // The catalogue carries the human title, units and seasonality. It is a v2
    // privilege, so without a key the panel falls back to the curated title.
    body['catalog'] = true;
  }

  const payload = await fetchJson<BlsResponse>(`${BASE}/${version}/timeseries/data/`, {
    timeoutMs: 30_000,
    retries: 1,
    method: 'POST',
    body: JSON.stringify(body),
    headers: { 'Content-Type': 'application/json' },
  });

  if (payload.status && payload.status !== 'REQUEST_SUCCEEDED') {
    const message = (payload.message ?? []).join(' ').trim();
    // The daily quota is the failure an operator will actually hit, and the
    // remedy — a free key — is not guessable from "REQUEST_NOT_PROCESSED".
    const throttled = /threshold|limit|exceed/i.test(message);
    throw new UpstreamError(message || `The BLS refused the request for ${id}`, {
      code: throttled ? 'rate_limited' : 'upstream_error',
      hint: throttled && !key
        ? `The unregistered BLS API allows ${25} queries per IP per day. A free key ` +
          `from https://data.bls.gov/registrationEngine/ raises that to 500 and doubles ` +
          `the history per call — set BLS_API_KEY.`
        : undefined,
    });
  }

  return payload.Results?.series?.[0];
}

/* ---------------------------------------------------------------- public */

const thisYear = (): number => new Date().getUTCFullYear();

/** `2015-01-01` → `2015`. A bare year is accepted too. */
function yearOf(date: string | undefined, fallback: number): number {
  const year = Number((date ?? '').slice(0, 4));
  return Number.isInteger(year) && year >= 1900 && year <= 2100 ? year : fallback;
}

export async function getSeries(
  rawId: string,
  start?: string,
  end?: string,
): Promise<DataSeriesResponse> {
  const id = assertBlsSeriesId(rawId);
  const span = apiKey() ? SPAN_REGISTERED : SPAN_UNREGISTERED;
  const endYear = yearOf(end, thisYear());
  // With no start, show one full window — the longest history one request buys.
  const startYear = Math.min(yearOf(start, endYear - span + 1), endYear);

  const cacheKey = `bls:series:${id}:${startYear}:${endYear}:${apiKey() ? 'k' : ''}`;

  return cache.cached(cacheKey, TTL.fred, async () => {
    const spans = windows(startYear, endYear, span);
    // Sequential rather than parallel: the BLS counts requests against a daily
    // quota per IP, and a burst of eight is the fastest way to spend it.
    const parts: BlsSeries[] = [];
    for (const span_ of spans) {
      const part = await fetchWindow(id, span_.start, span_.end);
      if (part) parts.push(part);
    }

    if (parts.length === 0 || parts.every((p) => (p.data ?? []).length === 0)) {
      throw new UpstreamError(`The BLS has no data for ${id} in ${startYear}–${endYear}`, {
        code: 'not_found',
        hint:
          `Either the series id is wrong or it has no observations in that window. ` +
          `Try \`ECOS <words> bls\` to find an id.`,
      });
    }

    const byDate = new Map<string, number | null>();
    for (const part of parts) {
      for (const datum of part.data ?? []) {
        const date = blsPeriodToDate(datum.year, datum.period);
        if (date === null) continue;
        const value = Number(datum.value.replace(/,/g, ''));
        byDate.set(date, Number.isFinite(value) ? value : null);
      }
    }

    const observations: DataObservation[] = [...byDate]
      .map(([date, value]) => ({ date, value }))
      .sort((a, b) => a.date.localeCompare(b.date));

    const catalog = parts.find((p) => p.catalog)?.catalog;
    const known = CATALOGUE.find((entry) => entry.id === id);
    const units = catalog?.measure_data_type ?? known?.units ?? '';

    return {
      series: {
        provider: 'bls',
        id,
        title: catalog?.series_title ?? known?.title ?? id,
        units,
        unitsShort: units,
        frequency: known?.frequency ?? '',
        seasonalAdjustment: catalog?.seasonality ?? '',
        lastUpdated: '',
        observationStart: observations[0]?.date ?? '',
        observationEnd: observations.at(-1)?.date ?? '',
        notes: catalog?.survey_name ? `Survey: ${catalog.survey_name}` : '',
        source: apiKey() ? 'v2' : 'v1',
        sourceUrl: `https://data.bls.gov/timeseries/${encodeURIComponent(id)}`,
      },
      observations,
    };
  });
}

/**
 * The headline BLS series, by id.
 *
 * The BLS publishes no series-search API — its own site searches a database
 * that has no public endpoint — so this is a curated list rather than a query.
 * It covers the prints that move markets; any other id still charts, this is
 * only what `ECOS` can offer before you know one.
 */
const CATALOGUE: readonly SdmxCatalogueEntry[] = [
  {
    id: 'LNS14000000',
    title: 'Unemployment rate, seasonally adjusted',
    units: 'Percent',
    frequency: 'Monthly',
    keywords: 'jobless unemployment labour labor household survey',
  },
  {
    id: 'LNS11300000',
    title: 'Labor force participation rate, seasonally adjusted',
    units: 'Percent',
    frequency: 'Monthly',
    keywords: 'participation labour labor supply',
  },
  {
    id: 'LNS12300000',
    title: 'Employment-population ratio, seasonally adjusted',
    units: 'Percent',
    frequency: 'Monthly',
    keywords: 'employment population ratio labour labor',
  },
  {
    id: 'LNS13327709',
    title: 'U-6 total unemployed plus marginally attached and part-time for economic reasons',
    units: 'Percent',
    frequency: 'Monthly',
    keywords: 'u6 underemployment jobless broad unemployment',
  },
  {
    id: 'CES0000000001',
    title: 'Total nonfarm employment, seasonally adjusted',
    units: 'Thousands of persons',
    frequency: 'Monthly',
    keywords: 'payrolls nfp jobs establishment survey employment',
  },
  {
    id: 'CES0500000003',
    title: 'Average hourly earnings, total private, seasonally adjusted',
    units: 'Dollars per hour',
    frequency: 'Monthly',
    keywords: 'wages earnings pay ahe payrolls',
  },
  {
    id: 'CES0500000002',
    title: 'Average weekly hours, total private, seasonally adjusted',
    units: 'Hours',
    frequency: 'Monthly',
    keywords: 'hours worked payrolls',
  },
  {
    id: 'CUUR0000SA0',
    title: 'CPI-U, all items, US city average, not seasonally adjusted',
    units: 'Index 1982-84=100',
    frequency: 'Monthly',
    keywords: 'cpi inflation prices consumer headline',
  },
  {
    id: 'CUSR0000SA0',
    title: 'CPI-U, all items, US city average, seasonally adjusted',
    units: 'Index 1982-84=100',
    frequency: 'Monthly',
    keywords: 'cpi inflation prices consumer headline',
  },
  {
    id: 'CUUR0000SA0L1E',
    title: 'CPI-U, all items less food and energy, not seasonally adjusted',
    units: 'Index 1982-84=100',
    frequency: 'Monthly',
    keywords: 'core cpi inflation prices consumer',
  },
  {
    id: 'CUSR0000SA0L1E',
    title: 'CPI-U, all items less food and energy, seasonally adjusted',
    units: 'Index 1982-84=100',
    frequency: 'Monthly',
    keywords: 'core cpi inflation prices consumer',
  },
  {
    id: 'CUUR0000SETB01',
    title: 'CPI-U, gasoline (all types), US city average',
    units: 'Index 1982-84=100',
    frequency: 'Monthly',
    keywords: 'gasoline petrol fuel energy prices cpi',
  },
  {
    id: 'CUUR0000SAF1',
    title: 'CPI-U, food, US city average',
    units: 'Index 1982-84=100',
    frequency: 'Monthly',
    keywords: 'food groceries prices cpi',
  },
  {
    id: 'CUUR0000SAH1',
    title: 'CPI-U, shelter, US city average',
    units: 'Index 1982-84=100',
    frequency: 'Monthly',
    keywords: 'shelter rent housing prices cpi',
  },
  {
    id: 'WPUFD4',
    title: 'PPI final demand',
    units: 'Index Nov 2009=100',
    frequency: 'Monthly',
    keywords: 'ppi producer prices wholesale inflation',
  },
  {
    id: 'WPUFD49104',
    title: 'PPI final demand less foods, energy and trade services',
    units: 'Index',
    frequency: 'Monthly',
    keywords: 'core ppi producer prices wholesale inflation',
  },
  {
    id: 'PRS85006092',
    title: 'Nonfarm business sector labour productivity, percent change from previous quarter',
    units: 'Percent',
    frequency: 'Quarterly',
    keywords: 'productivity output per hour',
  },
  {
    id: 'CIU1010000000000A',
    title: 'Employment Cost Index, total compensation, all civilian workers',
    units: 'Percent change, 12-month',
    frequency: 'Quarterly',
    keywords: 'eci wages compensation labour costs',
  },
  {
    id: 'JTS000000000000000JOL',
    title: 'Job openings, total nonfarm, seasonally adjusted (JOLTS)',
    units: 'Thousands',
    frequency: 'Monthly',
    keywords: 'jolts vacancies job openings labour demand',
  },
  {
    id: 'JTS000000000000000QUR',
    title: 'Quits rate, total nonfarm, seasonally adjusted (JOLTS)',
    units: 'Percent',
    frequency: 'Monthly',
    keywords: 'jolts quits turnover labour',
  },
];

export function search(query: string, limit: number): DataSearchResult[] {
  return searchCatalogue('bls', CATALOGUE, query, limit);
}
