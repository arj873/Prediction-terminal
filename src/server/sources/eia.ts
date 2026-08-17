/**
 * US Energy Information Administration — api.eia.gov v2.
 *
 * The settlement feed for energy markets: the weekly retail gasoline price a
 * "gas above $X" contract resolves on, Henry Hub, WTI and Brent spot, crude
 * inventories, electricity generation. Kalshi lists several of these directly.
 *
 * The EIA publishes **no anonymous tier** — every route answers 403 without a
 * key — so this provider declares itself unavailable rather than failing at
 * request time when `EIA_API_KEY` is unset. A key is free and issued instantly.
 *
 * Two id forms, because the v2 API has two front doors and they suit different
 * readers:
 *
 *  - `EIA:PET.RWTC.D` — a v1-style series id, resolved through the `/seriesid`
 *    compatibility route. This is what published documentation and most of the
 *    internet quotes, so it is the one that works when you paste something in.
 *  - `EIA:petroleum/pri/spt/data?facets[series][]=RWTC&frequency=daily` — the
 *    native route form, for anything the compatibility layer does not cover.
 */

import type { DataObservation, DataSearchResult, DataSeriesResponse } from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';
import { periodToDate } from '../lib/period.js';
import { searchCatalogue, type SdmxCatalogueEntry } from './sdmx.js';

const BASE = process.env.EIA_API_BASE ?? 'https://api.eia.gov/v2';

export const apiKey = (): string | undefined => process.env.EIA_API_KEY?.trim() || undefined;

export function hasCredentials(): boolean {
  return apiKey() !== undefined;
}

function requireKey(): string {
  const key = apiKey();
  if (!key) {
    throw new UpstreamError('The EIA needs an API key, and none is set', {
      code: 'not_configured',
      hint:
        'The EIA publishes nothing anonymously. Register free at ' +
        'https://www.eia.gov/opendata/register.php and set EIA_API_KEY.',
    });
  }
  return key;
}

interface EiaRow {
  period?: string;
  value?: number | string | null;
  units?: string;
  'series-description'?: string;
  seriesDescription?: string;
  [key: string]: unknown;
}

interface EiaResponse {
  response?: {
    total?: number | string;
    dateFormat?: string;
    frequency?: string;
    description?: string;
    data?: EiaRow[];
  };
  error?: string;
}

/** An EIA period as a calendar date. See {@link periodToDate}. */
export const eiaPeriodToDate = periodToDate;

/** Split `route/path?query` into the two halves, tolerating a missing query. */
function splitRoute(id: string): { path: string; search: URLSearchParams } {
  const at = id.indexOf('?');
  if (at === -1) return { path: id, search: new URLSearchParams() };
  return { path: id.slice(0, at), search: new URLSearchParams(id.slice(at + 1)) };
}

function buildUrl(rawId: string, start?: string, end?: string): string {
  const key = requireKey();
  const id = rawId.trim().replace(/^\/+/, '');

  // A bare id with no slash is a v1-style series id; anything with a path is the
  // native route form and the caller has said what they want.
  const native = id.includes('/');
  const { path, search } = splitRoute(id);

  const url = new URL(
    native
      ? `${BASE}/${path.replace(/\/data\/?$/, '')}/data/`
      : `${BASE}/seriesid/${encodeURIComponent(id)}`,
  );

  for (const [k, v] of search) url.searchParams.append(k, v);

  url.searchParams.set('api_key', key);
  if (!url.searchParams.has('data[0]')) url.searchParams.append('data[0]', 'value');
  if (start) url.searchParams.set('start', start);
  if (end) url.searchParams.set('end', end);
  // Ascending, so a truncated response loses the oldest rather than the newest.
  if (!url.searchParams.has('sort[0][column]')) {
    url.searchParams.append('sort[0][column]', 'period');
    url.searchParams.append('sort[0][direction]', 'asc');
  }
  if (!url.searchParams.has('length')) url.searchParams.set('length', '5000');

  return url.toString();
}

