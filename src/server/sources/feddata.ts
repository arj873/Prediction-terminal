/**
 * Federal Reserve Board — the Data Download Program at federalreserve.gov.
 *
 * This is the Board's own publication, not FRED's mirror of it: H.15 constant
 * maturity yields, the H.4.1 balance sheet, H.6 money stock, G.17 industrial
 * production. Where FRED re-publishes these under its own ids, the DDP is where
 * they appear first and how the Board itself words them.
 *
 * The awkward part is identity. The DDP addresses data by an MD5 hash of a
 * *selection* — `bf17364827e38702b42a58cf8eaa3f78` is "the Treasury constant
 * maturity package of H.15" — and there is no endpoint that maps a series to
 * its package. Asking a reader to type a hash would be absurd, so this module
 * builds the mapping instead: the release's chooser page lists its packages,
 * and one one-observation request per package reveals which series each holds.
 * That index is cached for the session, after which `fed:H15/RIFLGFCY10_N.B`
 * resolves in a single request.
 *
 * An id is therefore `RELEASE/SERIES`, both as the Board writes them.
 */

import * as cheerio from 'cheerio';
import type { DataObservation, DataSearchResult, DataSeriesResponse } from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { numberCell, parseCsv } from '../lib/csv.js';
import { UpstreamError, fetchText } from '../lib/http.js';
import { periodToDate } from '../lib/period.js';
import { searchCatalogue, type SdmxCatalogueEntry } from './sdmx.js';

const BASE = process.env.FED_DDP_BASE ?? 'https://www.federalreserve.gov/datadownload';

/** Releases the DDP publishes, with the Board's own names. */
const RELEASES: Record<string, string> = {
  H15: 'H.15 Selected Interest Rates',
  H41: 'H.4.1 Factors Affecting Reserve Balances',
  H6: 'H.6 Money Stock Measures',
  H8: 'H.8 Assets and Liabilities of Commercial Banks',
  H10: 'H.10 Foreign Exchange Rates',
  H3: 'H.3 Aggregate Reserves of Depository Institutions',
  G17: 'G.17 Industrial Production and Capacity Utilization',
  G19: 'G.19 Consumer Credit',
  G20: 'G.20 Finance Companies',
  Z1: 'Z.1 Financial Accounts of the United States',
  CP: 'Commercial Paper',
  PRATES: 'Policy Rates',
  SLOOS: 'Senior Loan Officer Opinion Survey',
  DSR: 'Household Debt Service and Financial Obligations Ratios',
  CHGDEL: 'Charge-Off and Delinquency Rates',
  E2: 'E.2 Survey of Terms of Business Lending',
  FOR: 'Household Financial Obligations',
};

/** `H15/RIFLGFCY10_N.B` → the two halves, release upper-cased. */
export function splitFedId(id: string): { release: string; series: string } {
  const trimmed = id.trim().replace(/^\/+|\/+$/g, '');
  const slash = trimmed.indexOf('/');
  if (slash === -1) return { release: trimmed.toUpperCase(), series: '' };
  return {
    release: trimmed.slice(0, slash).toUpperCase().replace(/\./g, ''),
    series: trimmed.slice(slash + 1).trim(),
  };
}

/* ------------------------------------------------------------------- csv */

/** One column of a DDP package: a series and everything the header says about it. */
export interface FedColumn {
  /** The Board's short name, e.g. `RIFLGFCY10_N.B` — the half a reader types. */
  name: string;
  description: string;
  unit: string;
  multiplier: number;
  currency: string;
  /** The fully-qualified `H15/H15/RIFLGFCY10_N.B` form. */
  uniqueId: string;
}

export interface FedPackage {
  columns: FedColumn[];
  /**
   * Rows, in file order, dated `YYYY-MM-DD`. Values align with {@link columns}
   * by index.
   */
  rows: { date: string; values: (number | null)[] }[];
}

/**
 * Parse a `layout=seriescolumn&label=include` package.
 *
 * The file is six labelled header rows — description, unit, multiplier,
 * currency, unique identifier, short name — followed by dated observations. The
 * headers are keyed by the *first cell* rather than by row number, because the
 * Board does not emit every one of them for every release: H.4.1 omits the
 * currency row, and reading by position would then shift the short names into
 * the unique-identifier slot and leave every series unnameable.
 */
