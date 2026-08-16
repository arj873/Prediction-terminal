/**
 * Lining one broker's series up against the others'.
 *
 * The terminal quotes three exchanges that list many of the same questions and
 * agree on no identifier for any of them. This module builds, per venue, an
 * index of open series, then pairs those indexes up — by a curated table where
 * one exists and by {@link scoreSeries} everywhere else — so `Fed decision in
 * Oct 2026?` can be read at all three prices at once.
 *
 * Two rules keep it honest:
 *
 *   A curated link is checked, not asserted. Every entry names live series
 *   identifiers, and a leg that is not in today's catalogue is dropped rather
 *   than shown — a table that goes stale degrades to a text match instead of
 *   quoting a market that no longer exists.
 *
 *   Every match carries its confidence and its reason. `linked` means the pair
 *   is stated in the table; anything else is the matcher's reading of two
 *   titles, labelled as such, with the terms it matched on.
 */

import type {
  CompareEventLeg,
  CompareLeg,
  CompareResponse,
  CompareRow,
  LinkedSeries,
  LinkedSeriesResponse,
  Market,
  MatchConfidence,
  SeriesLeg,
  Venue,
  VenueEvent,
} from '../../shared/types.js';
import { VENUE_IDS, venueInfo } from '../../shared/venue.js';
import {
  MATCH_FLOOR,
  pairLabels,
  scoreEvent,
  scoreSeries,
  tokenise,
  type MatchScore,
  type SeriesDescriptor,
} from '../../shared/match.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError } from '../lib/http.js';
import { sumOrNull } from './corpus.js';
import { sourceFor } from './venues.js';

/* ------------------------------------------------------------------ index */

interface IndexedSeries {
  venue: Venue;
  seriesTicker: string;
  title: string;
  events: VenueEvent[];
  markets: number;
  volume24h: number | null;
  closeTime: string;
  sampleEvent: string;
  descriptor: SeriesDescriptor;
  tokens: Set<string>;
}

interface VenueIndex {
  venue: Venue;
  series: IndexedSeries[];
  byTicker: Map<string, IndexedSeries>;
  /** Token → series carrying it, for blocking. */
  byToken: Map<string, IndexedSeries[]>;
  builtAt: number;
  truncated: boolean;
}

/**
 * A token in more than this share of a venue's series tells us nothing and
 * would drag every candidate into every comparison. `sports` is one of these.
 */
const COMMON_TOKEN_SHARE = 0.05;

function buildIndex(venue: Venue, events: VenueEvent[], truncated: boolean): VenueIndex {
  const grouped = new Map<string, VenueEvent[]>();
  for (const event of events) {
    const key = event.seriesTicker || event.eventTicker;
    if (!key) continue;
    const bucket = grouped.get(key);
    if (bucket) bucket.push(event);
    else grouped.set(key, [event]);
  }

  const series: IndexedSeries[] = [];
  for (const [seriesTicker, group] of grouped) {
    // The busiest event stands for the series: it is the one with a live book,
    // and its title is the one worth matching on.
    const sorted = [...group].sort(
      (a, b) =>
        (sumOrNull(b.markets, (m) => m.volume24h) ?? 0) -
        (sumOrNull(a.markets, (m) => m.volume24h) ?? 0),
    );
    const lead = sorted[0]!;
    const markets = group.reduce((sum, e) => sum + e.markets.length, 0);
    const closes = group
      .flatMap((e) => e.markets.map((m) => m.closeTime))
      .filter(Boolean)
      .sort();

    const descriptor: SeriesDescriptor = {
      id: seriesTicker,
      title: lead.title,
      context: `${lead.subTitle} ${lead.category}`,
    };

    series.push({
      venue,
      seriesTicker,
      title: lead.title,
      events: sorted,
      markets,
      volume24h: sumOrNull(
        group.flatMap((e) => e.markets),
        (m) => m.volume24h,
      ),
      closeTime: closes[0] ?? '',
      sampleEvent: lead.eventTicker,
      descriptor,
      tokens: new Set([...tokenise(`${descriptor.title} ${descriptor.context ?? ''}`)]),
    });
  }

  const byTicker = new Map(series.map((s) => [s.seriesTicker.toLowerCase(), s]));

  const counts = new Map<string, number>();
  for (const s of series) for (const token of s.tokens) counts.set(token, (counts.get(token) ?? 0) + 1);

  const ceiling = Math.max(4, Math.floor(series.length * COMMON_TOKEN_SHARE));
  const byToken = new Map<string, IndexedSeries[]>();
  for (const s of series) {
    for (const token of s.tokens) {
      if ((counts.get(token) ?? 0) > ceiling) continue;
      const bucket = byToken.get(token);
      if (bucket) bucket.push(s);
      else byToken.set(token, [s]);
    }
  }

  return { venue, series, byTicker, byToken, builtAt: Date.now(), truncated };
}

