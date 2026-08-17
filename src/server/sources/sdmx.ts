/**
 * SDMX, the dialect the ECB, the IMF and the OECD all answer in.
 *
 * All three publish through an SDMX 2.1 REST service, all three accept
 * `format=csv`, and all three then disagree about everything else: the ECB
 * names the value column `OBS_VALUE` and the title `TITLE`, the OECD repeats
 * every dimension as a code column *and* a label column and calls the title
 * `Measure`, the IMF spells frequency `FREQUENCY` where the other two spell it
 * `FREQ`. What they genuinely share is the two columns that matter —
 * `TIME_PERIOD` and `OBS_VALUE` — and the period vocabulary written into the
 * SDMX standard.
 *
 * So this module owns the two hard parts once: reading a period of any
 * frequency into a calendar date, and picking the descriptive columns out of a
 * document whose other headers are not knowable in advance. Each agency's own
 * module is then only its base URL, its URL grammar, and its catalogue.
 */

import type {
  DataObservation,
  DataSearchResult,
  DataSeriesResponse,
} from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { cellAt, firstCell, headerIndex, numberCell, parseCsv } from '../lib/csv.js';
import { UpstreamError, fetchText } from '../lib/http.js';
import { periodToDate } from '../lib/period.js';

/** An SDMX time period as the day it begins. See {@link periodToDate}. */
export const sdmxPeriodToDate = periodToDate;

/**
 * Column names each concept goes by, across the three agencies.
 *
 * Order is preference, not alternatives-of-equal-worth: `TITLE` is the ECB's
 * human sentence and `Measure` is the OECD's, so a document carrying both
 * should show the fuller one.
 */
const TITLE_COLUMNS = [
  'TITLE',
  'Series title',
  'TITLE_COMPL',
  'SERIES_NAME',
  'INDICATOR',
  'Measure',
  'MEASURE',
  'STRUCTURE_NAME',
] as const;

const UNIT_COLUMNS = [
  'UNIT',
  'Unit of measure',
  'UNIT_MEASURE',
  'UNIT_TYPE',
  'Unit multiplier',
] as const;

const FREQ_COLUMNS = ['FREQ', 'FREQUENCY', 'Frequency of observation', 'Frequency'] as const;

const ADJUSTMENT_COLUMNS = ['ADJUSTMENT', 'Adjustment', 'SEASONAL_ADJUSTMENT', 'ADJUST'] as const;

/** SDMX frequency codes, spelled the way FRED spells them for the same thing. */
const FREQUENCY_LABELS: Record<string, string> = {
  A: 'Annual',
  S: 'Semiannual',
  B: 'Semiannual',
  Q: 'Quarterly',
  M: 'Monthly',
  W: 'Weekly',
  D: 'Daily',
  H: 'Hourly',
};

export interface SdmxSeries {
  observations: DataObservation[];
  title: string;
  units: string;
  frequency: string;
  seasonalAdjustment: string;
  /** Distinct series keys seen, when the document carries a key column at all. */
  keys: string[];
  /**
   * How many series the document holds.
   *
   * Counted from repeated time periods rather than from a key column, because
   * only the ECB publishes one: the OECD spreads the key over a dozen dimension
   * columns and the IMF's first column is the dataflow, identical on every row.
   * Two rows for March 2026 is two series, whatever the columns are called.
   */
  seriesCount: number;
}

/**
 * Read an SDMX CSV document into one series.
 *
 * A key that under-specifies its dimensions matches many series and the CSV
 * then interleaves them, which would render as a sawtooth rather than as an
 * error. The distinct keys are counted and reported so the caller can say so
 * plainly instead of drawing nonsense.
 */
