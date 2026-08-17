/**
 * The series publishers, as one table.
 *
 * Eight upstreams answer the same two questions — "chart this id" and "what ids
 * match these words" — and they were each written to their own shape. This is
 * where they become interchangeable, so `ECO` and `ECOS` are one command apiece
 * rather than eight, and so adding a ninth publisher is a row here plus a
 * module, with nothing to remember to update in the router, the client or the
 * help text.
 *
 * The shape mirrors `lib/providers.ts` deliberately. That module puts several
 * sources behind *one* answer when they are alternatives; this one puts several
 * sources behind one *interface* when they are peers. Both make availability a
 * declaration rather than something a caller discovers by getting an error:
 * `ECOS` must know it cannot ask the EIA before it fans out, and the `SRC`
 * board has to show a reader what this deployment can actually serve.
 */

import type { DataSearchResponse, DataSearchResult, DataSeriesResponse, DataSourceStatus } from '../../shared/types.js';
import {
  DATA_SOURCES,
  SERIES_SOURCE_IDS,
  dataSourceInfo,
  isDataSource,
  parseDataRef,
  type DataSourceId,
} from '../../shared/dataset.js';
import { UpstreamError } from '../lib/http.js';
import * as bls from './bls.js';
import * as cftc from './cftc.js';
import * as congress from './congress.js';
import * as datagov from './datagov.js';
import { ecb } from './ecb.js';
import * as eia from './eia.js';
import * as fed from './feddata.js';
import * as fred from './fred.js';
import { imf } from './imf.js';
import { oecd } from './oecd.js';
import * as polygon from './polygon.js';

/** Why a source cannot answer in this deployment, or `null` when it can. */
type Unavailable = string | null;

interface SeriesProvider {
  id: DataSourceId;
  series(id: string, start?: string, end?: string): Promise<DataSeriesResponse>;
  search(query: string, limit: number): Promise<DataSearchResult[]> | DataSearchResult[];
  /** `null` when usable. A string is the operator-facing reason it is not. */
  unavailable(): Unavailable;
}

const usable = (): Unavailable => null;

/**
 * The table. Order is the order `ECOS` reports results in, and it is
 * deliberate: FRED first because it re-publishes most of the others under one
 * naming scheme, then the primary publishers, then the two that need a key.
 */
const PROVIDERS: readonly SeriesProvider[] = [
  {
    id: 'fred',
    series: (id, start, end) => fred.getSeries(id, start, end),
    search: async (query, limit) => (await fred.searchSeries(query, limit)).value,
    unavailable: usable,
  },
  {
    id: 'bls',
    series: (id, start, end) => bls.getSeries(id, start, end),
    search: (query, limit) => bls.search(query, limit),
    unavailable: usable,
  },
  {
    id: 'fed',
    series: (id, start, end) => fed.getSeries(id, start, end),
    search: (query, limit) => fed.search(query, limit),
    unavailable: usable,
  },
  {
    id: 'ecb',
    series: (id, start, end) => ecb.series(id, start, end),
    search: (query, limit) => ecb.search(query, limit),
    unavailable: usable,
  },
  {
    id: 'oecd',
    series: (id, start, end) => oecd.series(id, start, end),
    search: (query, limit) => oecd.search(query, limit),
    unavailable: usable,
  },
  {
    id: 'imf',
    series: (id, start, end) => imf.series(id, start, end),
    search: (query, limit) => imf.search(query, limit),
    unavailable: usable,
  },
  {
    id: 'cftc',
    series: (id, start, end) => cftc.getSeries(id, start, end),
    search: (query, limit) => cftc.search(query, limit),
    unavailable: usable,
  },
  {
    id: 'eia',
    series: (id, start, end) => eia.getSeries(id, start, end),
    search: (query, limit) => eia.search(query, limit),
    // The EIA serves nothing anonymously, so this is a hard gate rather than a
    // degraded mode: asking it without a key can only ever produce a 403.
    unavailable: () =>
      eia.hasCredentials()
        ? null
        : 'Set EIA_API_KEY — the EIA publishes nothing anonymously. Free from ' +
          'https://www.eia.gov/opendata/register.php.',
  },
];

const BY_ID = new Map<DataSourceId, SeriesProvider>(PROVIDERS.map((p) => [p.id, p]));

/** Every publisher `ECO` can chart, whether or not this deployment can reach it. */
export function seriesProviderIds(): readonly DataSourceId[] {
  return PROVIDERS.map((p) => p.id);
}

function providerFor(id: DataSourceId): SeriesProvider {
  const provider = BY_ID.get(id);
  if (!provider) {
    const info = dataSourceInfo(id);
    throw new UpstreamError(`${info.label} does not publish chartable series`, {
      code: 'unsupported',
      hint:
        info.kind === 'documents'
          ? `${info.label} publishes documents rather than observations — use its own command.`
          : `${info.label} feeds price charts (STK, CRY, IMP) rather than ECO.`,
    });
  }
  return provider;
}

/* ---------------------------------------------------------------- series */

/** Chart `[source:]id`, defaulting to FRED for an unprefixed reference. */
export async function getSeries(
  reference: string,
  start?: string,
  end?: string,
): Promise<DataSeriesResponse> {
  let ref;
  try {
    ref = parseDataRef(reference);
  } catch (err) {
    throw new UpstreamError(err instanceof Error ? err.message : `Bad reference "${reference}"`, {
      code: 'bad_request',
      hint:
        'A reference is `[source:]id`, e.g. UNRATE, bls:LNS14000000, ' +
        'ecb:EXR/D.USD.EUR.SP00.A, fed:H15/RIFLGFCY10_N.B.',
    });
  }

  const provider = providerFor(ref.source);
  const blocked = provider.unavailable();
  if (blocked !== null) {
    throw new UpstreamError(`${dataSourceInfo(ref.source).label} is not configured`, {
      code: 'not_configured',
      hint: blocked,
    });
  }

  return provider.series(ref.id, start, end);
}

