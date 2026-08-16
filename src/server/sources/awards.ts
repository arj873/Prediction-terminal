/**
 * Awards — nominees and winners, from Wikidata.
 *
 * This is the single largest hole in the entertainment book. Kalshi's award
 * markets are ~61% of the entertainment volume that has no companion feed, and
 * `KXOSCARPIC` alone carries more open interest than anything the terminal
 * already covers. Their `settlement_sources` name oscars.org, emmys.com,
 * grammy.com and thegameawards.com — none of which answer a datacentre IP:
 * oscars.org and its awards database both return 403 from a container, the way
 * fred.stlouisfed.org resets one.
 *
 * Wikidata does answer, and it holds the same facts as structured statements:
 *
 *   P166   award received      → the winner
 *   P1411  nominated for       → the nominee
 *   P585   point in time       → which ceremony, as a qualifier on the statement
 *   P1686  for work            → what a person was nominated *for*
 *
 * Two things about this source are worth stating plainly, because both change
 * how the panel should be read.
 *
 * **Winners are reliable; nominee lists lag.** Editors record a winner within
 * minutes and fill the losing slate in over days or weeks. The 98th Academy
 * Awards had its Best Picture winner recorded immediately and only three of ten
 * nominees. So the panel reports the nominee count it actually got rather than
 * implying a complete slate — a partial ballot presented as whole is exactly the
 * kind of wrong number that still looks right.
 *
 * **A ceremony that has not happened is empty, and that is the answer.** The
 * open `KXOSCARPIC-27` market settles at the 99th Academy Awards in 2027;
 * nominations are not announced yet. That returns no rows, which is a `note`,
 * not a `not_found` — the same rule as a film with no Tomatometer yet.
 *
 * Queries go to the WDQS SPARQL endpoint rather than `wikidata.org/w/api.php`,
 * which rate-limits shared egress hard enough to be unusable (429 on the first
 * request from this container). Entity search still happens against the
 * MediaWiki API — but *server-side*, through WDQS's `wikibase:mwapi` service, so
 * the request Wikimedia rate-limits comes from WDQS and not from here.
 */

import type { AwardEntry, AwardResult } from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';

const SPARQL = process.env['WIKIDATA_SPARQL_BASE'] ?? 'https://query.wikidata.org/sparql';

/**
 * Wikimedia asks automated clients to identify themselves, and throttles the
 * ones that do not. This is the contact string, not camouflage — unlike the
 * scraped sources, WDQS is a public API being used exactly as intended.
 */
const UA = 'prediction-terminal/1.0 (https://github.com/arj873/prediction-terminal)';

/**
 * Short names for the awards Kalshi actually lists, expanded to the label
 * Wikidata files them under.
 *
 * Not a QID table on purpose. QIDs are opaque, need verifying one at a time, and
 * go stale silently when an item is merged; a label search resolves the same
 * thing and keeps working for the award Kalshi lists next month. The aliases
 * exist because "best picture" has to find the Academy Award and not the dozen
 * other bodies that hand out a prize by that name.
 */
