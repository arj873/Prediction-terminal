/**
 * The venues the terminal quotes: who they are, what they can answer, and how
 * a contract at one of them is named.
 *
 * Three brokers list the same questions under three naming schemes. Kalshi has
 * upper-case tickers (`KXFEDDECISION-26SEP-T3.75`), both Polymarkets have
 * lower-case slugs (`will-the-fed-decrease-interest-rates-…`,
 * `tec-mlb-nlchamp-2026-09-27-lad`). Rather than three parallel command sets,
 * every command takes one *reference* — an optional venue prefix and an
 * identifier — and this module is the only place that knows how to read one.
 *
 *   KXFEDDECISION-26SEP-T3.75      → kalshi (the default, so nothing changes)
 *   pm:fed-decision-in-september   → Polymarket International
 *   pmus:fed-decision-2026-09-16   → Polymarket US
 *
 * Case is part of the identifier, not decoration: Kalshi 404s a lower-case
 * ticker and Polymarket 404s an upper-case slug, so folding happens here, once,
 * per venue — never at the call site.
 *
 * One table drives all of it. `Venue` is *derived* from the table rather than
 * declared beside it, so a broker that is added to one and not the other is a
 * compile error instead of a runtime throw. Everything a venue differs by —
 * its aliases, whether it prints bare, whether it is the default for an
 * unprefixed reference, and which endpoints it actually serves — is a field
 * here rather than a `=== 'kalshi'` somewhere downstream.
 */

/** How `TOP` ranks a book. Defined here because every venue declares which it can serve. */
export type MoverSort = 'volume' | 'gainers' | 'losers' | 'open_interest' | 'liquidity';

/**
 * What a venue's public API actually offers.
 *
 * Declared rather than discovered. The alternative — call the endpoint and see
 * whether it throws `unsupported` — means the UI cannot know until after it has
 * offered the reader a button, and it cannot tell "this venue publishes no
 * volume" apart from "this venue is down". Both of those were live bugs.
 */
export interface VenueCapabilities {
  /** Publishes price history the terminal can chart (`GP`). */
  candles: boolean;
  /** Publishes a public print tape (`TAS`). */
  trades: boolean;
  /** `listSeries` honours a category filter. */
  seriesCategoryFilter: boolean;
  /** The rankings this venue populates. `TOP` will not ask it for the others. */
  sorts: readonly MoverSort[];
  /**
   * Why a missing capability is missing, shown to the reader in place of a
   * dead button. Absent when the venue serves everything.
   */
  note?: string;
}

/** The table's own row shape. `id` widens to `string` so `Venue` can derive from it. */
interface VenueDefinition {
  id: string;
  /** Full name, for panel headers and error messages. */
  label: string;
  /** Short badge for a table column. */
  code: string;
  /** Canonical command-line prefix, including the colon. */
  prefix: string;
  /** What this venue calls a contract identifier, for usage text. */
  idLabel: string;
  /** Identifier case. Kalshi shouts; the Polymarkets do not. */
  case: 'upper' | 'lower';
  /** Public web page for a market, so a panel can link out. */
  site: string;
  /**
   * Names a trader might type. Deliberately generous — the cost of accepting a
   * synonym is nil and the cost of rejecting one is a command retyped.
   */
  aliases: readonly string[];
  /** An unprefixed reference belongs to this venue. Exactly one venue sets it. */
  isDefault?: boolean;
  /** This venue's references print without their prefix. Follows from being the default. */
  bareRef?: boolean;
  capabilities: VenueCapabilities;
}

const ALL_SORTS = ['volume', 'gainers', 'losers', 'open_interest', 'liquidity'] as const;

const VENUE_TABLE = [
  {
    id: 'kalshi',
    label: 'Kalshi',
    code: 'KAL',
    prefix: 'kx:',
    idLabel: 'ticker',
    case: 'upper',
    site: 'https://kalshi.com',
    aliases: ['kalshi', 'kal', 'kx', 'k'],
    // Every command, example and habit in this terminal predates the other two
    // venues; an unprefixed reference has to keep meaning what it always did.
    isDefault: true,
    bareRef: true,
    capabilities: {
      candles: true,
      trades: true,
      seriesCategoryFilter: true,
      sorts: ALL_SORTS,
    },
  },
  {
    id: 'polymarket',
    label: 'Polymarket International',
    code: 'PM',
    prefix: 'pm:',
    idLabel: 'slug',
    case: 'lower',
    site: 'https://polymarket.com',
    aliases: ['polymarket', 'poly', 'pm', 'intl', 'international', 'polymarket-intl'],
    capabilities: {
      candles: true,
      trades: true,
      seriesCategoryFilter: false,
      // The catalogue carries no open interest, so an OI board would rank it last
      // on a figure it never published rather than on a small one.
      sorts: ['volume', 'gainers', 'losers', 'liquidity'],
      note: 'Polymarket publishes price samples rather than OHLC bars, and no open interest.',
    },
  },
  {
    id: 'polymarket-us',
    label: 'Polymarket US',
    code: 'PMUS',
    prefix: 'pmus:',
    idLabel: 'slug',
    case: 'lower',
    site: 'https://polymarket.us',
    aliases: ['polymarketus', 'polymarket-us', 'polyus', 'pmus', 'pm-us', 'us'],
    capabilities: {
      candles: false,
      trades: false,
      seriesCategoryFilter: false,
      // Its public catalogue carries no volume, change or open interest at all,
      // so it can be ranked on nothing.
      sorts: [],
      note:
        'Polymarket US serves price history, prints and ranking figures only to an ' +
        'authenticated caller. The terminal reads public endpoints only, so quote ' +
        'and book (DES, OB) work and the rest say so.',
    },
  },
] as const satisfies readonly VenueDefinition[];

