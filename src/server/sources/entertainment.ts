/**
 * Kalshi's entertainment universe, grouped into genres.
 *
 * Kalshi files ~2,500 series under the `Entertainment` category and ~545 of
 * them have open events at any time, but the API offers no sub-category: the
 * `/events` endpoint accepts a `category` parameter and ignores it, so the only
 * discriminator available is the ticker itself. Kalshi's tickers are
 * disciplined — `KXOSCARPIC`, `KXNETFLIXRANKSHOW`, `KXGAMEAWARDS` — so a prefix
 * table classifies them reliably, with title keywords as a backstop for the
 * handful of one-off series that do not follow the convention.
 *
 * Genres are *tags*, not a partition. An Oscar market is both `film` and
 * `awards`, and someone typing `ENT film` expects to see it.
 *
 * The second half of the table is the interesting one: `FEEDS` maps a series to
 * the terminal command that shows the data the market settles against. Those
 * pairings are not guesses — they come from each series' own
 * `settlement_sources`, which name the upstream Kalshi resolves against.
 */

import type { EntEvent, EntGenre, EntResponse, VenueEvent } from '../../shared/types.js';
import { ENT_GENRES } from '../../shared/types.js';
import { UpstreamError } from '../lib/http.js';
import { sumOrNull } from './corpus.js';
import { corpusSnapshot } from './kalshi.js';

const CATEGORY = 'entertainment';

/**
 * Series-ticker prefix → genres.
 *
 * Ordered longest-prefix-first at match time, so `KXGAMEAWARDS` resolves to
 * games+awards rather than stopping at a shorter `KXGAME` rule.
 */
const PREFIX_GENRES: Record<string, EntGenre[]> = {
  // ---- music -------------------------------------------------------------
  KXALBUM: ['music'],
  KXPUREALBUMS: ['music'],
  KXALBUMEQUIV: ['music'],
  KXARTISTSTREAMS: ['music'],
  KXTOPARTIST: ['music'],
  KXTOPSONG: ['music'],
  KXTOPALBUM: ['music'],
  KXTOPMONTHLY: ['music'],
  KX1ALBUM: ['music'],
  KX1SONG: ['music'],
  KX10SONG: ['music'],
  KX20SONG: ['music'],
  KXBILLBOARD: ['music'],
  KXWEEKSNUM1: ['music'],
  KXSONGRELEASE: ['music'],
  KXTOUR: ['music'],
  KXHEADLINE: ['music'],
  KXPERFORMSUPERBOWL: ['music'],
  KXSUPERBOWLHEADLINE: ['music'],
  KXROLEATEVENTCOACHELLA: ['music'],
  KXROLEATEVENT: ['music'],
  KXEUROVISION: ['music'],
  KXYT: ['music'],
  KXRANKLISTSONG: ['music'],
  KXLEAVEGROUP: ['music'],
  KXSPOTIFY: ['music'],
  KXNEWTAYLOR: ['music'],

  // ---- film --------------------------------------------------------------
  KXRT: ['film'],
  KXRTCOMPARE: ['film'],
  KXBOND: ['film'],
  KXMOVIE: ['film'],
  KXMOVIECAST: ['film'],
  KXMOVIEDELAY: ['film'],
  KXMOVIERELEASEDATE: ['film'],
  KXROLEINPRODUCTION: ['film'],
  KXPERFORMROLE: ['film'],
  KXVFILMFESTIVAL: ['film', 'awards'],
  KXAVENGERS: ['film'],
  KXIRONMAN: ['film'],
  KXGGBOXOFFICE: ['film', 'awards'],

  // ---- tv ----------------------------------------------------------------
  KXBIGBROTHER: ['tv'],
  KXNETFLIX: ['tv'],
  KXDWTS: ['tv'],
  KXSNL: ['tv'],
  KXSUMMERHOUSE: ['tv'],
  KXTVSHOWSCANCELLED: ['tv'],
  KXMEDIAGUEST: ['tv'],
  KXMEDIARELEASE: ['tv'],
  KX60MINUTES: ['tv'],
  KXSPINOFF: ['tv'],
  KXLOVEISLAND: ['tv'],
  KXUPONLY: ['tv'],

  // ---- games -------------------------------------------------------------
  KXGAME: ['games'],
  KXGAMEAWARDS: ['games', 'awards'],
  KXGAMERELEASE: ['games'],
  KXGTA: ['games'],
  KXESVI: ['games'],
  KXPS6: ['games'],
  KXSTEAM: ['games', 'awards'],
  KXPOKEMON: ['games'],
  KXVIDEOLENGTH: ['games'],
  KXMETACRITIC: ['games'],
  GAMERANK: ['games'],
  KXANIME: ['games', 'awards'],

  // ---- awards ------------------------------------------------------------
  KXOSCAR: ['film', 'awards'],
  KXEMMY: ['tv', 'awards'],
  KXGRAM: ['music', 'awards'],
  KXLGRAM: ['music', 'awards'],
  KXBAFTA: ['film', 'awards'],
  KXGOLDENGLOBE: ['film', 'awards'],
  KXCRITICS: ['film', 'awards'],
  KXAMA: ['music', 'awards'],
  KXACMA: ['music', 'awards'],
  KXTIME: ['awards', 'celeb'],
  KXSEXYMAN: ['awards', 'celeb'],
  KXWORDOFTHEYEAR: ['awards'],

  // ---- celebrity / culture ------------------------------------------------
  KXSWIFT: ['celeb', 'music'],
  KXKIMK: ['celeb'],
  KXENGAGEMENT: ['celeb'],
  KXFOLLOWERCOUNT: ['celeb'],
  KXTWITCHSUBS: ['celeb', 'games'],
  KXRANKLISTGOOGLESEARCH: ['celeb'],
  KXMEDIACOVER: ['celeb'],
  KXPERSON: ['celeb'],
  KXHERMES: ['celeb'],
  KXART: ['celeb'],
};

