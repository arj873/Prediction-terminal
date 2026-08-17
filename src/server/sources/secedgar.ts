/**
 * SEC EDGAR — filings and the XBRL facts inside them.
 *
 * Two surfaces, and they answer different questions:
 *
 *  - **Filings** (`/submissions/CIK….json`) is the wire. An 8-K appears here
 *    within seconds of acceptance, which is often before the press release, and
 *    is what a market on a merger, a bankruptcy or an earnings date settles on.
 *  - **Company concept** (`/api/xbrl/companyconcept/…`) is the history. Every
 *    value a company has ever reported for one tag — `Revenues`,
 *    `EarningsPerShareDiluted` — with the filing that reported it. Restatements
 *    appear as additional values for the same period rather than as edits, so
 *    "what did they say, and when did they change it" is answerable.
 *
 * EDGAR requires a declaring User-Agent and blocks callers without one. The
 * default names this project; an operator running it publicly should set
 * `SEC_USER_AGENT` to their own contact, which is what the SEC's fair-access
 * policy actually asks for.
 */

import type {
  SecCompany,
  SecConceptResponse,
  SecFact,
  SecFiling,
  SecFilingsResponse,
  SecSearchResult,
} from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';

const DATA_BASE = process.env.SEC_DATA_BASE ?? 'https://data.sec.gov';
const WWW_BASE = process.env.SEC_WWW_BASE ?? 'https://www.sec.gov';

/**
 * EDGAR's fair-access policy wants `Company Name contact@domain`, and it means
 * it: a User-Agent carrying parentheses or a semicolon — the ordinary shape of
 * a browser UA — is answered with 403 rather than with an explanation. So the
 * default is the two-token form the policy asks for, and an operator running
 * this publicly should replace the address with their own.
 */
const USER_AGENT =
  process.env.SEC_USER_AGENT?.trim() || 'prediction-terminal contact@example.com';

function headers(): Record<string, string> {
  return { 'User-Agent': USER_AGENT };
}

/** EDGAR keys everything on a zero-padded ten-digit CIK. */
export function padCik(cik: string | number): string {
  return String(cik).replace(/\D/g, '').padStart(10, '0');
}

/* -------------------------------------------------------------- ticker map */

interface TickerRow {
  cik_str?: number;
  ticker?: string;
  title?: string;
}

/**
 * The ticker → CIK map, fetched once per session.
 *
 * It is the only way to go from a symbol to a CIK — EDGAR has no lookup
 * endpoint — and at roughly 10,000 rows it is small enough to hold. Keyed by
 * ticker *and* by CIK so `SEC AAPL` and `SEC 320193` both resolve.
 */
async function tickerMap(): Promise<Map<string, { cik: string; name: string; ticker: string }>> {
  const key = 'sec:tickers';
  const hit = cache.get<Map<string, { cik: string; name: string; ticker: string }>>(key);
  if (hit) return hit;

  const raw = await fetchJson<Record<string, TickerRow>>(`${WWW_BASE}/files/company_tickers.json`, {
    timeoutMs: 40_000,
    retries: 1,
    headers: headers(),
  });

  const map = new Map<string, { cik: string; name: string; ticker: string }>();
  for (const row of Object.values(raw)) {
    if (!row.ticker || row.cik_str === undefined) continue;
    const entry = { cik: padCik(row.cik_str), name: row.title ?? row.ticker, ticker: row.ticker };
    // A company with several share classes appears more than once; the first
    // listing is its primary one, so it wins.
    if (!map.has(entry.ticker)) map.set(entry.ticker, entry);
    if (!map.has(entry.cik)) map.set(entry.cik, entry);
  }

  cache.set(key, map, TTL.catalogue);
  return map;
}

/** Resolve a ticker, a CIK or a company name to a CIK. */
export async function resolveCik(query: string): Promise<string> {
  const trimmed = query.trim();
  if (!trimmed) throw new UpstreamError('Missing company', { code: 'bad_request' });

  const map = await tickerMap();

  const direct = map.get(trimmed.toUpperCase());
  if (direct) return direct.cik;

  // A bare number is a CIK whether or not it is in the ticker file — plenty of
  // filers (funds, individuals filing Form 4) have a CIK and no ticker at all.
  if (/^\d{1,10}$/.test(trimmed)) return padCik(trimmed);

  const words = trimmed.toLowerCase().split(/\s+/).filter(Boolean);
  for (const entry of map.values()) {
    const name = entry.name.toLowerCase();
    if (words.every((word) => name.includes(word))) return entry.cik;
  }

  throw new UpstreamError(`EDGAR has no filer matching "${query}"`, {
    code: 'not_found',
    hint: 'Try a ticker (AAPL), a CIK (320193), or a distinctive word from the name.',
  });
}