export function parseSdmxCsv(csv: string, agency: string): SdmxSeries {
  const rows = parseCsv(csv);
  const header = rows[0];

  if (!header || rows.length < 2) {
    throw new UpstreamError(`${agency} returned no observations`, {
      code: 'empty_upstream',
      hint: 'The key is valid but selects nothing. Widen it, or check the period range.',
    });
  }

  const index = headerIndex(header);
  if (!index.has('time_period') || !index.has('obs_value')) {
    throw new UpstreamError(`${agency} returned a document without TIME_PERIOD/OBS_VALUE`, {
      code: 'bad_upstream_body',
      hint: `Columns were: ${header.slice(0, 12).join(', ')}`,
    });
  }

  const observations: DataObservation[] = [];
  const keys = new Set<string>();
  const perPeriod = new Map<string, number>();
  let title = '';
  let units = '';
  let frequency = '';
  let adjustment = '';

  for (let i = 1; i < rows.length; i++) {
    const row = rows[i]!;
    const date = sdmxPeriodToDate(cellAt(row, index, 'TIME_PERIOD'));
    if (date === null) continue;

    observations.push({ date, value: numberCell(cellAt(row, index, 'OBS_VALUE')) });
    perPeriod.set(date, (perPeriod.get(date) ?? 0) + 1);

    const key = firstCell(row, index, ['KEY', 'SERIES_KEY']);
    if (key) keys.add(key);

    // Descriptive columns repeat on every row; the first non-empty one wins so a
    // gap in one row does not blank the panel's header.
    if (!title) title = firstCell(row, index, TITLE_COLUMNS);
    if (!units) units = firstCell(row, index, UNIT_COLUMNS);
    if (!frequency) frequency = firstCell(row, index, FREQ_COLUMNS);
    if (!adjustment) adjustment = firstCell(row, index, ADJUSTMENT_COLUMNS);
  }

  if (observations.length === 0) {
    throw new UpstreamError(`${agency} returned no readable observations`, {
      code: 'empty_upstream',
      hint: 'Every row had a time period this terminal could not date.',
    });
  }

  observations.sort((a, b) => a.date.localeCompare(b.date));

  return {
    observations,
    title,
    units,
    frequency: FREQUENCY_LABELS[frequency.toUpperCase()] ?? frequency,
    seasonalAdjustment: adjustment,
    keys: [...keys],
    seriesCount: Math.max(keys.size, ...perPeriod.values()),
  };
}

/**
 * Refuse a key that selected more than one series.
 *
 * Called by each agency module after parsing, with the id the reader typed, so
 * the message can tell them to narrow *their* key rather than describing SDMX.
 *
 * This is the failure that most needs catching, because it does not look like
 * one: an under-specified key returns 200 with a perfectly well-formed body,
 * and collapsing it to one point per date would draw a line that alternates
 * between countries. Better to say the key is too loose.
 */
export function assertSingleSeries(series: SdmxSeries, id: string, agency: string): void {
  if (series.seriesCount <= 1) return;
  const examples = series.keys.slice(0, 3).join(', ');
  throw new UpstreamError(`"${id}" matches ${series.seriesCount} ${agency} series, not one`, {
    code: 'bad_request',
    hint:
      `A partly-specified key selects every series under it, and they cannot be charted ` +
      `as one line. Fill in the empty dimensions to narrow it` +
      (examples ? ` — e.g. ${examples}.` : '.'),
  });
}

/** Duplicate dates collapse to the last value, which is the revised one. */
export function dedupeObservations(observations: DataObservation[]): DataObservation[] {
  const byDate = new Map<string, number | null>();
  for (const o of observations) byDate.set(o.date, o.value);
  return [...byDate].map(([date, value]) => ({ date, value })).sort((a, b) => a.date.localeCompare(b.date));
}

/* ------------------------------------------------------------ the agencies */

/** One well-known series, so `ECOS` can answer before a reader knows SDMX. */
export interface SdmxCatalogueEntry {
  /** `FLOW/KEY`, exactly as `ECO` would take it. */
  id: string;
  title: string;
  units?: string;
  frequency?: string;
  /** Extra words to match on that are not in the title — `jobless`, `oil`. */
  keywords?: string;
}

