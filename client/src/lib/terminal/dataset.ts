/**
 * The reference data sources the terminal reads, and how a series at one of
 * them is named.
 *
 * A prediction market settles against a number somebody else publishes. Twelve
 * publishers are wired in here, and they agree on nothing: the BLS names a
 * series `LNS14000000`, the ECB names one `EXR/D.USD.EUR.SP00.A`, the Federal
 * Reserve names one `H15/RIFLGFCY10_N.B`, FRED names one `UNRATE`. Rather than
 * twelve command sets, every data command takes one *reference* — an optional
 * source prefix and an identifier — and this module is the only place that
 * knows how to read one.
 *
 *   UNRATE                       → FRED (the default, so nothing changes)
 *   bls:LNS14000000              → Bureau of Labor Statistics
 *   ecb:EXR/D.USD.EUR.SP00.A     → European Central Bank
 *   fed:H15/RIFLGFCY10_N.B       → Federal Reserve
 *
 * This is `venue.ts` applied to publishers instead of brokers, and for the same
 * reason: one table drives the parser, the help text, the search filter and the
 * client, so a source added to one and not the others is a compile error rather
 * than a runtime throw.
 *
 * Case is part of the identifier, not decoration. FRED and the BLS shout their
 * ids; the SDMX agencies mix case *inside* a key (`EXR/D.USD.EUR.SP00.A`) and
 * folding either way 404s it, which is what `keep` is for.
 *
 * Like `venue.ts` this module is deliberately one of two halves —
 * `crates/core/src/dataset.rs` is the other — and the identifiers come from the
 * generated bindings rather than from the table below, so a publisher added to
 * one half and not the other is a compile error here. Behaviour is pinned by
 * `contract/source-cases.json`, a parity fixture both halves' tests read.
 */

import type { DataSource, DataSourceKind } from '$gen';

/**
 * Re-exported so a caller that reads a reference does not also have to know
 * which generated file the source identifiers live in.
 */
export type { DataSource, DataSourceKind };

/** How a deployment supplies a source's credential, when it needs one. */
export interface DataSourceKey {
  /** Environment variable read for the credential. */
  env: string;
  /** Where a reader gets one. */
  signup: string;
  /**
   * What the source still does without it. Empty means nothing at all — the
   * difference between a degraded feed and an absent one, which the terminal
   * has to state rather than let a reader discover through an error.
   */
  withoutKey: string;
}

/** The table's own row shape. `id` is the generated `DataSource`, so the table cannot name a publisher the server does not have. */
interface DataSourceDefinition {
  id: DataSource;
  /** Full name, for panel headers and error messages. */
  label: string;
  /** Short badge for a table column. */
  code: string;
  /** Canonical command-line prefix, including the colon. */
  prefix: string;
  kind: DataSourceKind;
  /** What this publisher calls an identifier, for usage text. */
  idLabel: string;
  /** A real identifier, shown when a reader gets the shape wrong. */
  idExample: string;
  /**
   * Identifier case. `keep` is not indecision: an SDMX key carries meaningful
   * case inside it and folding either way 404s the request.
   */
  case: 'upper' | 'lower' | 'keep';
  /** Public documentation, so a panel can link out. */
  site: string;
  /** One line on what this publisher actually covers. */
  covers: string;
  /**
   * Names a reader might type. Deliberately generous — the cost of accepting a
   * synonym is nil and the cost of rejecting one is a command retyped.
   */
  aliases: readonly string[];
  /** An unprefixed reference belongs to this source. Exactly one sets it. */
  isDefault?: boolean;
  /** This source's references print without their prefix. Follows from being the default. */
  bareRef?: boolean;
  /** The credential this source needs, when it needs one. */
  key?: DataSourceKey;
}

