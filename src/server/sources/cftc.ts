/**
 * CFTC — Commitments of Traders, from the Commission's public reporting portal.
 *
 * Every Friday the CFTC publishes who is long and who is short each US futures
 * market, broken out by what kind of trader they are. It is the only public,
 * mandatory census of positioning that exists, and it is the reason a "will gold
 * be above X" market and a "will speculators capitulate" thesis can be checked
 * against each other rather than argued about.
 *
 * Three reports cover different market families, and they name their trader
 * categories differently because the categories genuinely differ:
 *
 *  - **legacy** — commercial vs non-commercial. Every market, back to 1986.
 *  - **disaggregated** — producers, swap dealers, managed money, other
 *    reportables. Physical commodities, since 2006.
 *  - **financial** (TFF) — dealers, asset managers, leveraged funds. Rates,
 *    equities and currencies, since 2010.
 *
 * The portal is a Socrata instance, so the whole history is queryable and a
 * position series is a real chart rather than a single week's snapshot.
 */

import type {
  CotCategory,
  CotMarket,
  CotReport,
  DataObservation,
  DataSearchResult,
  DataSeriesResponse,
} from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';

const BASE = process.env.CFTC_API_BASE ?? 'https://publicreporting.cftc.gov/resource';

/** One trader category, as the fields that describe it in a report's rows. */
interface CategorySpec {
  /** Short key used in a series id, e.g. `noncomm`. */
  key: string;
  name: string;
  long: string;
  short: string;
  spread?: string;
  changeLong?: string;
  changeShort?: string;
  pctLong?: string;
  pctShort?: string;
  traders?: string;
}

interface ReportSpec {
  id: string;
  label: string;
  /** Socrata dataset identifier. */
  dataset: string;
  covers: string;
  categories: readonly CategorySpec[];
}