export interface SdmxAgencyConfig {
  /** {@link DataSourceId} of the agency. */
  provider: string;
  /** Human name, for error text. */
  agency: string;
  /** Service root, up to but not including `/data`. */
  base: string;
  /**
   * How this service is asked for CSV. The ECB and the OECD take a query
   * parameter and ignore the header; the IMF takes the header and ignores the
   * parameter. Sending both costs nothing and means one code path.
   */
  format: string;
  accept: string;
  /** Builds the page a reader clicks to check a series against the publisher. */
  webUrl(flow: string, key: string): string;
  /**
   * Attempts after the first. The OECD answers a burst of requests with HTTP
   * 500 rather than 429 — the same throttle, wearing a different number — and
   * that reads as an outage unless it is retried through.
   */
  retries?: number;
  /** First retry delay, doubling. Raised for a service that throttles in seconds. */
  retryBaseMs?: number;
  catalogue: readonly SdmxCatalogueEntry[];
}

/**
 * An SDMX agency as a pair of functions.
 *
 * Everything that differs between the ECB, the IMF and the OECD is in the
 * config above; everything that does not — splitting `FLOW/KEY`, the period
 * window, the cache key, the too-loose-key check, turning a catalogue into
 * search results — is here, once.
 */
export function sdmxSource(config: SdmxAgencyConfig): {
  series(id: string, start?: string, end?: string): Promise<DataSeriesResponse>;
  search(query: string, limit: number): DataSearchResult[];
} {
  /** `FLOW/KEY`. A flow with no key is legal SDMX and selects the whole flow. */
  const split = (id: string): { flow: string; key: string } => {
    const trimmed = id.trim().replace(/^\/+|\/+$/g, '');
    const slash = trimmed.indexOf('/');
    if (slash === -1) return { flow: trimmed, key: '' };
    return { flow: trimmed.slice(0, slash), key: trimmed.slice(slash + 1) };
  };

  return {
    async series(id, start, end) {
      const { flow, key } = split(id);
      if (!flow) {
        throw new UpstreamError(`"${id}" does not name a ${config.agency} dataflow`, {
          code: 'bad_request',
          hint: `Ids look like ${config.catalogue[0]?.id ?? 'FLOW/KEY'} — the dataflow, a slash, then the series key.`,
        });
      }

      const cacheKey = `${config.provider}:series:${flow}/${key}:${start ?? ''}:${end ?? ''}`;

      return cache.cached(cacheKey, TTL.fred, async () => {
        const url = new URL(
          `${config.base}/data/${encodeSdmxFlow(flow)}/${key ? encodeSdmxKey(key) : 'all'}`,
        );
        url.searchParams.set('format', config.format);
        // SDMX periods are as coarse as the series: sending a full date as
        // `startPeriod` against an annual flow is accepted by all three.
        if (start) url.searchParams.set('startPeriod', start);
        if (end) url.searchParams.set('endPeriod', end);

        const csv = await fetchText(url.toString(), {
          timeoutMs: 40_000,
          retries: config.retries ?? 1,
          ...(config.retryBaseMs === undefined ? {} : { retryBaseMs: config.retryBaseMs }),
          browserHeaders: false,
          headers: { Accept: config.accept },
        }).catch((err: unknown) => {
          throw asThrottle(err, config.agency);
        });

        const parsed = parseSdmxCsv(csv, config.agency);
        assertSingleSeries(parsed, id, config.agency);

        const known = config.catalogue.find((entry) => entry.id === `${flow}/${key}`);
        const observations = dedupeObservations(parsed.observations);
        const units = parsed.units || known?.units || '';

        return {
          series: {
            provider: config.provider,
            id: `${flow}/${key}`,
            title: known?.title || parsed.title || `${config.agency} ${flow} ${key}`.trim(),
            units,
            unitsShort: units,
            frequency: parsed.frequency || known?.frequency || '',
            seasonalAdjustment: parsed.seasonalAdjustment,
            lastUpdated: '',
            observationStart: observations[0]?.date ?? '',
            observationEnd: observations.at(-1)?.date ?? '',
            notes: '',
            source: 'sdmx',
            sourceUrl: config.webUrl(flow, key),
          },
          observations,
        };
      });
    },

    search(query, limit) {
      return searchCatalogue(config.provider, config.catalogue, query, limit);
    },
  };
}