const SOURCE_TABLE = [
  {
    id: 'fred',
    label: 'FRED (St. Louis Fed)',
    code: 'FRED',
    prefix: 'fred:',
    kind: 'series',
    idLabel: 'series id',
    idExample: 'UNRATE',
    case: 'upper',
    site: 'https://fred.stlouisfed.org',
    covers: '800,000+ US and international series, aggregated from 100+ publishers',
    aliases: ['fred', 'stlouisfed', 'stl'],
    // Every `FRED <id>` in this terminal's history, help and README predates the
    // other eleven sources; an unprefixed reference has to keep meaning what it
    // always did.
    isDefault: true,
    bareRef: true,
    key: {
      env: 'FRED_API_KEY',
      signup: 'https://fred.stlouisfed.org/docs/api/api_key.html',
      withoutKey: 'the fred.stlouisfed.org scrape, which datacentre IPs are often refused',
    },
  },
  {
    id: 'bls',
    label: 'Bureau of Labor Statistics',
    code: 'BLS',
    prefix: 'bls:',
    kind: 'series',
    idLabel: 'series id',
    idExample: 'LNS14000000',
    case: 'upper',
    site: 'https://www.bls.gov/developers/',
    covers:
      'CPI, unemployment, payrolls, earnings, productivity — the US labour statistics of record',
    aliases: ['bls', 'labor', 'labour', 'bureauoflaborstatistics'],
    key: {
      env: 'BLS_API_KEY',
      signup: 'https://data.bls.gov/registrationEngine/',
      withoutKey: 'the v1 API: 25 queries a day and 10 years of history per call',
    },
  },
  {
    id: 'ecb',
    label: 'European Central Bank',
    code: 'ECB',
    prefix: 'ecb:',
    kind: 'series',
    idLabel: 'SDMX key',
    idExample: 'EXR/D.USD.EUR.SP00.A',
    case: 'keep',
    site: 'https://data.ecb.europa.eu',
    covers: 'euro area rates, exchange rates, HICP inflation, monetary aggregates',
    aliases: ['ecb', 'europeancentralbank', 'eurozone'],
  },
  {
    id: 'imf',
    label: 'International Monetary Fund',
    code: 'IMF',
    prefix: 'imf:',
    kind: 'series',
    idLabel: 'SDMX key',
    idExample: 'CPI/US.CPI._Z._Z.M',
    case: 'keep',
    site: 'https://data.imf.org',
    covers: 'cross-country CPI, balance of payments, reserves, government finance',
    aliases: ['imf', 'fund'],
  },
  {
    id: 'oecd',
    label: 'OECD',
    code: 'OECD',
    prefix: 'oecd:',
    kind: 'series',
    idLabel: 'SDMX key',
    idExample: 'DSD_KEI@DF_KEI/USA.M.PRVM.IX...',
    case: 'keep',
    site: 'https://data-explorer.oecd.org',
    covers: 'member-country growth, inflation, unemployment and leading indicators',
    aliases: ['oecd'],
  },
  {
    id: 'fed',
    label: 'Federal Reserve Board',
    code: 'FED',
    prefix: 'fed:',
    kind: 'series',
    idLabel: 'release/series',
    idExample: 'H15/RIFLGFCY10_N.B',
    case: 'keep',
    site: 'https://www.federalreserve.gov/datadownload/',
    covers: 'H.15 yields, H.4.1 balance sheet, H.6 money stock, G.17 industrial production',
    aliases: ['fed', 'federalreserve', 'frb', 'board', 'ddp'],
  },
  {
    id: 'eia',
    label: 'US Energy Information Administration',
    code: 'EIA',
    prefix: 'eia:',
    kind: 'series',
    idLabel: 'route/facet path',
    idExample: 'petroleum/pri/gnd/EMM_EPMR_PTE_NUS_DPG',
    case: 'keep',
    site: 'https://www.eia.gov/opendata/',
    covers:
      'crude, gasoline, natural gas, electricity and generation — the feeds energy markets settle on',
    aliases: ['eia', 'energy'],
    key: {
      env: 'EIA_API_KEY',
      signup: 'https://www.eia.gov/opendata/register.php',
      // The EIA publishes no anonymous tier at all, so this is the honest answer
      // rather than an omission.
      withoutKey: '',
    },
  },
  {
    id: 'cftc',
    label: 'CFTC',
    code: 'CFTC',
    prefix: 'cftc:',
    kind: 'series',
    idLabel: 'report/market/field',
    idExample: 'legacy/GOLD/noncomm_net',
    case: 'keep',
    site: 'https://publicreporting.cftc.gov',
    covers:
      'Commitments of Traders — who is long and short each futures market, weekly since 1986',
    aliases: ['cftc', 'cot', 'commitments'],
  },
  {
    id: 'congress',
    label: 'Congress.gov',
    code: 'CONG',
    prefix: 'congress:',
    kind: 'documents',
    idLabel: 'congress/type/number',
    idExample: '119/hr/1',
    case: 'lower',
    site: 'https://api.congress.gov',
    covers: 'bills, amendments, members and roll-call actions back to the 93rd Congress',
    aliases: ['congress', 'congressgov', 'bills', 'gov'],
    key: {
      env: 'CONGRESS_API_KEY',
      signup: 'https://api.congress.gov/sign-up/',
      withoutKey: "api.data.gov's shared DEMO_KEY, which is rate-limited per IP",
    },
  },
  {
    id: 'sec',
    label: 'SEC EDGAR',
    code: 'SEC',
    prefix: 'sec:',
    kind: 'documents',
    idLabel: 'ticker or CIK',
    idExample: 'AAPL',
    case: 'upper',
    site: 'https://www.sec.gov/edgar/sec-api-documentation',
    covers: 'company filings and every XBRL fact a US issuer has reported since 2009',
    aliases: ['sec', 'edgar'],
  },
  {
    id: 'datagov',
    label: 'data.gov',
    code: 'DGOV',
    prefix: 'datagov:',
    kind: 'documents',
    idLabel: 'dataset id',
    idExample: 'consumer-price-index',
    case: 'lower',
    site: 'https://data.gov/developers/apis/',
    covers:
      "the US government's dataset catalogue — 300,000+ datasets across every federal agency",
    aliases: ['datagov', 'data-gov', 'usgov', 'usa'],
    key: {
      env: 'DATAGOV_API_KEY',
      signup: 'https://api.data.gov/signup/',
      withoutKey: "api.data.gov's shared DEMO_KEY, which is rate-limited per IP",
    },
  },
  {
    id: 'polygon',
    label: 'Polygon.io',
    code: 'POLY',
    prefix: 'polygon:',
    kind: 'prices',
    idLabel: 'symbol',
    idExample: 'AAPL',
    case: 'upper',
    site: 'https://polygon.io/docs',
    covers:
      'consolidated US equity, index, FX and crypto bars — the paid tape behind STK, CRY and IMP',
    aliases: ['polygon', 'poly', 'polygonio'],
    key: {
      env: 'POLYGON_API_KEY',
      signup: 'https://polygon.io/dashboard/api-keys',
      withoutKey: '',
    },
  },
] as const satisfies readonly DataSourceDefinition[];