const REPORTS: readonly ReportSpec[] = [
  {
    id: 'legacy',
    label: 'Legacy — commercial vs non-commercial',
    dataset: '6dca-aqww',
    covers: 'every futures market, weekly since 1986',
    categories: [
      {
        key: 'noncomm',
        name: 'Non-commercial',
        long: 'noncomm_positions_long_all',
        short: 'noncomm_positions_short_all',
        // The Commission's own field name carries this typo. Correcting it here
        // would simply stop matching the column.
        spread: 'noncomm_postions_spread_all',
        changeLong: 'change_in_noncomm_long_all',
        changeShort: 'change_in_noncomm_short_all',
        pctLong: 'pct_of_oi_noncomm_long_all',
        pctShort: 'pct_of_oi_noncomm_short_all',
        traders: 'traders_noncomm_long_all',
      },
      {
        key: 'comm',
        name: 'Commercial',
        long: 'comm_positions_long_all',
        short: 'comm_positions_short_all',
        changeLong: 'change_in_comm_long_all',
        changeShort: 'change_in_comm_short_all',
        pctLong: 'pct_of_oi_comm_long_all',
        pctShort: 'pct_of_oi_comm_short_all',
        traders: 'traders_comm_long_all',
      },
      {
        key: 'nonrept',
        name: 'Non-reportable',
        long: 'nonrept_positions_long_all',
        short: 'nonrept_positions_short_all',
        changeLong: 'change_in_nonrept_long_all',
        changeShort: 'change_in_nonrept_short_all',
        pctLong: 'pct_of_oi_nonrept_long_all',
        pctShort: 'pct_of_oi_nonrept_short_all',
      },
    ],
  },
  {
    id: 'disaggregated',
    label: 'Disaggregated — producers, swaps, managed money',
    dataset: '72hh-3qpy',
    covers: 'physical commodities, weekly since 2006',
    categories: [
      {
        key: 'prod',
        name: 'Producer / merchant / processor / user',
        long: 'prod_merc_positions_long',
        short: 'prod_merc_positions_short',
        changeLong: 'change_in_prod_merc_long',
        changeShort: 'change_in_prod_merc_short',
      },
      {
        key: 'swap',
        name: 'Swap dealer',
        long: 'swap_positions_long_all',
        short: 'swap__positions_short_all',
        spread: 'swap__positions_spread_all',
        changeLong: 'change_in_swap_long_all',
        changeShort: 'change_in_swap_short_all',
      },
      {
        key: 'mmoney',
        name: 'Managed money',
        long: 'm_money_positions_long_all',
        short: 'm_money_positions_short_all',
        spread: 'm_money_positions_spread',
        changeLong: 'change_in_m_money_long_all',
        changeShort: 'change_in_m_money_short_all',
      },
      {
        key: 'other',
        name: 'Other reportable',
        long: 'other_rept_positions_long',
        short: 'other_rept_positions_short',
        spread: 'other_rept_positions_spread',
        changeLong: 'change_in_other_rept_long',
        changeShort: 'change_in_other_rept_short',
      },
      {
        key: 'nonrept',
        name: 'Non-reportable',
        long: 'nonrept_positions_long_all',
        short: 'nonrept_positions_short_all',
        changeLong: 'change_in_nonrept_long_all',
        changeShort: 'change_in_nonrept_short_all',
      },
    ],
  },
  {
    id: 'financial',
    label: 'Traders in Financial Futures — dealers, asset managers, leveraged funds',
    dataset: 'gpe5-46if',
    covers: 'rates, equity indices and currencies, weekly since 2010',
    categories: [
      {
        key: 'dealer',
        name: 'Dealer / intermediary',
        long: 'dealer_positions_long_all',
        short: 'dealer_positions_short_all',
        spread: 'dealer_positions_spread_all',
        changeLong: 'change_in_dealer_long_all',
        changeShort: 'change_in_dealer_short_all',
        pctLong: 'pct_of_oi_dealer_long_all',
        pctShort: 'pct_of_oi_dealer_short_all',
      },
      {
        key: 'assetmgr',
        name: 'Asset manager / institutional',
        long: 'asset_mgr_positions_long',
        short: 'asset_mgr_positions_short',
        spread: 'asset_mgr_positions_spread',
        changeLong: 'change_in_asset_mgr_long',
        changeShort: 'change_in_asset_mgr_short',
        pctLong: 'pct_of_oi_asset_mgr_long',
        pctShort: 'pct_of_oi_asset_mgr_short',
      },
      {
        key: 'levfund',
        name: 'Leveraged funds',
        long: 'lev_money_positions_long',
        short: 'lev_money_positions_short',
        spread: 'lev_money_positions_spread',
        changeLong: 'change_in_lev_money_long',
        changeShort: 'change_in_lev_money_short',
        pctLong: 'pct_of_oi_lev_money_long',
        pctShort: 'pct_of_oi_lev_money_short',
      },
      {
        key: 'other',
        name: 'Other reportable',
        long: 'other_rept_positions_long',
        short: 'other_rept_positions_short',
        spread: 'other_rept_positions_spread',
        changeLong: 'change_in_other_rept_long',
        changeShort: 'change_in_other_rept_short',
      },
      {
        key: 'nonrept',
        name: 'Non-reportable',
        long: 'nonrept_positions_long_all',
        short: 'nonrept_positions_short_all',
        changeLong: 'change_in_nonrept_long_all',
        changeShort: 'change_in_nonrept_short_all',
      },
    ],
  },
];

const DEFAULT_REPORT = 'legacy';

export function reportSpec(id: string): ReportSpec {
  const key = id.trim().toLowerCase();
  const found = REPORTS.find(
    (r) => r.id === key || (key === 'tff' && r.id === 'financial') || (key === 'disagg' && r.id === 'disaggregated'),
  );
  if (found) return found;
  throw new UpstreamError(`"${id}" is not a Commitments of Traders report`, {
    code: 'bad_request',
    hint: `Reports are ${REPORTS.map((r) => r.id).join(', ')}.`,
  });
}