/** Fallback when the ticker is not in the table: match on the event title. */
const TITLE_GENRES: [RegExp, EntGenre[]][] = [
  [/\b(album|song|single|billboard|spotify|streams?|tour|concert|headlin|grammy)\b/i, ['music']],
  [/\b(box office|rotten tomatoes|film|movie|oscar|cast as|screenplay|director)\b/i, ['film']],
  [/\b(netflix|episode|season|series|emmy|show|tv|premiere)\b/i, ['tv']],
  [/\b(game|steam|nintendo|playstation|xbox|console|dlc|speedrun)\b/i, ['games']],
  [/\b(award|nominee|winner|nomination)\b/i, ['awards']],
];

/**
 * Series → the command that shows what the market settles against.
 *
 * Read off each series' `settlement_sources`. `KXRT` settles on
 * rottentomatoes.com, so `RT` is the companion command; `KXNETFLIXRANKSHOW`
 * settles on Netflix's own Top 10 publication, so `NFLX` is. Longest prefix
 * wins, same as the genre table.
 */
const FEEDS: Record<string, { command: string; source: string }> = {
  KXRT: { command: 'RT', source: 'Rotten Tomatoes' },
  KXRTCOMPARE: { command: 'RT', source: 'Rotten Tomatoes' },
  KXNETFLIX: { command: 'NFLX', source: 'Netflix Top 10' },
  KXTOPARTIST: { command: 'SPOT', source: 'Spotify charts' },
  KXTOPSONGSPOTIFY: { command: 'SPOT', source: 'Spotify charts' },
  KXTOPALBUMSPOTIFY: { command: 'SPOT', source: 'Spotify charts' },
  KXARTISTSTREAMS: { command: 'SPOT', source: 'Spotify charts' },
  KXTOPMONTHLY: { command: 'SPOT', source: 'Spotify charts' },
  KXSPOTIFY: { command: 'SPOT', source: 'Spotify charts' },
  KXYT: { command: 'YT', source: 'YouTube charts' },
  KXTOPSONG: { command: 'BB hot-100', source: 'Billboard Hot 100' },
  KXTOPALBUM: { command: 'BB billboard-200', source: 'Billboard 200' },
  KXBILLBOARD: { command: 'BB hot-100', source: 'Billboard' },
  KX1SONG: { command: 'BB hot-100', source: 'Billboard Hot 100' },
  KX10SONG: { command: 'BB hot-100', source: 'Billboard Hot 100' },
  KX20SONG: { command: 'BB hot-100', source: 'Billboard Hot 100' },
  KX1ALBUM: { command: 'BB billboard-200', source: 'Billboard 200' },
  KXWEEKSNUM1: { command: 'BB hot-100', source: 'Billboard Hot 100' },
  KXALBUMEQUIV: { command: 'BB billboard-200', source: 'Luminate / Billboard' },
  KXPUREALBUMS: { command: 'BB billboard-200', source: 'Luminate / Billboard' },
  KXRANKLISTSONG: { command: 'BB hot-100', source: 'Billboard Hot 100' },
  KXSTEAM: { command: 'STEAM', source: 'Steam' },
  GAMERANK: { command: 'STEAM', source: 'Steam' },
  KXGGBOXOFFICE: { command: 'BO', source: 'Box Office Mojo' },
  KXBIGBROTHER: { command: 'TV', source: 'TV schedule' },
  KXDWTS: { command: 'TV', source: 'TV schedule' },
  KXSNL: { command: 'TV', source: 'TV schedule' },
};