/** Derived from the table, so the two can never disagree. */
export type Venue = (typeof VENUE_TABLE)[number]['id'];

export interface VenueInfo extends VenueDefinition {
  id: Venue;
}

export const VENUES: readonly VenueInfo[] = VENUE_TABLE;

export const VENUE_IDS: readonly Venue[] = VENUES.map((v) => v.id);

const BY_ID = new Map<Venue, VenueInfo>(VENUES.map((v) => [v.id, v]));

export function venueInfo(venue: Venue): VenueInfo {
  const info = BY_ID.get(venue);
  if (!info) throw new Error(`Unknown venue "${venue}"`);
  return info;
}

export function isVenue(value: string): value is Venue {
  return BY_ID.has(value as Venue);
}

/** The venue an unprefixed reference belongs to. */
export const DEFAULT_VENUE: Venue = (VENUES.find((v) => v.isDefault) ?? VENUES[0]!).id;

/** Alias → venue, built from the table rather than maintained beside it. */
const ALIASES: ReadonlyMap<string, Venue> = new Map(
  VENUES.flatMap((v) => v.aliases.map((alias) => [alias, v.id] as const)),
);

/**
 * Aliases that are ordinary English words.
 *
 * These are fine as a prefix — `us:` is unambiguous — but must not be claimed
 * out of free text, or `SRCH us election` quietly becomes "search Polymarket US
 * for *election*" and `TV The Office US` searches the wrong book. A venue
 * filter is a convenience; silently changing which exchange was searched is not.
 */
const PREFIX_ONLY_ALIASES: ReadonlySet<string> = new Set(['us', 'k', 'intl', 'international']);

/**
 * Read a venue name or alias. `null` when the token names no venue.
 *
 * `context` says where the token came from. A `prefix` reading accepts every
 * alias; a `word` reading — one token among the words of a search — declines
 * the ones that are also ordinary English.
 */
export function parseVenue(token: string, context: 'prefix' | 'word' = 'prefix'): Venue | null {
  const key = token.trim().toLowerCase();
  if (context === 'word' && PREFIX_ONLY_ALIASES.has(key)) return null;
  return ALIASES.get(key) ?? null;
}

/** A contract, event or series at a named venue. */
export interface VenueRef {
  venue: Venue;
  id: string;
}

/** Normalise an identifier to the case its venue actually answers to. */
export function normaliseId(venue: Venue, id: string): string {
  return venueInfo(venue).case === 'upper' ? id.toUpperCase() : id.toLowerCase();
}

/** Identifiers are one path segment: a letter or digit, then the venue's punctuation. */
const IDENTIFIER = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;

/**
 * Whether an identifier is safe to splice into an upstream URL path.
 *
 * `encodeURIComponent` leaves `.` alone, so a ticker of `..` reached the
 * upstream as a dot-segment and URL normalisation quietly resolved
 * `…/v2/markets/..` to `…/v2/` — a different endpoint on the same host, chosen
 * by the caller. Requiring the first character to be alphanumeric is what rules
 * that out: `.` and `..` are the only segments normalisation collapses, and
 * neither can start with a letter or a digit.
 *
 * A predicate rather than an assertion because this module is shared with the
 * browser and has no opinion on how a refusal should be reported — each venue
 * source raises its own, naming what *it* calls an identifier.
 */
export function isValidIdentifier(id: string): boolean {
  return IDENTIFIER.test(id);
}

/**
 * Read `[venue:]identifier`.
 *
 * Only a *known* prefix is treated as one. Polymarket slugs are full of
 * colons' cousins but not colons, and Kalshi tickers have none, so a bare
 * `foo:bar` with an unrecognised `foo` is far more likely to be a typo than an
 * identifier — it is reported rather than silently mangled.
 */
export function parseRef(raw: string, fallback: Venue = DEFAULT_VENUE): VenueRef {
  const text = raw.trim();
  const colon = text.indexOf(':');

  if (colon > 0) {
    const venue = parseVenue(text.slice(0, colon));
    if (venue) {
      const id = text.slice(colon + 1).trim();
      if (!id) throw new Error(`"${raw}" names a venue but no ${venueInfo(venue).idLabel}`);
      return { venue, id: normaliseId(venue, id) };
    }
    throw new Error(
      `Unknown venue prefix "${text.slice(0, colon)}". Try ${VENUES.map((v) => v.prefix).join(', ')}`,
    );
  }

  return { venue: fallback, id: normaliseId(fallback, text) };
}

/**
 * The canonical string form of a reference.
 *
 * The default venue prints bare, because that is what every existing command,
 * example and README line already says.
 */
export function formatRef(ref: VenueRef): string {
  const info = venueInfo(ref.venue);
  return info.bareRef ? ref.id : `${info.prefix}${ref.id}`;
}

/** Whether a venue serves a given capability, for a UI deciding what to offer. */
export function supports(venue: Venue, capability: 'candles' | 'trades'): boolean {
  return venueInfo(venue).capabilities[capability];
}

/** Whether `TOP` can rank this venue by `sort`. */
export function supportsSort(venue: Venue, sort: MoverSort): boolean {
  return venueInfo(venue).capabilities.sorts.includes(sort);
}
