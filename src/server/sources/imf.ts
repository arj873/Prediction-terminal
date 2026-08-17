/**
 * International Monetary Fund — the SDMX service at api.imf.org.
 *
 * The IMF's value is coverage rather than depth: the same statistic, compiled
 * to one methodology, for nearly every country on earth. That is the feed for a
 * question about an economy no national statistics office publishes in English.
 *
 * An id is `FLOW/KEY`, e.g. `CPI/USA.CPI._T.IX.M`. Two things about this
 * service are worth knowing before typing a key:
 *
 *  - Countries are **ISO alpha-3**. `US` is not a code the IMF knows, and
 *    asking for one returns HTTP 200 with a body containing the dataflow's
 *    description and no observations at all. That is why {@link parseSdmxCsv}
 *    treats an observation-free document as an error with a hint rather than as
 *    an empty series.
 *  - Some dataflows are served only to registered callers and answer 403 to
 *    anyone else. The catalogue below is limited to flows that answer
 *    anonymously, which is what this terminal can actually promise.
 */

import { sdmxSource, type SdmxCatalogueEntry } from './sdmx.js';

const BASE = process.env.IMF_API_BASE ?? 'https://api.imf.org/external/sdmx/2.1';

const COUNTRIES: readonly { code: string; name: string }[] = [
  { code: 'USA', name: 'United States' },
  { code: 'GBR', name: 'United Kingdom' },
  { code: 'DEU', name: 'Germany' },
  { code: 'FRA', name: 'France' },
  { code: 'JPN', name: 'Japan' },
  { code: 'CHN', name: 'China' },
  { code: 'IND', name: 'India' },
  { code: 'BRA', name: 'Brazil' },
  { code: 'CAN', name: 'Canada' },
  { code: 'MEX', name: 'Mexico' },
  { code: 'TUR', name: 'Türkiye' },
  { code: 'ARG', name: 'Argentina' },
];

const CATALOGUE: readonly SdmxCatalogueEntry[] = [
  ...COUNTRIES.map((c) => ({
    id: `CPI/${c.code}.CPI._T.IX.M`,
    title: `${c.name} — consumer price index, all items`,
    units: 'Index',
    frequency: 'Monthly',
    keywords: `inflation cpi prices ${c.code} imf`,
  })),
  ...COUNTRIES.filter((c) => c.code !== 'USA').map((c) => ({
    id: `ER/${c.code}.USD_XDC.PA_RT.M`,
    title: `${c.name} — US dollar exchange rate, period average`,
    units: 'National currency per USD',
    frequency: 'Monthly',
    keywords: `fx forex currency exchange rate ${c.code} imf`,
  })),
];

export const imf = sdmxSource({
  provider: 'imf',
  agency: 'the IMF',
  base: BASE,
  // The IMF reads the Accept header and ignores `?format=`; the ECB and OECD do
  // the reverse. Sending both is one code path instead of a per-agency branch.
  format: 'csv',
  accept: 'application/vnd.sdmx.data+csv;version=2.0.0',
  webUrl: (flow) => `https://data.imf.org/en/Data-Explorer?datasetUrn=IMF.STA:${encodeURIComponent(flow)}`,
  catalogue: CATALOGUE,
});