/** Companies whose ticker or name matches, for the search panel. */
export async function search(query: string, limit = 25): Promise<SecSearchResult[]> {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return [];

  const map = await tickerMap();
  const seen = new Set<string>();
  const results: SecSearchResult[] = [];

  for (const entry of map.values()) {
    if (seen.has(entry.cik)) continue;
    const haystack = `${entry.ticker} ${entry.name}`.toLowerCase();
    if (!words.every((word) => haystack.includes(word))) continue;
    seen.add(entry.cik);
    results.push({ cik: entry.cik, ticker: entry.ticker, name: entry.name });
    if (results.length >= limit) break;
  }

  return results;
}

/* -------------------------------------------------------------- filings */

interface SubmissionsFilings {
  accessionNumber?: string[];
  filingDate?: string[];
  reportDate?: string[];
  form?: string[];
  items?: string[];
  size?: number[];
  primaryDocument?: string[];
  primaryDocDescription?: string[];
}

interface Submissions {
  cik?: string;
  name?: string;
  tickers?: string[];
  exchanges?: string[];
  sic?: string;
  sicDescription?: string;
  fiscalYearEnd?: string;
  filings?: { recent?: SubmissionsFilings };
}

/**
 * EDGAR ships `filings.recent` as parallel arrays — one per field, aligned by
 * index — rather than as a list of objects. Zipping them here means nothing
 * downstream has to know that, and an array that is short (which happens when a
 * field does not apply to every form) yields an empty string rather than
 * shifting every subsequent filing's data by one.
 */
export function zipFilings(
  filings: SubmissionsFilings,
  cik: string,
  limit: number,
  form?: string,
): SecFiling[] {
  const accessions = filings.accessionNumber ?? [];
  const wanted = form?.trim().toUpperCase();
  const out: SecFiling[] = [];

  for (let i = 0; i < accessions.length && out.length < limit; i++) {
    const formType = filings.form?.[i] ?? '';
    if (wanted && formType.toUpperCase() !== wanted) continue;

    const accession = accessions[i] ?? '';
    const bare = accession.replace(/-/g, '');
    const document = filings.primaryDocument?.[i] ?? '';

    out.push({
      accession,
      form: formType,
      filed: filings.filingDate?.[i] ?? '',
      reportDate: filings.reportDate?.[i] ?? '',
      items: filings.items?.[i] ?? '',
      primaryDocument: document,
      description: filings.primaryDocDescription?.[i] ?? '',
      size: filings.size?.[i] ?? null,
      url: document
        ? `${WWW_BASE}/Archives/edgar/data/${Number(cik)}/${bare}/${document}`
        : `${WWW_BASE}/Archives/edgar/data/${Number(cik)}/${bare}/`,
    });
  }

  return out;
}

function companyOf(raw: Submissions, cik: string): SecCompany {
  return {
    cik,
    name: raw.name ?? cik,
    tickers: raw.tickers ?? [],
    exchanges: raw.exchanges ?? [],
    sic: raw.sic ?? '',
    sicDescription: raw.sicDescription ?? '',
    fiscalYearEnd: raw.fiscalYearEnd ?? '',
    url: `${WWW_BASE}/cgi-bin/browse-edgar?action=getcompany&CIK=${cik}&type=&dateb=&owner=include&count=40`,
  };
}

async function submissions(cik: string): Promise<Submissions> {
  return cache.cached(`sec:submissions:${cik}`, TTL.edgar, () =>
    fetchJson<Submissions>(`${DATA_BASE}/submissions/CIK${cik}.json`, {
      timeoutMs: 30_000,
      retries: 1,
      headers: headers(),
    }),
  );
}