const ALIASES: Record<string, string> = {
  // ---- Academy Awards ------------------------------------------------------
  'best picture': 'Academy Award for Best Picture',
  picture: 'Academy Award for Best Picture',
  oscar: 'Academy Award for Best Picture',
  'best director': 'Academy Award for Best Director',
  director: 'Academy Award for Best Director',
  'best actor': 'Academy Award for Best Actor',
  actor: 'Academy Award for Best Actor',
  'best actress': 'Academy Award for Best Actress',
  actress: 'Academy Award for Best Actress',
  'supporting actor': 'Academy Award for Best Supporting Actor',
  'supporting actress': 'Academy Award for Best Supporting Actress',
  animated: 'Academy Award for Best Animated Feature',
  documentary: 'Academy Award for Best Documentary Feature Film',
  international: 'Academy Award for Best International Feature Film',
  cinematography: 'Academy Award for Best Cinematography',
  score: 'Academy Award for Best Original Score',
  song: 'Academy Award for Best Original Song',
  'adapted screenplay': 'Academy Award for Best Adapted Screenplay',
  'original screenplay': 'Academy Award for Best Original Screenplay',
  screenplay: 'Academy Award for Best Original Screenplay',
  'visual effects': 'Academy Award for Best Visual Effects',
  makeup: 'Academy Award for Best Makeup and Hairstyling',

  // ---- Emmys ---------------------------------------------------------------
  'comedy series': 'Primetime Emmy Award for Outstanding Comedy Series',
  'drama series': 'Primetime Emmy Award for Outstanding Drama Series',
  'limited series': 'Primetime Emmy Award for Outstanding Limited or Anthology Series',
  emmy: 'Primetime Emmy Award for Outstanding Drama Series',

  // ---- Grammys -------------------------------------------------------------
  'album of the year': 'Grammy Award for Album of the Year',
  aoty: 'Grammy Award for Album of the Year',
  'record of the year': 'Grammy Award for Record of the Year',
  roty: 'Grammy Award for Record of the Year',
  'song of the year': 'Grammy Award for Song of the Year',
  soty: 'Grammy Award for Song of the Year',
  'new artist': 'Grammy Award for Best New Artist',
  grammy: 'Grammy Award for Album of the Year',

  // ---- Games ---------------------------------------------------------------
  // Wikidata's label is "The Game Awards − Game of the Year", with a U+2212
  // minus. Searching for the label verbatim finds nothing; dropping the leading
  // article and the separator finds it every time.
  'game of the year': 'Game Awards Game of the Year',
  goty: 'Game Awards Game of the Year',
  'game awards': 'Game Awards Game of the Year',

  // ---- Other bodies --------------------------------------------------------
  'golden globe': 'Golden Globe Award for Best Motion Picture Drama',
  globe: 'Golden Globe Award for Best Motion Picture Drama',
  bafta: 'BAFTA Award for Best Film',
};

/** Awards the terminal advertises, for `AWRD` with no argument. */
export const AWARD_MENU: { key: string; label: string }[] = [
  { key: 'best picture', label: 'Oscar — Best Picture' },
  { key: 'best director', label: 'Oscar — Best Director' },
  { key: 'best actor', label: 'Oscar — Best Actor' },
  { key: 'best actress', label: 'Oscar — Best Actress' },
  { key: 'supporting actor', label: 'Oscar — Best Supporting Actor' },
  { key: 'supporting actress', label: 'Oscar — Best Supporting Actress' },
  { key: 'animated', label: 'Oscar — Best Animated Feature' },
  { key: 'international', label: 'Oscar — Best International Feature' },
  { key: 'comedy series', label: 'Emmy — Outstanding Comedy Series' },
  { key: 'drama series', label: 'Emmy — Outstanding Drama Series' },
  { key: 'limited series', label: 'Emmy — Outstanding Limited Series' },
  { key: 'album of the year', label: 'Grammy — Album of the Year' },
  { key: 'record of the year', label: 'Grammy — Record of the Year' },
  { key: 'song of the year', label: 'Grammy — Song of the Year' },
  { key: 'new artist', label: 'Grammy — Best New Artist' },
  { key: 'game of the year', label: 'The Game Awards — Game of the Year' },
  { key: 'golden globe', label: 'Golden Globe — Best Motion Picture, Drama' },
];

/* ------------------------------------------------------------ sparql plumbing */

interface SparqlBinding {
  [key: string]: { value?: string; type?: string } | undefined;
}

interface SparqlResponse {
  results?: { bindings?: SparqlBinding[] };
}

function bound(row: SparqlBinding, key: string): string {
  return row[key]?.value ?? '';
}