export function listReports(): { id: string; label: string; covers: string }[] {
  return REPORTS.map((r) => ({ id: r.id, label: r.label, covers: r.covers }));
}

/* ------------------------------------------------------------------ rows */

type Row = Record<string, string | undefined>;

/** Socrata sends every column as a string, including the numbers. */
function num(row: Row, field: string | undefined): number | null {
  if (!field) return null;
  const raw = row[field];
  if (raw === undefined || raw === '') return null;
  const value = Number(raw);
  return Number.isFinite(value) ? value : null;
}

async function query(dataset: string, params: Record<string, string>, ttlMs: number): Promise<Row[]> {
  const url = new URL(`${BASE}/${dataset}.json`);
  for (const [key, value] of Object.entries(params)) url.searchParams.set(key, value);

  return cache.cached(`cftc:${url.search}`, ttlMs, () =>
    fetchJson<Row[]>(url.toString(), { timeoutMs: 30_000, retries: 1 }),
  );
}

/** Socrata string literals are single-quoted, and a quote inside one is doubled. */
function sqlLiteral(value: string): string {
  return `'${value.replace(/'/g, "''")}'`;
}

/* --------------------------------------------------------------- markets */

/**
 * Markets whose name contains every word of `query`.
 *
 * Matching on all words rather than the whole phrase is what makes `COT gold
 * comex` work as well as `COT gold`: the CFTC writes the name as
 * `GOLD - COMMODITY EXCHANGE INC.`, which contains neither phrase verbatim.
 */
export async function findMarkets(
  search: string,
  report = DEFAULT_REPORT,
  limit = 40,
): Promise<{ query: string; report: string; markets: CotMarket[] }> {
  const spec = reportSpec(report);
  const words = search.trim().split(/\s+/).filter(Boolean);

  const where = words
    .map((word) => `upper(market_and_exchange_names) like ${sqlLiteral(`%${word.toUpperCase()}%`)}`)
    .join(' AND ');

  const rows = await query(
    spec.dataset,
    {
      $select:
        'market_and_exchange_names,cftc_contract_market_code,' +
        'max(report_date_as_yyyy_mm_dd) as latest,max(open_interest_all) as oi',
      $group: 'market_and_exchange_names,cftc_contract_market_code',
      $order: 'latest DESC',
      $limit: String(Math.min(Math.max(limit, 1), 200)),
      ...(where ? { $where: where } : {}),
    },
    TTL.catalogue,
  );

  return {
    query: search,
    report: spec.id,
    markets: rows.map((row) => {
      const full = row['market_and_exchange_names'] ?? '';
      const dash = full.indexOf(' - ');
      return {
        contractCode: (row['cftc_contract_market_code'] ?? '').trim(),
        market: dash === -1 ? full : full.slice(0, dash).trim(),
        exchange: dash === -1 ? '' : full.slice(dash + 3).trim(),
        latest: (row['latest'] ?? '').slice(0, 10),
        openInterest: num(row, 'oi'),
      };
    }),
  };
}

/**
 * Resolve what a reader typed to one market.
 *
 * A contract market code is unambiguous and is taken as-is; anything else is a
 * name search whose best hit is the most recently reported market that matched.
 */
async function resolveMarket(spec: ReportSpec, market: string): Promise<string> {
  const trimmed = market.trim();

  if (/^\d{3,6}$/.test(trimmed)) {
    const rows = await query(
      spec.dataset,
      {
        $select: 'market_and_exchange_names',
        $where: `cftc_contract_market_code=${sqlLiteral(trimmed)}`,
        $order: 'report_date_as_yyyy_mm_dd DESC',
        $limit: '1',
      },
      TTL.catalogue,
    );
    const name = rows[0]?.['market_and_exchange_names'];
    if (name) return name;
  }

  const { markets } = await findMarkets(trimmed, spec.id, 1);
  const best = markets[0];
  if (!best) {
    throw new UpstreamError(`No ${spec.id} COT market matches "${market}"`, {
      code: 'not_found',
      hint: `Try a shorter name — \`COT gold\`, \`COT e-mini s&p\`, \`COT crude\` — or another report (${REPORTS.map((r) => r.id).join(', ')}).`,
    });
  }
  return best.exchange ? `${best.market} - ${best.exchange}` : best.market;
}