export interface DataSourceInfo extends DataSourceDefinition {
  id: DataSource;
}

export const DATA_SOURCES: readonly DataSourceInfo[] = SOURCE_TABLE;

export const DATA_SOURCE_IDS: readonly DataSource[] = DATA_SOURCES.map((s) => s.id);

/** The sources `ECO` can chart and `ECOS` can search, in table order. */
export const SERIES_SOURCE_IDS: readonly DataSource[] = DATA_SOURCES.filter(
  (s) => s.kind === 'series',
).map((s) => s.id);

const BY_ID = new Map<DataSource, DataSourceInfo>(DATA_SOURCES.map((s) => [s.id, s]));

export function dataSourceInfo(source: DataSource): DataSourceInfo {
  const info = BY_ID.get(source);
  if (!info) throw new Error(`Unknown data source "${source}"`);
  return info;
}

export function isDataSource(value: string): value is DataSource {
  return BY_ID.has(value as DataSource);
}

/** The source an unprefixed reference belongs to. */
export const DEFAULT_DATA_SOURCE: DataSource = (
  DATA_SOURCES.find((s) => s.isDefault) ?? DATA_SOURCES[0]!
).id;

/** Alias → source, built from the table rather than maintained beside it. */
const ALIASES: ReadonlyMap<string, DataSource> = new Map(
  DATA_SOURCES.flatMap((s) => s.aliases.map((alias) => [alias, s.id] as const)),
);