/* ---------------------------------------------------------------- search */

/**
 * Ask every usable publisher at once and merge what comes back.
 *
 * One publisher being down must not empty the board, so failures are collected
 * and reported beside the results rather than thrown: a search that found
 * fourteen series and lost the OECD to a throttle is a useful answer, and
 * saying which one is missing is the difference between that and "no match".
 */
export async function searchSeries(
  query: string,
  sources?: readonly DataSourceId[],
  limit = 40,
): Promise<DataSearchResponse> {
  const q = query.trim();
  if (!q) {
    throw new UpstreamError('Search needs at least one word', {
      code: 'bad_request',
      hint: 'e.g. `ECOS unemployment`, `ECOS oil eia`, `ECOS inflation ecb oecd`.',
    });
  }

  const wanted = sources?.length ? PROVIDERS.filter((p) => sources.includes(p.id)) : PROVIDERS;

  const skipped: { provider: string; hint: string }[] = [];
  const unavailable: { provider: string; error: string }[] = [];

  // Per provider, ask for a full share of the limit rather than an equal slice:
  // most queries match in one or two catalogues, and pre-dividing would cap a
  // good publisher at five results to reserve room for seven that matched none.
  const perProvider = Math.min(Math.max(limit, 5), 100);

  const settled = await Promise.all(
    wanted.map(async (provider) => {
      const blocked = provider.unavailable();
      if (blocked !== null) {
        skipped.push({ provider: provider.id, hint: blocked });
        return [] as DataSearchResult[];
      }
      try {
        return await provider.search(q, perProvider);
      } catch (err) {
        unavailable.push({
          provider: provider.id,
          error: err instanceof Error ? err.message : String(err),
        });
        return [] as DataSearchResult[];
      }
    }),
  );

  // Interleave rather than concatenate. Straight concatenation would fill the
  // whole board with FRED — it has 800,000 series and always matches — and bury
  // the primary publisher a reader asked `ECOS` in order to discover.
  const queues = settled.filter((r) => r.length > 0);
  const results: DataSearchResult[] = [];
  for (let round = 0; results.length < limit; round++) {
    const before = results.length;
    for (const queue of queues) {
      const item = queue[round];
      if (item) results.push(item);
      if (results.length >= limit) break;
    }
    if (results.length === before) break;
  }

  return { query: q, results, unavailable, skipped };
}

/* --------------------------------------------------------------- statuses */

/** Availability of each source's credential, for the `SRC` board and `/health`. */
function credentialState(id: DataSourceId): { available: boolean; note: string } {
  const info = dataSourceInfo(id);
  const provider = BY_ID.get(id);

  if (provider) {
    const blocked = provider.unavailable();
    if (blocked !== null) return { available: false, note: blocked };
  }

  if (id === 'eia') return { available: eia.hasCredentials(), note: '' };
  if (id === 'polygon') {
    return polygon.hasCredentials()
      ? { available: true, note: 'Leads the STK / CRY / IMP price chain.' }
      : {
          available: false,
          note:
            'Set POLYGON_API_KEY to put a licensed tape in front of Yahoo and Nasdaq. ' +
            'Without it, prices still work — this source simply is not used.',
        };
  }
  if (id === 'congress') {
    return {
      available: true,
      note: congress.usingDemoKey()
        ? 'Using the shared DEMO_KEY — throttled per IP. Set CONGRESS_API_KEY to lift it.'
        : 'Using CONGRESS_API_KEY.',
    };
  }
  if (id === 'datagov') {
    return {
      available: true,
      note: datagov.usingDemoKey()
        ? 'Using the shared DEMO_KEY — throttled per IP. Set DATAGOV_API_KEY to lift it.'
        : 'Using DATAGOV_API_KEY.',
    };
  }
  if (id === 'fred') {
    return {
      available: true,
      note: process.env.FRED_API_KEY?.trim()
        ? 'Scrape first, official API behind it.'
        : 'Scraping only. Set FRED_API_KEY to add the official API as a fallback — ' +
          'datacentre IPs are often refused by the scrape.',
    };
  }
  if (id === 'bls') {
    return {
      available: true,
      note: process.env.BLS_API_KEY?.trim()
        ? 'Registered: 500 queries a day, 20 years per call.'
        : 'Unregistered: 25 queries a day, 10 years per call. Set BLS_API_KEY to raise both.',
    };
  }

  return { available: true, note: info.key ? '' : 'No credential needed.' };
}

export function sourceStatuses(): DataSourceStatus[] {
  return DATA_SOURCES.map((info) => {
    const { available, note } = credentialState(info.id);
    return {
      id: info.id,
      label: info.label,
      code: info.code,
      prefix: info.prefix,
      kind: info.kind,
      covers: info.covers,
      site: info.site,
      idExample: info.bareRef ? info.idExample : `${info.prefix}${info.idExample}`,
      available,
      note,
    };
  });
}

/** Read a list of source names from a query parameter, rejecting unknown ones. */
export function parseSourceList(raw: string): DataSourceId[] {
  const out: DataSourceId[] = [];
  for (const token of raw.split(',').map((t) => t.trim()).filter(Boolean)) {
    if (!isDataSource(token)) {
      throw new UpstreamError(`"${token}" is not a data source`, {
        code: 'bad_request',
        hint: `Series sources are ${SERIES_SOURCE_IDS.join(', ')}.`,
      });
    }
    if (!out.includes(token)) out.push(token);
  }
  return out;
}