export async function getFilings(
  query: string,
  options: { limit?: number; form?: string } = {},
): Promise<SecFilingsResponse> {
  const cik = await resolveCik(query);
  const raw = await submissions(cik);
  const recent = raw.filings?.recent ?? {};

  const limit = Math.min(Math.max(options.limit ?? 40, 1), 250);
  const filings = zipFilings(recent, cik, limit, options.form);

  if (filings.length === 0 && options.form) {
    throw new UpstreamError(`${raw.name ?? cik} has filed no recent ${options.form}`, {
      code: 'not_found',
      hint:
        `EDGAR's recent-filings document covers the last ~1,000 filings. Forms present ` +
        `are ${[...new Set(recent.form ?? [])].slice(0, 10).join(', ')}.`,
    });
  }

  return {
    company: companyOf(raw, cik),
    filings,
    forms: [...new Set(recent.form ?? [])].sort(),
  };
}

/* ------------------------------------------------------------ xbrl facts */

interface ConceptUnit {
  start?: string;
  end?: string;
  val?: number;
  accn?: string;
  fy?: number;
  fp?: string;
  form?: string;
  filed?: string;
}

interface ConceptResponse {
  cik?: number;
  taxonomy?: string;
  tag?: string;
  label?: string;
  description?: string;
  entityName?: string;
  units?: Record<string, ConceptUnit[]>;
}

/** XBRL tags are CamelCase identifiers; anything else is a typo, not a tag. */
const TAG = /^[A-Za-z][A-Za-z0-9_-]{1,120}$/;

/**
 * Every value a company has reported for one XBRL tag.
 *
 * The unit is chosen rather than assumed: a concept can be reported in `USD`,
 * `shares` and `USD/shares` at once, and picking the first key in the object
 * would depend on JSON ordering. The richest series wins, which is what someone
 * asking for `Revenues` means.
 */
export async function getConcept(
  query: string,
  rawTag: string,
  taxonomy = 'us-gaap',
): Promise<SecConceptResponse> {
  const tag = rawTag.trim();
  if (!TAG.test(tag)) {
    throw new UpstreamError(`"${rawTag}" is not an XBRL tag`, {
      code: 'bad_request',
      hint: 'Tags are CamelCase, e.g. Revenues, NetIncomeLoss, Assets, EarningsPerShareDiluted.',
    });
  }

  const cik = await resolveCik(query);
  const [raw, company] = await Promise.all([
    cache.cached(`sec:concept:${cik}:${taxonomy}:${tag}`, TTL.edgarFacts, () =>
      fetchJson<ConceptResponse>(
        `${DATA_BASE}/api/xbrl/companyconcept/CIK${cik}/${encodeURIComponent(taxonomy)}/${encodeURIComponent(tag)}.json`,
        { timeoutMs: 30_000, retries: 1, headers: headers() },
      ).catch((err: unknown) => {
        if (err instanceof UpstreamError && err.code === 'not_found') {
          throw new UpstreamError(`This filer has never reported ${taxonomy}:${tag}`, {
            code: 'not_found',
            hint:
              'Not every issuer tags every concept. Common ones: Revenues, ' +
              'RevenueFromContractWithCustomerExcludingAssessedTax, NetIncomeLoss, Assets, ' +
              'Liabilities, StockholdersEquity, EarningsPerShareDiluted.',
          });
        }
        throw err;
      }),
    ),
    submissions(cik).catch(() => ({}) as Submissions),
  ]);

  const units = raw.units ?? {};
  const unit =
    Object.entries(units).sort(([, a], [, b]) => b.length - a.length)[0] ?? ['', [] as ConceptUnit[]];

  const facts: SecFact[] = unit[1]
    .filter((f): f is ConceptUnit & { val: number } => typeof f.val === 'number' && Boolean(f.end))
    .map((f) => ({
      end: f.end!,
      start: f.start ?? '',
      value: f.val,
      fiscalYear: f.fy ?? null,
      fiscalPeriod: f.fp ?? '',
      form: f.form ?? '',
      filed: f.filed ?? '',
      accession: f.accn ?? '',
      unit: unit[0],
    }))
    .sort((a, b) => a.end.localeCompare(b.end) || a.filed.localeCompare(b.filed));

  return {
    company: companyOf(company, cik),
    taxonomy: raw.taxonomy ?? taxonomy,
    tag: raw.tag ?? tag,
    label: raw.label ?? tag,
    description: (raw.description ?? '').slice(0, 2000),
    unit: unit[0],
    facts,
  };
}