/* ---------------------------------------------------------------- report */

/** The latest published report for a market, with every category broken out. */
export async function getReport(market: string, report = DEFAULT_REPORT): Promise<CotReport> {
  const spec = reportSpec(report);
  const name = await resolveMarket(spec, market);

  const rows = await query(
    spec.dataset,
    {
      $where: `market_and_exchange_names=${sqlLiteral(name)}`,
      $order: 'report_date_as_yyyy_mm_dd DESC',
      $limit: '1',
    },
    TTL.cot,
  );

  const row = rows[0];
  if (!row) {
    throw new UpstreamError(`The CFTC has published no ${spec.id} report for ${name}`, {
      code: 'not_found',
    });
  }

  const dash = name.indexOf(' - ');
  const categories: CotCategory[] = spec.categories.map((category) => {
    const long = num(row, category.long);
    const short = num(row, category.short);
    const changeLong = num(row, category.changeLong);
    const changeShort = num(row, category.changeShort);

    return {
      name: category.name,
      long,
      short,
      spreading: num(row, category.spread),
      net: long !== null && short !== null ? long - short : null,
      netChange:
        changeLong !== null && changeShort !== null ? changeLong - changeShort : null,
      percentLong: num(row, category.pctLong),
      percentShort: num(row, category.pctShort),
      traderCount: num(row, category.traders),
    };
  });

  return {
    report: spec.id,
    reportLabel: spec.label,
    market: dash === -1 ? name : name.slice(0, dash).trim(),
    exchange: dash === -1 ? '' : name.slice(dash + 3).trim(),
    contractCode: (row['cftc_contract_market_code'] ?? '').trim(),
    date: (row['report_date_as_yyyy_mm_dd'] ?? '').slice(0, 10),
    openInterest: num(row, 'open_interest_all'),
    openInterestChange: num(row, 'change_in_open_interest_all'),
    categories,
    sourceUrl: 'https://www.cftc.gov/MarketReports/CommitmentsofTraders/index.htm',
  };
}

/* ---------------------------------------------------------------- series */

/** `noncomm_net` → the category and which figure of it. */
function splitField(field: string): { category: string; measure: 'net' | 'long' | 'short' } {
  const at = field.lastIndexOf('_');
  const measure = at === -1 ? '' : field.slice(at + 1).toLowerCase();
  if (measure === 'net' || measure === 'long' || measure === 'short') {
    return { category: field.slice(0, at).toLowerCase(), measure };
  }
  return { category: field.toLowerCase(), measure: 'net' };
}

/**
 * Positioning over time: `report/market/field`.
 *
 * `legacy/GOLD/noncomm_net` is the classic speculative-positioning series. The
 * open interest itself is `report/market/oi`.
 */