export function parseFedPackage(csv: string): FedPackage {
  const rows = parseCsv(csv);
  const header = new Map<string, string[]>();
  const data: { date: string; values: (number | null)[] }[] = [];

  for (const row of rows) {
    const first = (row[0] ?? '').trim();

    // The DDP dates a row in whatever period the release publishes: `2026-08-12`
    // for H.15 daily, `2026-08` for its monthly averages, `2026` for an annual
    // series. Matching only the daily form treated every monthly row as a header
    // and returned a package with no observations in it at all.
    const date = periodToDate(first);
    if (date !== null) {
      data.push({ date, values: row.slice(1).map((cell) => numberCell(cell)) });
      continue;
    }

    // `"Unique Identifier: "` ships with a trailing space, and `"Unit:"` with a
    // colon; key on the word so both forms land in the same slot.
    const key = first.replace(/[:\s]+$/, '').trim().toLowerCase();
    if (key) header.set(key, row.slice(1));
  }

  const names = header.get('time period') ?? [];
  const descriptions = header.get('series description') ?? [];
  const units = header.get('unit') ?? [];
  const multipliers = header.get('multiplier') ?? [];
  const currencies = header.get('currency') ?? [];
  const uniqueIds = header.get('unique identifier') ?? [];

  const columns: FedColumn[] = names.map((name, i) => ({
    name: name.trim(),
    description: (descriptions[i] ?? '').replace(/\s+/g, ' ').trim(),
    // The Board writes units as `Percent:_Per_Year` — underscores for spaces
    // and a colon between the measure and its qualifier. Both are punctuation,
    // and the part before the colon is the unit itself, so neither is dropped:
    // "Per Year" alone does not say what is being measured.
    unit: (units[i] ?? '').replace(/_/g, ' ').replace(/:/g, ' ').replace(/\s+/g, ' ').trim(),
    multiplier: numberCell(multipliers[i]) ?? 1,
    currency: (currencies[i] ?? '').trim(),
    uniqueId: (uniqueIds[i] ?? '').trim(),
  }));

  return { columns, rows: data };
}

/* --------------------------------------------------------------- packages */

/** A preformatted package offered on a release's chooser page. */
interface PackageRef {
  hash: string;
  label: string;
}

/**
 * The packages a release offers.
 *
 * Scraped, because the DDP has no machine-readable index of itself. Every
 * package is an element carrying `rel=…&series=<hash>…` in its `value`, so the
 * hash comes from there — it is a URL, and therefore stable — and the label
 * from the text beside it, degrading to the hash if the wording moves.
 *
 * Two markups, because the DDP is inconsistent about it: H.15 renders its
 * packages as radio `<input>`s with the label as sibling text, while G.17 uses
 * a `<select>` whose `<option>`s carry their own label. Matching only the first
 * silently found nothing for half the releases — and "no packages" is
 * indistinguishable from "no such release" unless both are handled.
 */
export function parseChooserPage(html: string): PackageRef[] {
  const $ = cheerio.load(html);
  const seen = new Set<string>();
  const packages: PackageRef[] = [];

  $('input[value*="series="], option[value*="series="]').each((_, el) => {
    const value = $(el).attr('value') ?? '';
    const hash = /series=([a-f0-9]{32})/.exec(value)?.[1];
    if (!hash || seen.has(hash)) return;
    seen.add(hash);

    // An `<option>` holds its own label; an `<input>`'s sits beside it. Both
    // end with a `[csv, …]` size annotation that is noise here.
    const node = $(el);
    const text = node.is('option') ? node.text() : node.parent().text();
    const label = (text ?? '').replace(/\s+/g, ' ').trim().split('[')[0]?.trim();

    packages.push({ hash, label: label || hash });
  });

  return packages;
}

function packageUrl(
  release: string,
  hash: string,
  options: { lastObs?: number; from?: string; to?: string },
): string {
  const url = new URL(`${BASE}/Output.aspx`);
  url.searchParams.set('rel', release);
  url.searchParams.set('series', hash);
  url.searchParams.set('filetype', 'csv');
  url.searchParams.set('label', 'include');
  url.searchParams.set('layout', 'seriescolumn');
  url.searchParams.set('type', 'package');

  // The DDP is particular in three ways, all of which fail silently — it
  // answers HTTP 200 with a zero-byte body rather than complaining:
  //
  //   - `lastobs` must be *present and empty* for a from/to range to count,
  //     and absent entirely for the full history. `lastobs=0` returns headers
  //     and no observations.
  //   - a range needs *both* bounds. An open-ended `to=` returns nothing at
  //     all rather than "up to today", so an unspecified side is filled in.
  //   - `from` earlier than the release's own start is fine, which is what
  //     makes filling one in safe.
  if (options.lastObs !== undefined) {
    url.searchParams.set('lastobs', String(options.lastObs));
  } else if (options.from || options.to) {
    url.searchParams.set('lastobs', '');
    url.searchParams.set('from', options.from || '01/01/1900');
    url.searchParams.set('to', options.to || toDdpDate(new Date().toISOString().slice(0, 10)));
  }

  return url.toString();
}

/** `2015-06-30` → `06/30/2015`, the only date form the DDP accepts. */
export function toDdpDate(iso: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(iso.trim());
  return m ? `${m[2]}/${m[3]}/${m[1]}` : '';
}