/** Every venue's index, with the ones that failed named rather than hidden. */
interface Indexes {
  ok: VenueIndex[];
  unavailable: { venue: Venue; error: string }[];
  builtAt: number;
}

async function indexes(): Promise<Indexes> {
  return cache.cached('xvenue:indexes', TTL.catalogue, async () => {
    const settled = await Promise.allSettled(
      VENUE_IDS.map(async (venue) => {
        const snapshot = await sourceFor(venue).corpusSnapshot();
        return buildIndex(venue, snapshot.events, snapshot.truncated);
      }),
    );

    const ok: VenueIndex[] = [];
    const unavailable: { venue: Venue; error: string }[] = [];

    settled.forEach((result, i) => {
      const venue = VENUE_IDS[i]!;
      if (result.status === 'fulfilled') ok.push(result.value);
      else {
        unavailable.push({
          venue,
          error: result.reason instanceof Error ? result.reason.message : String(result.reason),
        });
      }
    });

    return { ok, unavailable, builtAt: Date.now() };
  });
}

/* --------------------------------------------------------- curated links */

/**
 * Series the terminal states are the same question, rather than inferring it.
 *
 * Each entry was read off all three live catalogues, and each is re-checked
 * against them on every request — a leg whose identifier no longer exists is
 * dropped, so this table can go stale without ever producing a wrong quote.
 *
 * It exists for the cases the matcher cannot reach on wording alone: nothing in
 * "Fed decision in Oct 2026?" and Polymarket's series slug `fomc` shares a
 * single token, and a temperature series named `KXHIGHNY` against
 * `weather-daily-high-nyc` needs to know that NY and NYC are one city before
 * either title helps. Everything the matcher *can* reach is left to it, so this
 * stays short enough to keep true.
 */
interface CuratedLink {
  key: string;
  title: string;
  legs: Partial<Record<Venue, string>>;
}

const CURATED_LINKS: CuratedLink[] = [
  {
    key: 'fed-decision',
    title: 'Federal Reserve rate decision',
    legs: { kalshi: 'KXFEDDECISION', polymarket: 'fomc', 'polymarket-us': 'usfed-fomc' },
  },
  {
    key: 'fed-rate-range',
    title: 'Fed funds target range',
    legs: { kalshi: 'KXFED', polymarket: 'fed-interest-rates' },
  },
  {
    key: 'cpi-yoy',
    title: 'US CPI, year over year',
    legs: { kalshi: 'KXCPIYOY', 'polymarket-us': 'cpi' },
  },
  {
    key: 'us-midterms-house',
    title: 'US House control after the midterms',
    legs: { kalshi: 'KXHOUSE', 'polymarket-us': 'usho-midterms' },
  },
  {
    key: 'us-midterms-senate',
    title: 'US Senate control after the midterms',
    legs: { kalshi: 'KXSENATE', 'polymarket-us': 'usse-midterms' },
  },
  {
    key: 'nyc-high-temperature',
    title: 'Daily high temperature, New York City',
    legs: { kalshi: 'KXHIGHNY', 'polymarket-us': 'weather-daily-high-nyc' },
  },
  {
    key: 'chicago-high-temperature',
    title: 'Daily high temperature, Chicago',
    legs: { kalshi: 'KXHIGHCHI', 'polymarket-us': 'weather-daily-high-chicago' },
  },
  {
    key: 'best-picture',
    title: 'Academy Award for Best Picture',
    legs: { kalshi: 'KXOSCARPIC', 'polymarket-us': 'oscars-2026' },
  },
  {
    key: 'world-series',
    title: 'World Series champion',
    legs: { kalshi: 'KXWORLDSERIES', 'polymarket-us': 'mlb-2026' },
  },
  {
    key: 'super-bowl',
    title: 'Super Bowl champion',
    legs: { kalshi: 'KXPROFOOTBALLCHAMP', 'polymarket-us': 'nfl-2026' },
  },
];

/* ---------------------------------------------------------------- linking */