export async function getSeries(
  rawId: string,
  start?: string,
  end?: string,
): Promise<DataSeriesResponse> {
  const id = rawId.trim();
  if (!id) {
    throw new UpstreamError('Missing EIA series id', {
      code: 'bad_request',
      hint: 'Ids look like PET.RWTC.D (WTI spot) or NG.RNGWHHD.D (Henry Hub).',
    });
  }

  const cacheKey = `eia:series:${id}:${start ?? ''}:${end ?? ''}`;

  return cache.cached(cacheKey, TTL.fred, async () => {
    const url = buildUrl(id, start, end);
    const payload = await fetchJson<EiaResponse>(url, { timeoutMs: 40_000, retries: 1 });

    const rows = payload.response?.data ?? [];
    if (rows.length === 0) {
      throw new UpstreamError(`The EIA returned no observations for ${id}`, {
        code: 'not_found',
        hint:
          payload.error ??
          'Check the series id. `ECOS <words> eia` lists the ones this terminal knows.',
      });
    }

    const observations: DataObservation[] = [];
    for (const row of rows) {
      const date = eiaPeriodToDate(String(row.period ?? ''));
      if (date === null) continue;
      const raw = row.value;
      const value = raw === null || raw === undefined || raw === '' ? null : Number(raw);
      observations.push({ date, value: value !== null && Number.isFinite(value) ? value : null });
    }

    observations.sort((a, b) => a.date.localeCompare(b.date));

    const known = CATALOGUE.find((entry) => entry.id === id);
    const first = rows[0] ?? {};
    const units = first.units ?? known?.units ?? '';
    const title =
      first['series-description'] ??
      first.seriesDescription ??
      payload.response?.description ??
      known?.title ??
      id;

    return {
      series: {
        provider: 'eia',
        id,
        title: String(title),
        units: String(units),
        unitsShort: String(units),
        frequency: payload.response?.frequency ?? known?.frequency ?? '',
        seasonalAdjustment: '',
        lastUpdated: '',
        observationStart: observations[0]?.date ?? '',
        observationEnd: observations.at(-1)?.date ?? '',
        notes: '',
        source: id.includes('/') ? 'v2' : 'seriesid',
        sourceUrl: 'https://www.eia.gov/opendata/browser/',
      },
      observations,
    };
  });
}

/** The energy prints markets settle on. Any other id still charts. */
const CATALOGUE: readonly SdmxCatalogueEntry[] = [
  {
    id: 'PET.RWTC.D',
    title: 'Cushing, OK WTI spot price FOB',
    units: 'Dollars per barrel',
    frequency: 'Daily',
    keywords: 'crude oil wti spot petroleum',
  },
  {
    id: 'PET.RBRTE.D',
    title: 'Europe Brent spot price FOB',
    units: 'Dollars per barrel',
    frequency: 'Daily',
    keywords: 'crude oil brent spot petroleum',
  },
  {
    id: 'PET.EMM_EPMR_PTE_NUS_DPG.W',
    title: 'US regular all-formulations retail gasoline price',
    units: 'Dollars per gallon',
    frequency: 'Weekly',
    keywords: 'gasoline petrol pump price retail gas',
  },
  {
    id: 'PET.EMD_EPD2D_PTE_NUS_DPG.W',
    title: 'US No. 2 diesel retail price',
    units: 'Dollars per gallon',
    frequency: 'Weekly',
    keywords: 'diesel fuel retail price',
  },
  {
    id: 'PET.WCESTUS1.W',
    title: 'US ending stocks of crude oil excluding SPR',
    units: 'Thousand barrels',
    frequency: 'Weekly',
    keywords: 'crude inventories stocks eia build draw',
  },
  {
    id: 'PET.WCSSTUS1.W',
    title: 'US ending stocks of crude oil in the Strategic Petroleum Reserve',
    units: 'Thousand barrels',
    frequency: 'Weekly',
    keywords: 'spr strategic petroleum reserve stocks',
  },
  {
    id: 'NG.RNGWHHD.D',
    title: 'Henry Hub natural gas spot price',
    units: 'Dollars per million Btu',
    frequency: 'Daily',
    keywords: 'natural gas henry hub spot nat gas',
  },
  {
    id: 'NG.NW2_EPG0_SWO_R48_BCF.W',
    title: 'US lower 48 working natural gas in underground storage',
    units: 'Billion cubic feet',
    frequency: 'Weekly',
    keywords: 'natural gas storage inventories injection withdrawal',
  },
  {
    id: 'ELEC.GEN.ALL-US-99.M',
    title: 'US net electricity generation, all sectors, all fuels',
    units: 'Thousand megawatthours',
    frequency: 'Monthly',
    keywords: 'electricity generation power grid',
  },
  {
    id: 'TOTAL.TETCBUS.M',
    title: 'US total primary energy consumption',
    units: 'Trillion Btu',
    frequency: 'Monthly',
    keywords: 'energy consumption total primary',
  },
];

export function search(query: string, limit: number): DataSearchResult[] {
  return searchCatalogue('eia', CATALOGUE, query, limit);
}