/** series name → the package holding it, for one release. */
type ReleaseIndex = Map<string, { hash: string; column: FedColumn }>;

async function releaseIndex(release: string): Promise<ReleaseIndex> {
  const key = `fed:index:${release}`;
  const cached = cache.get<ReleaseIndex>(key);
  if (cached) return cached;

  const html = await fetchText(`${BASE}/Choose.aspx?rel=${encodeURIComponent(release)}`, {
    timeoutMs: 30_000,
    retries: 1,
  });
  const packages = parseChooserPage(html);

  if (packages.length === 0) {
    throw new UpstreamError(`The Federal Reserve publishes no release called ${release}`, {
      code: 'not_found',
      hint: `Releases are ${Object.keys(RELEASES).slice(0, 8).join(', ')} and a few more.`,
    });
  }

  const index: ReleaseIndex = new Map();

  // One observation per package is enough to learn its column names, and keeps
  // the index cheap: the H.15 full history is a megabyte, its header is 2 KB.
  const heads = await Promise.all(
    packages.map(async (pkg) => {
      const csv = await fetchText(packageUrl(release, pkg.hash, { lastObs: 1 }), {
        timeoutMs: 30_000,
        retries: 1,
      }).catch(() => '');
      return { pkg, parsed: csv ? parseFedPackage(csv) : null };
    }),
  );

  for (const { pkg, parsed } of heads) {
    for (const column of parsed?.columns ?? []) {
      // First package wins: the Board offers the same series at several
      // frequencies, and the first listed is the primary one.
      if (column.name && !index.has(column.name)) index.set(column.name, { hash: pkg.hash, column });
    }
  }

  cache.set(key, index, TTL.catalogue);
  return index;
}

/* ---------------------------------------------------------------- public */

export async function getSeries(
  rawId: string,
  start?: string,
  end?: string,
): Promise<DataSeriesResponse> {
  const { release, series } = splitFedId(rawId);

  if (!release) {
    throw new UpstreamError(`"${rawId}" does not name a Federal Reserve release`, {
      code: 'bad_request',
      hint: 'Ids look like H15/RIFLGFCY10_N.B — the release, a slash, then the series.',
    });
  }

  const index = await releaseIndex(release);

  if (!series) {
    throw new UpstreamError(`${release} holds ${index.size} series — name one`, {
      code: 'bad_request',
      hint:
        `e.g. ${[...index.keys()].slice(0, 5).map((n) => `${release}/${n}`).join(', ')}. ` +
        `\`ECOS <words> fed\` searches them.`,
    });
  }

  const entry = index.get(series) ?? index.get(series.toUpperCase());
  if (!entry) {
    throw new UpstreamError(`${release} has no series called ${series}`, {
      code: 'not_found',
      hint: `Series in ${release} look like ${[...index.keys()].slice(0, 4).join(', ')}.`,
    });
  }

  const cacheKey = `fed:series:${release}/${series}:${start ?? ''}:${end ?? ''}`;

  return cache.cached(cacheKey, TTL.fred, async () => {
    const csv = await fetchText(
      packageUrl(release, entry.hash, {
        ...(start ? { from: toDdpDate(start) } : {}),
        ...(end ? { to: toDdpDate(end) } : {}),
      }),
      { timeoutMs: 60_000, retries: 1 },
    );

    const parsed = parseFedPackage(csv);
    const at = parsed.columns.findIndex((c) => c.name === entry.column.name);

    if (at === -1) {
      throw new UpstreamError(`${release} no longer returns a column for ${series}`, {
        code: 'empty_upstream',
      });
    }

    const observations: DataObservation[] = parsed.rows.map((row) => ({
      date: row.date,
      value: row.values[at] ?? null,
    }));

    if (observations.length === 0) {
      throw new UpstreamError(`The Federal Reserve returned no observations for ${rawId}`, {
        code: 'empty_upstream',
      });
    }

    const column = parsed.columns[at]!;

    return {
      series: {
        provider: 'fed',
        id: `${release}/${column.name}`,
        title: column.description || column.name,
        units: column.unit,
        unitsShort: column.unit,
        frequency: frequencyOf(column.name),
        seasonalAdjustment: '',
        lastUpdated: '',
        observationStart: observations[0]?.date ?? '',
        observationEnd: observations.at(-1)?.date ?? '',
        notes: RELEASES[release] ? `Release: ${RELEASES[release]}` : '',
        source: 'ddp',
        sourceUrl: `${BASE}/Choose.aspx?rel=${encodeURIComponent(release)}`,
      },
      observations,
    };
  });
}