function legOf(series: IndexedSeries): SeriesLeg {
  return {
    venue: series.venue,
    seriesTicker: series.seriesTicker,
    title: series.title,
    events: series.events.length,
    markets: series.markets,
    volume24h: series.volume24h,
    closeTime: series.closeTime,
    sampleEvent: series.sampleEvent,
  };
}

/** Busiest leg first, with venues that publish no volume ordered after. */
function byActivity(a: SeriesLeg, b: SeriesLeg): number {
  return (b.volume24h ?? -1) - (a.volume24h ?? -1) || b.markets - a.markets;
}

/**
 * Every candidate pairing for `anchor` at `other`, above the floor.
 *
 * Candidates come from the token index rather than the whole catalogue — with
 * thousands of series per venue, scoring every pair would mean millions of
 * comparisons for one board. A series sharing no distinguishing token with the
 * anchor cannot clear the floor anyway.
 */
function candidatesFor(
  anchor: IndexedSeries,
  other: VenueIndex,
): { series: IndexedSeries; match: MatchScore }[] {
  const candidates = new Set<IndexedSeries>();
  for (const token of anchor.tokens) {
    for (const candidate of other.byToken.get(token) ?? []) candidates.add(candidate);
  }

  const scored: { series: IndexedSeries; match: MatchScore }[] = [];
  for (const candidate of candidates) {
    const match = scoreSeries(anchor.descriptor, candidate.descriptor);
    if (match.score >= MATCH_FLOOR) scored.push({ series: candidate, match });
  }
  return scored;
}

/**
 * The single best counterpart for one series — used when comparing one event,
 * where there is no field of other anchors to weigh it against.
 */
function bestMatch(
  anchor: IndexedSeries,
  other: VenueIndex,
): { series: IndexedSeries; match: MatchScore } | null {
  let best: { series: IndexedSeries; match: MatchScore } | null = null;
  for (const candidate of candidatesFor(anchor, other)) {
    if (!best || candidate.match.score > best.match.score) best = candidate;
  }
  return best;
}

/**
 * Assign one venue's series to another's, globally rather than one at a time.
 *
 * Taking each anchor's own favourite in turn is not good enough where a family
 * of sibling series all resemble each other. The Emmys list twenty categories
 * that differ by three words — comedy or drama, lead or supporting, actor or
 * actress — and every one of them scores respectably against every other. Asked
 * in isolation, "Outstanding Lead Actor in a Comedy Series" will happily take
 * whichever sibling happens to score highest, leaving the sibling that was
 * actually its twin to be claimed by someone else.
 *
 * So every candidate pairing is scored first, then assigned best-first with
 * both sides struck off as they are taken — the discipline {@link pairLabels}
 * already applies to the rungs of one ladder, applied here to the catalogue.
 */
function assignPairs(
  anchors: IndexedSeries[],
  other: VenueIndex,
  taken: Set<string>,
): Map<IndexedSeries, { series: IndexedSeries; match: MatchScore }> {
  const all: { anchor: IndexedSeries; series: IndexedSeries; match: MatchScore }[] = [];

  for (const anchor of anchors) {
    for (const candidate of candidatesFor(anchor, other)) {
      if (taken.has(`${candidate.series.venue}:${candidate.series.seriesTicker}`)) continue;
      all.push({ anchor, ...candidate });
    }
  }

  all.sort((x, y) => y.match.score - x.match.score);

  const assigned = new Map<IndexedSeries, { series: IndexedSeries; match: MatchScore }>();
  const usedCandidates = new Set<IndexedSeries>();

  for (const pair of all) {
    if (assigned.has(pair.anchor) || usedCandidates.has(pair.series)) continue;
    assigned.set(pair.anchor, { series: pair.series, match: pair.match });
    usedCandidates.add(pair.series);
  }

  return assigned;
}

/** The weakest confidence among a set of matches — what the group is worth. */
function weakest(confidences: MatchConfidence[]): MatchConfidence {
  const order: MatchConfidence[] = ['linked', 'strong', 'likely', 'weak'];
  return confidences.reduce(
    (worst, c) => (order.indexOf(c) > order.indexOf(worst) ? c : worst),
    'linked' as MatchConfidence,
  );
}

function matchesQuery(series: LinkedSeries, terms: string[]): boolean {
  if (terms.length === 0) return true;
  const haystack = [
    series.key,
    series.title,
    ...series.legs.map((l) => `${l.seriesTicker} ${l.title}`),
  ]
    .join(' ')
    .toLowerCase();
  return terms.every((term) => haystack.includes(term));
}