/** Longest matching prefix in `table`, or undefined. */
function longestPrefix<T>(ticker: string, table: Record<string, T>): T | undefined {
  let best: string | undefined;
  for (const key of Object.keys(table)) {
    if (!ticker.startsWith(key)) continue;
    if (best === undefined || key.length > best.length) best = key;
  }
  return best === undefined ? undefined : table[best];
}

/**
 * Genres for one event.
 *
 * Exported for tests: the classification is the whole value of this module, and
 * a silent misclassification (Oscars filed under `music`) is exactly the kind of
 * bug that never surfaces as an error.
 */
export function classify(seriesTicker: string, title: string): EntGenre[] {
  const ticker = seriesTicker.toUpperCase();
  const fromPrefix = longestPrefix(ticker, PREFIX_GENRES);
  if (fromPrefix) return [...fromPrefix];

  const tags = new Set<EntGenre>();
  for (const [pattern, genres] of TITLE_GENRES) {
    if (pattern.test(title)) for (const g of genres) tags.add(g);
  }
  // Everything under Kalshi's Entertainment category is *something*; an event
  // that matches nothing is still worth listing rather than silently dropping.
  return tags.size > 0 ? [...tags] : ['celeb'];
}

/** The companion data command for a series, if this terminal has one. */
export function feedFor(seriesTicker: string): { command: string; source: string } | undefined {
  return longestPrefix(seriesTicker.toUpperCase(), FEEDS);
}

export function assertGenre(raw: string): EntGenre | 'all' {
  const value = raw.trim().toLowerCase();
  if (value === '' || value === 'all') return 'all';
  if ((ENT_GENRES as readonly string[]).includes(value)) return value as EntGenre;
  throw new UpstreamError(`"${raw}" is not an entertainment genre`, {
    code: 'bad_request',
    hint: `Genres are: ${ENT_GENRES.join(', ')}, or ALL.`,
  });
}

/** Shape one corpus event into the entertainment view. */
export function toEntEvent(event: VenueEvent): EntEvent {
  const genres = classify(event.seriesTicker, `${event.title} ${event.subTitle}`);
  const markets = [...event.markets].sort((a, b) => (b.volume24h ?? 0) - (a.volume24h ?? 0));

  // The soonest close is the one a trader is actually racing; an event whose
  // legs close on different days should show the nearest, not an arbitrary one.
  const closes = markets.map((m) => m.closeTime).filter(Boolean).sort();

  const feed = feedFor(event.seriesTicker);

  return {
    eventTicker: event.eventTicker,
    seriesTicker: event.seriesTicker,
    title: event.title,
    subTitle: event.subTitle,
    genres,
    markets,
    // This view is Kalshi-only, and Kalshi publishes both figures on every
    // market, so the fallback is unreachable rather than a silent zero.
    volume24h: sumOrNull(markets, (m) => m.volume24h) ?? 0,
    openInterest: sumOrNull(markets, (m) => m.openInterest) ?? 0,
    closeTime: closes[0] ?? '',
    ...(feed ? { feed } : {}),
  };
}

/**
 * Open entertainment events, optionally narrowed to one genre.
 *
 * Reads the shared open-event snapshot rather than crawling: `SRCH` and `TOP`
 * already keep it warm, and it is the only view of the universe that excludes
 * Kalshi's auto-generated parlay legs.
 */
export async function browse(genre: EntGenre | 'all', limit = 60): Promise<EntResponse> {
  const snapshot = await corpusSnapshot();

  const all = snapshot.events
    .filter((e) => e.category.toLowerCase() === CATEGORY)
    .map(toEntEvent);

  const counts: Record<string, number> = { all: all.length };
  for (const g of ENT_GENRES) counts[g] = all.filter((e) => e.genres.includes(g)).length;

  const events = (genre === 'all' ? all : all.filter((e) => e.genres.includes(genre)))
    .sort((a, b) => b.volume24h - a.volume24h || b.openInterest - a.openInterest)
    .slice(0, limit);

  return {
    genre,
    events,
    scanned: snapshot.events.length,
    snapshotAgeSeconds: Math.round((Date.now() - snapshot.builtAt) / 1000),
    counts,
  };
}