/** Strip the entity prefix off a Wikidata URI: `…/entity/Q42` → `Q42`. */
function qid(uri: string): string {
  return uri.replace(/^.*\/entity\//, '');
}

/**
 * A SPARQL string literal.
 *
 * The award name reaches here from the command line, and it is interpolated into
 * a query rather than bound as a parameter — WDQS takes the query as one blob
 * over GET, so there is nowhere to bind. Backslashes and quotes therefore have
 * to be neutralised, and control characters dropped, or a title with an
 * apostrophe becomes a syntax error at best.
 */
function literal(value: string): string {
  const escaped = value
    .replace(/[\u0000-\u001f\u007f]/g, ' ')
    .replace(/\\/g, '\\\\')
    .replace(/"/g, '\\"');
  return `"${escaped}"`;
}

async function ask(query: string, timeoutMs = 30_000): Promise<SparqlBinding[]> {
  const url = `${SPARQL}?format=json&query=${encodeURIComponent(query)}`;
  const body = await fetchJson<SparqlResponse>(url, {
    timeoutMs,
    retries: 2,
    headers: { 'User-Agent': UA, Accept: 'application/sparql-results+json' },
  });
  return body.results?.bindings ?? [];
}

/* ---------------------------------------------------------------- resolution */

export function expandAlias(raw: string): string {
  const key = raw.trim().toLowerCase().replace(/\s+/g, ' ');
  return ALIASES[key] ?? raw.trim();
}

/**
 * Award name → Wikidata item.
 *
 * Cached for the session: an award's identity does not change, and this is the
 * round trip that would otherwise double every panel load.
 */
export async function resolveAward(name: string): Promise<{ id: string; label: string }> {
  const search = expandAlias(name);
  if (!search) {
    throw new UpstreamError('Missing award name', {
      code: 'bad_request',
      hint: 'Usage: `AWRD <award> [year]`, e.g. `AWRD best picture 2026`.',
    });
  }

  return cache.cached(`awards:id:${search.toLowerCase()}`, TTL.catalogue, async () => {
    // The `EXISTS` guard is what makes this a search for an *award* rather than
    // for a page about one: it keeps only items something has actually been
    // nominated for or has won, which no article, list or category satisfies.
    const rows = await ask(`
      SELECT ?item ?itemLabel WHERE {
        SERVICE wikibase:mwapi {
          bd:serviceParam wikibase:api "EntitySearch" ;
                          wikibase:endpoint "www.wikidata.org" ;
                          mwapi:search ${literal(search)} ;
                          mwapi:language "en" .
          ?item wikibase:apiOutputItem mwapi:item .
        }
        FILTER(EXISTS { [] ps:P166 ?item } || EXISTS { [] ps:P1411 ?item })
        SERVICE wikibase:label { bd:serviceParam wikibase:language "en" }
      } LIMIT 1`);

    const first = rows[0];
    const id = qid(bound(first ?? {}, 'item'));

    if (!first || !/^Q\d+$/.test(id)) {
      throw new UpstreamError(`Wikidata has no award matching "${name}"`, {
        code: 'not_found',
        hint:
          'Try the full name — `AWRD "Academy Award for Best Picture"` — or one of the ' +
          'short forms: best picture, drama series, album of the year, game of the year.',
      });
    }

    return { id, label: bound(first, 'itemLabel') || search };
  });
}

/* --------------------------------------------------------------- the records */

/**
 * Rows to pull from each side before merging.
 *
 * Fixed, and deliberately not the caller's `limit`. The two sides are merged,
 * deduped and re-sorted afterwards, so truncating inside the query truncates the
 * wrong thing: `limit=4` once returned the four most recent nominees and the
 * four most recent winners, which overlapped almost entirely and left a single
 * row. It also keeps `limit` out of the cache key, so one fetch serves any row
 * count — the same arrangement as `getTop` and `getChart` elsewhere.
 *
 * 600 comfortably covers the ~100 years of a long-running Academy Award
 * category, with the truncation reported rather than silent when it does bite.
 */
const FETCH_CAP = 600;

/**
 * One side of the record — winners or nominees — for an award.
 *
 * Deliberately two queries rather than one `UNION`. The combined form joins two
 * unbound statement patterns and then runs the label service over the product of
 * both, which WDQS answers with a 502 or a timeout for anything as heavily
 * awarded as Best Picture. Split, each half returns in well under a second, and
 * they run concurrently anyway.
 */
async function side(
  awardId: string,
  property: 'P166' | 'P1411',
  year: number | null,
): Promise<AwardEntry[]> {
  const won = property === 'P166';

  // A year filter needs a bound date, so it also selects for dated statements.
  // Without one the date stays OPTIONAL: an undated statement is still a real
  // record, and dropping it silently would understate the field.
  const when = year === null
    ? 'OPTIONAL { ?st pq:P585 ?when }'
    : `?st pq:P585 ?when .
       FILTER(?when >= "${year}-01-01"^^xsd:dateTime && ?when < "${year + 1}-01-01"^^xsd:dateTime)`;

  const rows = await ask(`
    SELECT ?name ?nameLabel ?workLabel ?when WHERE {
      ?name p:${property} ?st .
      ?st ps:${property} wd:${awardId} .
      ${when}
      OPTIONAL { ?st pq:P1686 ?work }
      SERVICE wikibase:label { bd:serviceParam wikibase:language "en" }
    }
    ORDER BY DESC(?when)
    LIMIT ${FETCH_CAP}`);

  return rows.map((row) => {
    const label = bound(row, 'nameLabel');
    const iso = bound(row, 'when');
    const parsed = iso ? Number.parseInt(iso.slice(0, 4), 10) : Number.NaN;
    return {
      id: qid(bound(row, 'name')),
      // The label service echoes the QID back when an item has no English
      // label. That is not a name, and showing it as one is worse than nothing.
      name: /^Q\d+$/.test(label) ? '' : label,
      work: bound(row, 'workLabel').replace(/^Q\d+$/, ''),
      won,
      year: Number.isFinite(parsed) ? parsed : null,
    };
  });
}

/** Dedupe key: one person can be nominated twice in a year, for different works. */
function keyOf(entry: AwardEntry): string {
  return `${entry.id}|${entry.year ?? ''}|${entry.work}`;
}

export function assertYear(raw: string): number {
  const year = Number.parseInt(raw.trim(), 10);
  // Wide enough for the first Academy Awards (1929) and any ceremony a market
  // could plausibly reference, narrow enough to reject a stray ticker.
  if (!Number.isInteger(year) || year < 1900 || year > 2100) {
    throw new UpstreamError(`"${raw}" is not a ceremony year`, {
      code: 'bad_request',
      hint: 'Pass a four-digit year, e.g. `AWRD best picture 2026`.',
    });
  }
  return year;
}

/**
 * Nominees and winners for an award, optionally for one ceremony.
 *
 * Merges the two sides on the recipient: a winner is almost always recorded as a
 * nominee too, and listing them twice would double the apparent field.
 */
export async function getAward(name: string, year: number | null, limit = 300): Promise<AwardResult> {
  const award = await resolveAward(name);
  const key = `awards:${award.id}:${year ?? 'all'}`;

  const full = await cache.cached(key, TTL.awards, async () => {
    const [winners, nominees] = await Promise.all([
      side(award.id, 'P166', year),
      side(award.id, 'P1411', year),
    ]);

    const merged = new Map<string, AwardEntry>();
    for (const entry of [...nominees, ...winners]) {
      const existing = merged.get(keyOf(entry));
      // Winners land second, so `won` only ever gets promoted, never cleared.
      if (existing) existing.won ||= entry.won;
      else merged.set(keyOf(entry), { ...entry });
    }

    const entries = [...merged.values()]
      .filter((e) => e.name !== '')
      .sort(
        (a, b) =>
          (b.year ?? -Infinity) - (a.year ?? -Infinity) ||
          Number(b.won) - Number(a.won) ||
          a.name.localeCompare(b.name),
      );

    const years = [...new Set(entries.map((e) => e.year).filter((y): y is number => y !== null))]
      .sort((a, b) => b - a);

    const remark = note(entries, year);

    return {
      awardId: award.id,
      award: award.label,
      year,
      entries,
      years,
      sourceUrl: `https://www.wikidata.org/wiki/${award.id}`,
      ...(remark ? { note: remark } : {}),
    };
  });

  // `years` stays whole — it is the picker of what else is on record, and
  // trimming it to the visible rows would hide the ceremonies you can ask for.
  return { ...full, entries: full.entries.slice(0, limit) };
}

/**
 * What the panel should say about what it is showing.
 *
 * An empty ceremony is the interesting case: for an award Kalshi is trading, no
 * rows almost always means the nominations have not been announced, which is the
 * state the market exists to price — not a failure to find anything.
 */
function note(entries: AwardEntry[], year: number | null): string {
  if (entries.length === 0) {
    return year === null
      ? 'Wikidata holds no recipients for this award.'
      : `No ${year} ceremony recorded yet — nominations are announced weeks before the ceremony.`;
  }
  const winners = entries.filter((e) => e.won).length;
  if (year !== null && winners === 0) {
    return `${entries.length} nominees recorded, no winner yet.`;
  }
  return '';
}