/**
 * Series that more than one broker lists.
 *
 * Curated links are resolved first and claim their legs; everything left is
 * matched on wording, anchored at the venue with the most series so the pass
 * covers the widest catalogue.
 */
export async function linkedSeries(query = '', limit = 40): Promise<LinkedSeriesResponse> {
  const { ok, unavailable, builtAt } = await indexes();
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);

  const scanned: Record<string, number> = {};
  for (const index of ok) scanned[index.venue] = index.series.length;

  const claimed = new Set<string>();
  const claim = (series: IndexedSeries): string => `${series.venue}:${series.seriesTicker}`;
  const found: LinkedSeries[] = [];

  // ---- curated -----------------------------------------------------------
  for (const link of CURATED_LINKS) {
    const legs: SeriesLeg[] = [];
    for (const index of ok) {
      const wanted = link.legs[index.venue];
      if (!wanted) continue;
      const series = index.byTicker.get(wanted.toLowerCase());
      if (!series) continue;
      legs.push(legOf(series));
      claimed.add(claim(series));
    }
    if (legs.length < 2) continue;

    found.push({
      key: link.key,
      title: link.title,
      legs: legs.sort(byActivity),
      confidence: 'linked',
      reason: 'curated link, checked against both catalogues',
      score: 1,
    });
  }

  // ---- matched -----------------------------------------------------------
  // Every venue against every other, not each against one anchor. Anchoring on
  // the largest catalogue made any question the anchor does not list invisible:
  // Polymarket International and Polymarket US both run all 435 House districts
  // under word-for-word identical titles, and the 46 of them Kalshi does not
  // list were unreachable — not mis-scored, never compared.
  const links: {
    a: IndexedSeries;
    b: IndexedSeries;
    match: MatchScore;
  }[] = [];

  for (let i = 0; i < ok.length; i++) {
    for (let j = i + 1; j < ok.length; j++) {
      const left = ok[i]!;
      const right = ok[j]!;
      const free = left.series.filter((s) => !claimed.has(claim(s)));

      for (const [anchor, best] of assignPairs(free, right, claimed)) {
        links.push({ a: anchor, b: best.series, match: best.match });
      }
    }
  }

  // Strongest links first, so a doubtful pairing can never pre-empt a confident
  // one when the two would land in the same group.
  links.sort((x, y) => y.match.score - x.match.score);

  const groups = new Map<IndexedSeries, IndexedSeries[]>();
  const groupOf = new Map<IndexedSeries, IndexedSeries[]>();
  const evidence = new Map<IndexedSeries[], MatchScore[]>();

  for (const link of links) {
    const left = groupOf.get(link.a);
    const right = groupOf.get(link.b);
    if (left && left === right) continue;

    const merged = [...(left ?? [link.a]), ...(right ?? [link.b])];

    // One series per venue per group. Two Kalshi series can both link to the
    // same question at different venues without being the same question, and
    // merging them would put two prices in one column.
    const venues = new Set(merged.map((s) => s.venue));
    if (venues.size !== merged.length) continue;

    for (const member of merged) groupOf.set(member, merged);
    if (left) groups.delete(left[0]!);
    if (right) groups.delete(right[0]!);
    groups.set(merged[0]!, merged);

    evidence.set(merged, [
      ...(left ? (evidence.get(left) ?? []) : []),
      ...(right ? (evidence.get(right) ?? []) : []),
      link.match,
    ]);
  }

  for (const members of groups.values()) {
    if (members.length < 2) continue;

    const matches = evidence.get(members) ?? [];
    // A group is worth what its weakest pairing is worth, not its best: two
    // solid legs and a doubtful third is a doubtful board.
    const score = matches.reduce((worst, m) => Math.min(worst, m.score), 1);

    // The busiest member names the group, since its title is the one with a
    // live book behind it.
    const lead = [...members].sort((a, b) => (b.volume24h ?? -1) - (a.volume24h ?? -1))[0]!;

    found.push({
      key: lead.seriesTicker.toLowerCase(),
      title: lead.title,
      legs: members.map(legOf).sort(byActivity),
      confidence: weakest(matches.map((m) => m.confidence)),
      reason: matches.map((m) => m.reason).join(' · '),
      score,
    });
  }

  const series = found
    .filter((s) => matchesQuery(s, terms))
    .sort(
      (a, b) =>
        b.legs.length - a.legs.length ||
        b.score - a.score ||
        (b.legs[0]?.volume24h ?? -1) - (a.legs[0]?.volume24h ?? -1),
    )
    .slice(0, limit);

  return {
    query,
    series,
    scanned,
    unavailable,
    snapshotAgeSeconds: Math.round((Date.now() - builtAt) / 1000),
  };
}

