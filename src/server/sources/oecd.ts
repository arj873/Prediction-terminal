/**
 * OECD — the SDMX service behind the OECD Data Explorer.
 *
 * The value here is comparability: the same definition of "unemployment rate"
 * or "consumer prices" across 38 member countries plus the euro area, the G7
 * and the G20, which is what a question phrased as "will inflation be higher in
 * the US than the euro area" actually needs.
 *
 * An id is `DATAFLOW/KEY`. The Key Economic Indicators flow — `DSD_KEI@DF_KEI`
 * — orders its dimensions
 * `REF_AREA.FREQ.MEASURE.UNIT_MEASURE.ACTIVITY.ADJUSTMENT.TRANSFORMATION`, so
 * US CPI year-on-year is `USA.M.CP.GR._Z._Z.GY`. Swapping `USA` for `EA20`,
 * `DEU`, `GBR`, `JPN` or `G20` gives the same statistic elsewhere, which is the
 * point of using the OECD for it rather than each country's own publisher.
 */

import { sdmxSource, type SdmxCatalogueEntry } from './sdmx.js';

const BASE = process.env.OECD_API_BASE ?? 'https://sdmx.oecd.org/public/rest';

const KEI = 'DSD_KEI@DF_KEI';

/** The comparable headline indicators, per area, built rather than hand-listed. */
const AREAS: readonly { code: string; name: string }[] = [
  { code: 'USA', name: 'United States' },
  { code: 'EA20', name: 'Euro area (20 countries)' },
  { code: 'DEU', name: 'Germany' },
  { code: 'GBR', name: 'United Kingdom' },
  { code: 'JPN', name: 'Japan' },
  { code: 'CAN', name: 'Canada' },
  { code: 'FRA', name: 'France' },
  { code: 'G20', name: 'G20' },
];

/**
 * The indicators, as `MEASURE.UNIT_MEASURE.ACTIVITY.ADJUSTMENT.TRANSFORMATION`
 * — the five dimensions after area and frequency.
 */
const INDICATORS: readonly {
  tail: string;
  freq: string;
  title: string;
  units: string;
  keywords: string;
}[] = [
  {
    tail: 'CP.GR._Z._Z.GY',
    freq: 'M',
    title: 'consumer prices, year on year',
    units: 'Percent',
    keywords: 'inflation cpi prices',
  },
  {
    tail: 'UNEMP.PT_LF._T.Y._Z',
    freq: 'M',
    title: 'unemployment rate',
    units: 'Percent of labour force',
    keywords: 'jobless labour employment',
  },
  {
    tail: 'IRLT.PA._Z._Z._Z',
    freq: 'M',
    title: 'long-term interest rate (10-year government bond)',
    units: 'Percent per annum',
    keywords: 'yield rates bond govvie 10y',
  },
  {
    tail: 'IR3TIB.PA._Z._Z._Z',
    freq: 'M',
    title: 'short-term interest rate (3-month interbank)',
    units: 'Percent per annum',
    keywords: 'rates money market libor 3m',
  },
  {
    tail: 'LI.IX._T.AA._Z',
    freq: 'M',
    title: 'composite leading indicator (CLI)',
    units: 'Index, long-term average = 100',
    keywords: 'cli leading cycle turning point recession',
  },
  {
    tail: 'B1GQ_Q.GR._T.Y.GY',
    freq: 'Q',
    title: 'GDP volume, year on year',
    units: 'Percent',
    keywords: 'gdp growth output recession',
  },
  {
    tail: 'PRVM.IX.C.Y._Z',
    freq: 'M',
    title: 'manufacturing production volume',
    units: 'Index',
    keywords: 'industrial production manufacturing output',
  },
  {
    tail: 'CCICP.PB._Z.Y._Z',
    freq: 'M',
    title: 'consumer confidence',
    units: 'Normalised index',
    keywords: 'sentiment survey confidence',
  },
];

const CATALOGUE: readonly SdmxCatalogueEntry[] = AREAS.flatMap((area) =>
  INDICATORS.map((indicator) => ({
    id: `${KEI}/${area.code}.${indicator.freq}.${indicator.tail}`,
    title: `${area.name} — ${indicator.title}`,
    units: indicator.units,
    frequency: indicator.freq === 'M' ? 'Monthly' : 'Quarterly',
    keywords: `${indicator.keywords} ${area.code} oecd`,
  })),
);

export const oecd = sdmxSource({
  provider: 'oecd',
  agency: 'the OECD',
  base: BASE,
  format: 'csvfilewithlabels',
  accept: 'text/csv, application/vnd.sdmx.data+csv;version=1.0.0',
  retries: 3,
  retryBaseMs: 2_500,
  // The Data Explorer is a single-page application with no stable per-series
  // permalink, so this links to the query itself — which is the thing a reader
  // would need in order to check the number anyway.
  webUrl: (flow, key) =>
    `${BASE}/data/${encodeURIComponent(flow)}/${encodeURIComponent(key)}?format=csvfilewithlabels`,
  catalogue: CATALOGUE,
});