export async function getSeries(
  rawId: string,
  start?: string,
  end?: string,
): Promise<DataSeriesResponse> {
  const parts = rawId.split('/').map((p) => p.trim()).filter(Boolean);

  if (parts.length < 2) {
    throw new UpstreamError(`"${rawId}" does not name a CFTC series`, {
      code: 'bad_request',
      hint:
        'Ids are report/market/field, e.g. legacy/GOLD/noncomm_net, ' +
        'financial/E-MINI S&P 500/levfund_net, or legacy/CRUDE OIL/oi.',
    });
  }

  const spec = reportSpec(parts[0]!);
  const field = (parts.at(-1) ?? '').toLowerCase();
  const market = parts.slice(1, -1).join('/');
  const name = await resolveMarket(spec, market);

  const isOpenInterest = field === 'oi' || field === 'open_interest';
  const { category, measure } = splitField(field);
  const spec_ = isOpenInterest
    ? undefined
    : spec.categories.find((c) => c.key === category || c.name.toLowerCase().startsWith(category));

  if (!isOpenInterest && !spec_) {
    throw new UpstreamError(`The ${spec.id} report has no "${category}" category`, {
      code: 'bad_request',
      hint:
        `Categories are ${spec.categories.map((c) => c.key).join(', ')}, each with ` +
        `_net, _long or _short — plus \`oi\` for open interest.`,
    });
  }

  const where = [`market_and_exchange_names=${sqlLiteral(name)}`];
  if (start) where.push(`report_date_as_yyyy_mm_dd >= ${sqlLiteral(`${start}T00:00:00`)}`);
  if (end) where.push(`report_date_as_yyyy_mm_dd <= ${sqlLiteral(`${end}T23:59:59`)}`);

  const select = isOpenInterest
    ? ['report_date_as_yyyy_mm_dd', 'open_interest_all']
    : ['report_date_as_yyyy_mm_dd', spec_!.long, spec_!.short];

  const rows = await query(
    spec.dataset,
    {
      $select: select.join(','),
      $where: where.join(' AND '),
      $order: 'report_date_as_yyyy_mm_dd ASC',
      // Every market's full weekly history since 1986 is under 2,100 rows.
      $limit: '5000',
    },
    TTL.cot,
  );

  const observations: DataObservation[] = rows.map((row) => {
    const date = (row['report_date_as_yyyy_mm_dd'] ?? '').slice(0, 10);
    if (isOpenInterest) return { date, value: num(row, 'open_interest_all') };

    const long = num(row, spec_!.long);
    const short = num(row, spec_!.short);
    const value =
      measure === 'long' ? long : measure === 'short' ? short
      : long !== null && short !== null ? long - short : null;
    return { date, value };
  });

  if (observations.length === 0) {
    throw new UpstreamError(`The CFTC has no ${spec.id} history for ${name}`, {
      code: 'not_found',
    });
  }

  const label = isOpenInterest
    ? 'open interest'
    : `${spec_!.name} ${measure === 'net' ? 'net position' : measure}`;

  return {
    series: {
      provider: 'cftc',
      id: rawId,
      title: `${name} — ${label}`,
      units: 'Contracts',
      unitsShort: 'Contracts',
      frequency: 'Weekly (Tuesday)',
      seasonalAdjustment: '',
      lastUpdated: observations.at(-1)?.date ?? '',
      observationStart: observations[0]?.date ?? '',
      observationEnd: observations.at(-1)?.date ?? '',
      notes: `${spec.label}. Positions are as of the report Tuesday and published the following Friday.`,
      source: spec.dataset,
      sourceUrl: 'https://www.cftc.gov/MarketReports/CommitmentsofTraders/index.htm',
    },
    observations,
  };
}

/**
 * Search: the positioning series people actually chart, per matching market.
 *
 * Unlike the other providers this one *is* a live search — the portal indexes
 * every market name — so the catalogue is generated from the reader's own query
 * rather than curated in advance.
 */
export async function search(rawQuery: string, limit: number): Promise<DataSearchResult[]> {
  const { markets } = await findMarkets(rawQuery, DEFAULT_REPORT, Math.ceil(limit / 2));

  return markets.slice(0, Math.ceil(limit / 2)).flatMap((market) => [
    {
      provider: 'cftc',
      id: `legacy/${market.contractCode}/noncomm_net`,
      title: `${market.market} (${market.exchange}) — non-commercial net position`,
      units: 'Contracts',
      frequency: 'Weekly',
    },
    {
      provider: 'cftc',
      id: `legacy/${market.contractCode}/oi`,
      title: `${market.market} (${market.exchange}) — open interest`,
      units: 'Contracts',
      frequency: 'Weekly',
    },
  ]);
}