/* --------------------------------------------------------------- compare */

/** Locate an event in the indexes, wherever it is listed. */
async function findEvent(
  eventTicker: string,
  venue?: Venue,
): Promise<{ index: VenueIndex; series: IndexedSeries; event: VenueEvent }> {
  const { ok } = await indexes();
  const wanted = eventTicker.toLowerCase();

  for (const index of ok) {
    if (venue && index.venue !== venue) continue;
    for (const series of index.series) {
      const event = series.events.find((e) => e.eventTicker.toLowerCase() === wanted);
      if (event) return { index, series, event };
    }
  }

  throw new UpstreamError(`No open event ${eventTicker} in any venue's catalogue`, {
    code: 'not_found',
    hint: 'Cross-venue comparison works on open events. Use `XV <words>` to find one.',
  });
}

/**
 * The counterpart event at another venue.
 *
 * Two steps, because a series match and an expiry match are different
 * questions: find the series first (wording, dates ignored), then the event
 * within it whose numbers and close time line up (dates decisive). Skipping the
 * first step would let `Fed decision in Oct 2026?` match an unrelated series
 * that happens to mention October.
 */
function counterpart(
  anchorSeries: IndexedSeries,
  anchorEvent: VenueEvent,
  other: VenueIndex,
): { event: VenueEvent; match: MatchScore } | null {
  const curated = CURATED_LINKS.find(
    (l) => l.legs[anchorSeries.venue]?.toLowerCase() === anchorSeries.seriesTicker.toLowerCase(),
  );
  const curatedTicker = curated?.legs[other.venue];

  const series = curatedTicker
    ? other.byTicker.get(curatedTicker.toLowerCase())
    : bestMatch(anchorSeries, other)?.series;
  if (!series) return null;

  const anchorClose = Date.parse(anchorEvent.markets[0]?.closeTime ?? '');

  let best: { event: VenueEvent; match: MatchScore } | null = null;
  for (const event of series.events) {
    const match = scoreEvent(
      { id: anchorEvent.eventTicker, title: anchorEvent.title },
      { id: event.eventTicker, title: event.title },
    );

    // Titles alone rarely separate one expiry from the next — Polymarket US
    // words October's and December's Fed events identically — so closeness of
    // the close time breaks the tie.
    const close = Date.parse(event.markets[0]?.closeTime ?? '');
    const days =
      Number.isFinite(anchorClose) && Number.isFinite(close)
        ? Math.abs(anchorClose - close) / 86_400_000
        : Number.POSITIVE_INFINITY;
    const proximity = Number.isFinite(days) ? Math.max(0, 1 - days / 30) : 0;

    const combined = match.score * 0.7 + proximity * 0.3;
    if (combined < MATCH_FLOOR) continue;
    if (!best || combined > best.match.score) {
      best = {
        event,
        match: {
          ...match,
          score: combined,
          reason: Number.isFinite(days)
            ? `${match.reason}; closes ${days < 1 ? 'the same day' : `${Math.round(days)}d apart`}`
            : match.reason,
        },
      };
    }
  }

  return best;
}

function compareLeg(market: Market, eventTicker: string): CompareLeg {
  return {
    venue: market.venue,
    ticker: market.ticker,
    eventTicker,
    yesBid: market.yesBid,
    yesAsk: market.yesAsk,
    mid: market.mid,
    volume24h: market.volume24h,
  };
}

/**
 * The gap between brokers on one outcome.
 *
 * `divergence` is what the two books think, spread apart. `edge` is what could
 * actually be traded: the cheapest ask anywhere against the richest bid
 * anywhere, positive only when one venue's offer is below another's bid. They
 * differ by the width of the spreads, which is precisely the part that is not
 * profit.
 */
function summarise(legs: CompareLeg[]): Pick<CompareRow, 'divergence' | 'edge' | 'edgeVenues'> {
  const mids = legs.map((l) => l.mid).filter((m): m is number => m !== null);
  const divergence =
    mids.length >= 2 ? Math.round((Math.max(...mids) - Math.min(...mids)) * 10_000) / 10_000 : null;

  let edge: number | null = null;
  let edgeVenues: Venue[] = [];

  for (const buy of legs) {
    for (const sell of legs) {
      if (buy.venue === sell.venue) continue;
      if (buy.yesAsk === null || sell.yesBid === null) continue;
      const gap = Math.round((sell.yesBid - buy.yesAsk) * 10_000) / 10_000;
      if (edge === null || gap > edge) {
        edge = gap;
        edgeVenues = [buy.venue, sell.venue];
      }
    }
  }

  return { divergence, edge, edgeVenues };
}