/**
 * Frequency from the series name's suffix.
 *
 * The Board encodes it there — `.B` business daily, `.M` monthly, `.WF` weekly
 * — and publishes it nowhere else in the CSV, so reading the suffix is the only
 * way to label the panel without a second request.
 */
function frequencyOf(name: string): string {
  const suffix = name.split('.').pop()?.toUpperCase() ?? '';
  const known: Record<string, string> = {
    B: 'Business daily',
    D: 'Daily',
    WF: 'Weekly (Friday)',
    WW: 'Weekly (Wednesday)',
    W: 'Weekly',
    M: 'Monthly',
    Q: 'Quarterly',
    A: 'Annual',
  };
  return known[suffix] ?? '';
}

/** The Board's headline series, so `ECOS` can answer before you know a name. */
const CATALOGUE: readonly SdmxCatalogueEntry[] = [
  {
    id: 'H15/RIFLGFCY10_N.B',
    title: '10-year Treasury constant maturity yield',
    units: 'Percent per year',
    frequency: 'Business daily',
    keywords: 'treasury yield 10y rates govvie h15',
  },
  {
    id: 'H15/RIFLGFCY02_N.B',
    title: '2-year Treasury constant maturity yield',
    units: 'Percent per year',
    frequency: 'Business daily',
    keywords: 'treasury yield 2y rates govvie h15',
  },
  {
    id: 'H15/RIFLGFCY30_N.B',
    title: '30-year Treasury constant maturity yield',
    units: 'Percent per year',
    frequency: 'Business daily',
    keywords: 'treasury yield 30y long bond rates h15',
  },
  {
    id: 'H15/RIFLGFCM03_N.B',
    title: '3-month Treasury constant maturity yield',
    units: 'Percent per year',
    frequency: 'Business daily',
    keywords: 'treasury bill yield 3m rates h15',
  },
  {
    id: 'H15/RIFSPFF_N.M',
    title: 'Effective federal funds rate, monthly average',
    units: 'Percent per year',
    frequency: 'Monthly',
    keywords: 'fed funds effective policy rate eff h15 fomc',
  },
  {
    id: 'H15/RIFSPFF_N.WW',
    title: 'Effective federal funds rate, weekly average',
    units: 'Percent per year',
    frequency: 'Weekly (Wednesday)',
    keywords: 'fed funds effective policy rate eff h15 fomc',
  },
  {
    id: 'H15/RIFSPBLP_N.M',
    title: 'Bank prime loan rate, monthly average',
    units: 'Percent per year',
    frequency: 'Monthly',
    keywords: 'prime rate lending h15',
  },
  {
    id: 'PRATES/RESBM_N.D',
    title: 'Interest rate on reserve balances (IORB)',
    units: 'Percent',
    frequency: 'Daily',
    keywords: 'iorb reserves policy rate floor fomc',
  },
  {
    id: 'H41/RESPPALG_N.WW',
    title: 'Assets: securities held outright, Wednesday level',
    units: 'Millions of dollars',
    frequency: 'Weekly (Wednesday)',
    keywords: 'balance sheet qt qe soma h41 reserves securities',
  },
  {
    id: 'H6/M1_N.M',
    title: 'M1 money stock',
    units: 'Billions of dollars',
    frequency: 'Monthly',
    keywords: 'money supply m1 monetary aggregate h6',
  },
  {
    id: 'H6/M2_N.M',
    title: 'M2 money stock',
    units: 'Billions of dollars',
    frequency: 'Monthly',
    keywords: 'money supply m2 monetary aggregate h6',
  },
  {
    id: 'G17/IP.B50001.S',
    title: 'Industrial production, total index (seasonally adjusted)',
    units: 'Index 2017=100',
    frequency: 'Monthly',
    keywords: 'industrial production output manufacturing g17',
  },
  {
    id: 'G17/CAPUTL.B50001.S',
    title: 'Capacity utilisation, total industry (seasonally adjusted)',
    units: 'Percent of capacity',
    frequency: 'Monthly',
    keywords: 'capacity utilisation utilization slack g17',
  },
  {
    id: 'H10/RXI$US_N.B.EU',
    title: 'US dollar / euro spot exchange rate',
    units: 'USD per EUR',
    frequency: 'Business daily',
    keywords: 'fx forex eurusd dollar h10',
  },
  {
    id: 'H10/JRXWTFB_N.B',
    title: 'Nominal broad US dollar index',
    units: 'Index Jan 2006=100',
    frequency: 'Business daily',
    keywords: 'dollar index dxy broad trade weighted h10',
  },
];

export function search(query: string, limit: number): DataSearchResult[] {
  return searchCatalogue('fed', CATALOGUE, query, limit);
}

/** Releases and their names, for the `SRC` board and error hints. */
export function listReleases(): { id: string; name: string }[] {
  return Object.entries(RELEASES).map(([id, name]) => ({ id, name }));
}