/**
 * Aliases that are ordinary English words.
 *
 * Fine as a prefix — `gov:119/hr/1` is unambiguous — but they must not be
 * claimed out of free text, or `ECOS government spending` quietly becomes a
 * Congress.gov search for one word. Same rule, and same reason, as the venue
 * table's prefix-only aliases.
 */
const PREFIX_ONLY_ALIASES: ReadonlySet<string> = new Set([
  'gov',
  'usa',
  'energy',
  'labor',
  'labour',
  'fund',
  'board',
  'bills',
  'poly',
]);

/**
 * Read a source name or alias. `null` when the token names no source.
 *
 * `context` says where the token came from. A `prefix` reading accepts every
 * alias; a `word` reading — one token among the words of a search — declines
 * the ones that are also ordinary English.
 */
export function parseDataSource(
  token: string,
  context: 'prefix' | 'word' = 'prefix',
): DataSource | null {
  const key = token.trim().toLowerCase();
  if (context === 'word' && PREFIX_ONLY_ALIASES.has(key)) return null;
  return ALIASES.get(key) ?? null;
}

/** A series, filing or dataset at a named publisher. */
export interface DataRef {
  source: DataSource;
  id: string;
}

/** Normalise an identifier to the case its publisher actually answers to. */
export function normaliseDataId(source: DataSource, id: string): string {
  const casing = dataSourceInfo(source).case;
  if (casing === 'upper') return id.toUpperCase();
  if (casing === 'lower') return id.toLowerCase();
  return id;
}

/**
 * Read `[source:]identifier`.
 *
 * Only a *known* prefix is treated as one. Several publishers put colons
 * nowhere in an identifier and none put one first, so a bare `foo:bar` with an
 * unrecognised `foo` is far more likely to be a typo than an id — it is
 * reported rather than silently sent to FRED, which would 404 it with a message
 * about FRED.
 */
export function parseDataRef(raw: string, fallback: DataSource = DEFAULT_DATA_SOURCE): DataRef {
  const text = raw.trim();
  const colon = text.indexOf(':');

  if (colon > 0) {
    const source = parseDataSource(text.slice(0, colon));
    if (source) {
      const id = text.slice(colon + 1).trim();
      if (!id) throw new Error(`"${raw}" names a source but no ${dataSourceInfo(source).idLabel}`);
      return { source, id: normaliseDataId(source, id) };
    }
    // Only the chartable ones are offered: pointing a reader at `sec:` in
    // answer to a mistyped `ECO` prefix sends them to a source that command
    // cannot draw.
    throw new Error(
      `Unknown source prefix "${text.slice(0, colon)}". Try ` +
        DATA_SOURCES.filter((s) => s.kind === 'series')
          .map((s) => s.prefix)
          .join(', '),
    );
  }

  if (!text) throw new Error('Missing series id');
  return { source: fallback, id: normaliseDataId(fallback, text) };
}

/**
 * The canonical string form of a reference.
 *
 * The default source prints bare, because that is what every existing command,
 * example and README line already says.
 */
export function formatDataRef(ref: DataRef): string {
  const info = dataSourceInfo(ref.source);
  return info.bareRef ? ref.id : `${info.prefix}${ref.id}`;
}