/**
 * One question, quoted at every broker that lists it.
 *
 * Quotes are re-fetched live rather than read from the snapshot: the catalogue
 * is up to fifteen minutes old, which is fine for finding a market and useless
 * for pricing one.
 */
export async function compare(eventTicker: string, venue?: Venue): Promise<CompareResponse> {
  const { index, series, event: anchorStale } = await findEvent(eventTicker, venue);
  const { ok } = await indexes();

  const partners: { index: VenueIndex; event: VenueEvent; match: MatchScore }[] = [];
  for (const other of ok) {
    if (other.venue === index.venue) continue;
    const found = counterpart(series, anchorStale, other);
    if (found) partners.push({ index: other, event: found.event, match: found.match });
  }

  // Live quotes for the anchor and every partner. One failing venue must not
  // take the whole comparison down — it is dropped and the rest still print.
  const live = await Promise.allSettled([
    sourceFor(index.venue).getEvent(anchorStale.eventTicker),
    ...partners.map((p) => sourceFor(p.index.venue).getEvent(p.event.eventTicker)),
  ]);

  const anchor = live[0]?.status === 'fulfilled' ? live[0].value : anchorStale;

  const events: CompareEventLeg[] = [
    {
      venue: anchor.venue,
      eventTicker: anchor.eventTicker,
      title: anchor.title,
      closeTime: anchor.markets[0]?.closeTime ?? '',
      confidence: 'linked',
      score: 1,
      reason: 'anchor',
    },
  ];

  const ladders: VenueEvent[] = [anchor];

  partners.forEach((partner, i) => {
    const result = live[i + 1];
    const event = result?.status === 'fulfilled' ? result.value : partner.event;
    ladders.push(event);
    events.push({
      venue: event.venue,
      eventTicker: event.eventTicker,
      title: event.title,
      closeTime: event.markets[0]?.closeTime ?? '',
      confidence: partner.match.confidence,
      score: Math.round(partner.match.score * 100) / 100,
      reason: partner.match.reason,
    });
  });

  // ---- rows --------------------------------------------------------------
  // The anchor's ladder sets the rows; each partner's contracts are paired onto
  // it, and whatever is left over is reported rather than dropped.
  const rows: CompareRow[] = anchor.markets.map((market) => ({
    label: market.yesSubTitle || market.title,
    legs: [compareLeg(market, anchor.eventTicker)],
    divergence: null,
    edge: null,
    edgeVenues: [],
  }));

  const unmatched: CompareResponse['unmatched'] = [];

  for (const ladder of ladders.slice(1)) {
    const theirs = ladder.markets.map((m) => ({ label: m.yesSubTitle || m.title, market: m }));
    const paired = pairLabels(
      rows.map((r) => ({ label: r.label, row: r })),
      theirs,
    );

    const used = new Set(paired.map((p) => p.right.market));
    for (const pair of paired) {
      pair.left.row.legs.push(compareLeg(pair.right.market, ladder.eventTicker));
    }
    for (const leg of theirs) {
      if (!used.has(leg.market)) {
        unmatched.push({ venue: ladder.venue, label: leg.label, ticker: leg.market.ticker });
      }
    }
  }

  for (const row of rows) Object.assign(row, summarise(row.legs));

  return {
    title: anchor.title,
    events,
    // A row only one venue quotes is not a comparison; it stays out of the
    // table and its counterpart-less state is already visible in `events`.
    rows: rows.filter((r) => r.legs.length > 1),
    unmatched,
  };
}

export function warmIndexes(): void {
  indexes()
    .then(({ ok, unavailable }) => {
      const shape = ok.map((i) => `${venueInfo(i.venue).code} ${i.series.length}`).join(', ');
      console.log(`[xvenue] series index warm: ${shape}`);
      for (const miss of unavailable) console.warn(`[xvenue] ${miss.venue} unavailable: ${miss.error}`);
    })
    .catch((err: unknown) => {
      console.warn('[xvenue] index warm failed:', err instanceof Error ? err.message : err);
    });
}
