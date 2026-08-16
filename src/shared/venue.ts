/**
 * The venues the terminal quotes, and how a contract at one of them is named.
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
 */

export type Venue = 'kalshi' | 'polymarket' | 'polymarket-us';

export interface VenueInfo {
  id: Venue;
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
}

export const VENUES: readonly VenueInfo[] = [
  {
    id: 'kalshi',
    label: 'Kalshi',
    code: 'KAL',
    prefix: 'kx:',
    idLabel: 'ticker',
    case: 'upper',
    site: 'https://kalshi.com',
  },
  {
    id: 'polymarket',
    label: 'Polymarket International',
    code: 'PM',
    prefix: 'pm:',
    idLabel: 'slug',
    case: 'lower',
    site: 'https://polymarket.com',
  },
  {
    id: 'polymarket-us',
    label: 'Polymarket US',
    code: 'PMUS',
    prefix: 'pmus:',
    idLabel: 'slug',
    case: 'lower',
    site: 'https://polymarket.us',
  },
] as const;

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

/**
 * Names a trader might type for a venue.
 *
 * Deliberately generous — `pm`, `poly` and `intl` all mean the international
 * book — because the cost of accepting a synonym is nil and the cost of
 * rejecting one is a command that has to be retyped.
 */
const ALIASES: Record<string, Venue> = {
  kalshi: 'kalshi',
  kal: 'kalshi',
  kx: 'kalshi',
  k: 'kalshi',
  polymarket: 'polymarket',
  poly: 'polymarket',
  pm: 'polymarket',
  intl: 'polymarket',
  international: 'polymarket',
  'polymarket-intl': 'polymarket',
  polymarketus: 'polymarket-us',
  'polymarket-us': 'polymarket-us',
  polyus: 'polymarket-us',
  pmus: 'polymarket-us',
  'pm-us': 'polymarket-us',
  us: 'polymarket-us',
};

/** Read a venue name or alias. `null` when the token names no venue. */
export function parseVenue(token: string): Venue | null {
  return ALIASES[token.trim().toLowerCase()] ?? null;
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

/**
 * Read `[venue:]identifier`.
 *
 * An unprefixed reference belongs to `fallback`, which is Kalshi unless a
 * caller says otherwise — every command in the terminal predates the other two
 * venues, and none of their documented examples should change meaning.
 *
 * Only a *known* prefix is treated as one. Polymarket slugs are full of
 * colons' cousins but not colons, and Kalshi tickers have none, so a bare
 * `foo:bar` with an unrecognised `foo` is far more likely to be a typo than an
 * identifier — it is reported rather than silently mangled.
 */
export function parseRef(raw: string, fallback: Venue = 'kalshi'): VenueRef {
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
 * Kalshi references print bare, because that is what every existing command,
 * example and README line already says.
 */
export function formatRef(ref: VenueRef): string {
  return ref.venue === 'kalshi' ? ref.id : `${venueInfo(ref.venue).prefix}${ref.id}`;
}