/**
 * Name a throttle as a throttle.
 *
 * These services shed load with whatever status is nearest to hand — the OECD
 * alternates 429 and 500 for the same condition — and both read as "the
 * dataset is broken" unless someone says otherwise. It is not: the key is fine
 * and the answer is cached for half an hour once one request gets through.
 */
function asThrottle(err: unknown, agency: string): unknown {
  if (!(err instanceof UpstreamError)) return err;
  const status = err.status ?? 0;
  if (status !== 429 && status < 500) return err;

  return new UpstreamError(`${agency} is throttling this IP (HTTP ${status})`, {
    code: 'rate_limited',
    status,
    hint:
      `${agency}'s SDMX service limits requests per address and sheds the excess with ` +
      `429 or 500 — this is not a bad key. Retry in a minute; the answer is then cached ` +
      `for thirty.`,
  });
}

/**
 * Percent-encode a dataflow reference without destroying it.
 *
 * `encodeURIComponent` is wrong here: an OECD dataflow is `DSD_KEI@DF_KEI` and
 * an agency-qualified one is `OECD.SDD.STES,DSD_KEI@DF_KEI,4.0`. Escaping the
 * `@` to `%40` earns an HTTP 500 from the OECD's service, and escaping the
 * commas breaks the agency form. Both characters are legal in a URL path
 * segment, so this validates the shape instead of mangling it.
 */
export function encodeSdmxFlow(flow: string): string {
  if (!/^[A-Za-z0-9_@.,:+-]{1,120}$/.test(flow)) {
    throw new UpstreamError(`"${flow}" is not a valid SDMX dataflow reference`, {
      code: 'bad_request',
      hint: 'A dataflow is letters, digits and `_@.,:+-`, e.g. EXR or DSD_KEI@DF_KEI.',
    });
  }
  return flow;
}

/**
 * Percent-encode an SDMX key without destroying it.
 *
 * A key's dots and its `+` (an "or" between codes) are grammar, and
 * `encodeURIComponent` would escape the `+` into `%2B`, which the services read
 * as a literal plus and match nothing. Encoding each code separately keeps the
 * grammar and still escapes anything odd inside a code.
 */
export function encodeSdmxKey(key: string): string {
  return key
    .split('.')
    .map((part) => part.split('+').map(encodeURIComponent).join('+'))
    .join('.');
}

/**
 * Rank a curated catalogue against a query.
 *
 * These agencies publish no free-text search endpoint — the OECD's data
 * explorer is a JavaScript application over a structure API, and matching a
 * reader's words against 20,000 dataflow names would need the whole structure
 * document on every keystroke. A short, checked list of the series people
 * actually ask for answers the question that matters ("what do I type?") and is
 * honest about being a list rather than a search.
 */
export function searchCatalogue(
  provider: string,
  catalogue: readonly SdmxCatalogueEntry[],
  query: string,
  limit: number,
): DataSearchResult[] {
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);

  const scored = catalogue.map((entry) => {
    const haystack = `${entry.id} ${entry.title} ${entry.keywords ?? ''}`.toLowerCase();
    // Every term must appear, so `ecb oil` does not return every ECB series.
    const score = terms.every((term) => haystack.includes(term))
      ? terms.reduce((total, term) => total + (entry.title.toLowerCase().includes(term) ? 2 : 1), 0)
      : 0;
    return { entry, score };
  });

  return scored
    .filter((s) => s.score > 0)
    .sort((a, b) => b.score - a.score || a.entry.title.localeCompare(b.entry.title))
    .slice(0, limit)
    .map(({ entry }) => ({
      provider,
      id: entry.id,
      title: entry.title,
      ...(entry.units ? { units: entry.units } : {}),
      ...(entry.frequency ? { frequency: entry.frequency } : {}),
    }));
}
