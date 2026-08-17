/**
 * European Central Bank — the ECB Data Portal's SDMX service.
 *
 * Keyless, generous, and the publisher of record for everything the euro area
 * settles on: the policy rates themselves, HICP, M3, the AAA yield curve and
 * the daily reference exchange rates that most of the world's EUR conversions
 * are struck at.
 *
 * An id here is `FLOW/KEY` — `EXR/D.USD.EUR.SP00.A` is the daily USD reference
 * rate. The key's dots are dimensions in the order the dataflow declares them,
 * and leaving one empty means "any", which is why {@link sdmxSource} refuses a
 * key that ends up selecting more than one series rather than charting their
 * interleaving.
 */

import { sdmxSource, type SdmxCatalogueEntry } from './sdmx.js';

const BASE = process.env.ECB_API_BASE ?? 'https://data-api.ecb.europa.eu/service';

/**
 * The euro-area series a reader is most likely to want, checked against the
 * live service. Any other key still works — this is what `ECOS` can offer
 * before you know one, not a limit on what `ECO` will fetch.
 */
const CATALOGUE: readonly SdmxCatalogueEntry[] = [
  {
    id: 'FM/D.U2.EUR.4F.KR.MRR_FR.LEV',
    title: 'ECB main refinancing operations rate (fixed rate)',
    units: 'Percent per annum',
    frequency: 'Daily',
    keywords: 'policy rate refi mro hike cut',
  },
  {
    id: 'FM/D.U2.EUR.4F.KR.DFR.LEV',
    title: 'ECB deposit facility rate',
    units: 'Percent per annum',
    frequency: 'Daily',
    keywords: 'policy rate deposit hike cut',
  },
  {
    id: 'FM/D.U2.EUR.4F.KR.MLFR.LEV',
    title: 'ECB marginal lending facility rate',
    units: 'Percent per annum',
    frequency: 'Daily',
    keywords: 'policy rate lending',
  },
  {
    id: 'EST/B.EU000A2X2A25.WT',
    title: '€STR — euro short-term rate, volume-weighted trimmed mean',
    units: 'Percent per annum',
    frequency: 'Daily',
    keywords: 'ester overnight money market benchmark',
  },
  {
    id: 'ICP/M.U2.N.000000.4.ANR',
    title: 'HICP — euro area headline inflation, annual rate',
    units: 'Percent per annum',
    frequency: 'Monthly',
    keywords: 'inflation cpi hicp prices',
  },
  {
    id: 'ICP/M.U2.N.XEF000.4.ANR',
    title: 'HICP excluding energy and food — euro area core inflation, annual rate',
    units: 'Percent per annum',
    frequency: 'Monthly',
    keywords: 'core inflation cpi hicp prices',
  },
  {
    id: 'BSI/M.U2.Y.V.M30.X.I.U2.2300.Z01.A',
    title: 'M3 monetary aggregate — euro area, annual growth rate',
    units: 'Percent per annum',
    frequency: 'Monthly',
    keywords: 'money supply monetary aggregate',
  },
  {
    id: 'LFSI/M.I9.S.UNEHRT.TOTAL0.15_74.T',
    title: 'Euro area unemployment rate',
    units: 'Percent of labour force',
    frequency: 'Monthly',
    keywords: 'jobless labour market employment',
  },
  {
    id: 'YC/B.U2.EUR.4F.G_N_A.SV_C_YM.SR_10Y',
    title: 'Euro area AAA government bond spot yield, 10 years',
    units: 'Percent per annum',
    frequency: 'Daily',
    keywords: 'yield curve bund govvie rates 10y',
  },
  {
    id: 'YC/B.U2.EUR.4F.G_N_A.SV_C_YM.SR_2Y',
    title: 'Euro area AAA government bond spot yield, 2 years',
    units: 'Percent per annum',
    frequency: 'Daily',
    keywords: 'yield curve bund govvie rates 2y',
  },
  {
    id: 'EXR/D.USD.EUR.SP00.A',
    title: 'US dollar / euro reference exchange rate',
    units: 'USD per EUR',
    frequency: 'Daily',
    keywords: 'fx forex eurusd dollar',
  },
  {
    id: 'EXR/D.GBP.EUR.SP00.A',
    title: 'Pound sterling / euro reference exchange rate',
    units: 'GBP per EUR',
    frequency: 'Daily',
    keywords: 'fx forex eurgbp sterling',
  },
  {
    id: 'EXR/D.JPY.EUR.SP00.A',
    title: 'Japanese yen / euro reference exchange rate',
    units: 'JPY per EUR',
    frequency: 'Daily',
    keywords: 'fx forex eurjpy yen',
  },
  {
    id: 'EXR/D.CHF.EUR.SP00.A',
    title: 'Swiss franc / euro reference exchange rate',
    units: 'CHF per EUR',
    frequency: 'Daily',
    keywords: 'fx forex eurchf franc',
  },
  {
    id: 'MIR/M.U2.B.A2C.AM.R.A.2250.EUR.N',
    title: 'Euro area bank lending rate to households for house purchase',
    units: 'Percent per annum',
    frequency: 'Monthly',
    keywords: 'mortgage lending rate households credit',
  },
];

export const ecb = sdmxSource({
  provider: 'ecb',
  agency: 'the ECB',
  base: BASE,
  format: 'csvdata',
  accept: 'text/csv, application/vnd.sdmx.data+csv;version=1.0.0',
  webUrl: (flow, key) =>
    `https://data.ecb.europa.eu/data/datasets/${encodeURIComponent(flow)}/${encodeURIComponent(`${flow}.${key}`)}`,
  catalogue: CATALOGUE,
});
