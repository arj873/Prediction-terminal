/**
 * Deciding whether two brokers are listing the same question.
 *
 * Three exchanges word the same market three ways and name it three more:
 *
 *   Kalshi          KXFEDDECISION      "Fed decision in Oct 2026?"
 *   Polymarket      fomc               "Fed Decision in October?"
 *   Polymarket US   usfed-fomc         "Fed Decision in October"
 *
 * Nothing in any payload connects those. What connects them is the language,
 * and this module is the part of the terminal that reads it — kept pure and
 * free of any network so the judgement can be tested against fixed strings
 * rather than against whatever is listed today.
 *
 * The output is a score *and* the terms it came from. A cross-venue quote that
 * cannot say why it thinks two contracts are the same contract is worth less
 * than no quote at all, because a trader will act on it.
 */

import type { MatchConfidence } from './types.js';

/* ---------------------------------------------------------------- tokens */

/**
 * Words that appear in market titles without narrowing anything down.
 *
 * Question scaffolding ("will", "be", "the") and the exchanges' own filler
 * ("market", "winner", "outcome"). Dropping them stops two unrelated markets
 * from scoring on "will the … be".
 */
const STOPWORDS = new Set([
  'a', 'an', 'and', 'any', 'are', 'as', 'at', 'be', 'been', 'before', 'by', 'do', 'does',
  'for', 'from', 'get', 'has', 'have', 'how', 'in', 'is', 'it', 'many', 'much', 'of', 'on',
  'or', 'the', 'their', 'there', 'this', 'to', 'up', 'was', 'what', 'when', 'which', 'who',
  'will', 'with', 'market', 'markets', 'outcome', 'contract', 'event',
]);

/**
 * Terms the three venues use interchangeably, folded onto one word.
 *
 * Every entry is a pair actually observed across the three catalogues, not a
 * general-purpose thesaurus: `fomc` and `fed` name one committee, `btc` and
 * `bitcoin` one asset, `gop` and `republican` one party. Folding anything
 * looser would start matching questions that merely share a subject.
 */
const SYNONYMS: Record<string, string> = {
  fomc: 'fed',
  federal: 'fed',
  reserve: 'fed',
  fedfunds: 'fed',
  powell: 'powell',
  rates: 'rate',
  bps: 'bp',
  basis: 'bp',
  points: 'bp',
  cut: 'decrease',
  cuts: 'decrease',
  lower: 'decrease',
  hike: 'increase',
  hikes: 'increase',
  raise: 'increase',
  unchanged: 'nochange',
  hold: 'nochange',
  btc: 'bitcoin',
  xbt: 'bitcoin',
  eth: 'ethereum',
  sol: 'solana',
  doge: 'dogecoin',
  gop: 'republican',
  republicans: 'republican',
  dem: 'democrat',
  dems: 'democrat',
  democrats: 'democrat',
  democratic: 'democrat',
  potus: 'president',
  presidential: 'president',
  inflation: 'cpi',
  unemployment: 'jobless',
  nyc: 'newyork',
  ny: 'newyork',
  la: 'losangeles',
  sf: 'sanfrancisco',
  temp: 'temperature',
  temperatures: 'temperature',
  high: 'high',
  highest: 'high',
  academy: 'oscar',
  oscars: 'oscar',
  awards: 'award',
  champion: 'champ',
  championship: 'champ',
  champions: 'champ',
  winner: 'win',
  wins: 'win',
  winners: 'win',
  season: 'season',
  midterm: 'midterms',
  senate: 'senate',
  house: 'house',
  governor: 'governor',
  gubernatorial: 'governor',
  election: 'election',
  elections: 'election',
};

/** Month names, folded to their number so `Oct` and `October` agree. */
const MONTHS: Record<string, string> = {
  jan: '1', january: '1', feb: '2', february: '2', mar: '3', march: '3',
  apr: '4', april: '4', may: '5', jun: '6', june: '6', jul: '7', july: '7',
  aug: '8', august: '8', sep: '9', sept: '9', september: '9', oct: '10', october: '10',
  nov: '11', november: '11', dec: '12', december: '12',
};

/**
 * Multi-word phrases the venues use for one idea, folded before tokenising.
 *
 * These cannot go in {@link SYNONYMS}, which maps single words: Kalshi's `Fed
 * maintains rate`, Polymarket's `No change` and Polymarket US's `Unchanged` are
 * the same rung of the same ladder, and no word-for-word mapping connects them.
 */
const PHRASES: [RegExp, string][] = [
  [/\bno change\b/g, 'nochange'],
  [/\bunchanged\b/g, 'nochange'],
  [/\bmaintains? (?:the )?(?:rate|rates)\b/g, 'nochange'],
  [/\bbasis points?\b/g, 'bp'],
  [/\bbps\b/g, 'bp'],
  // A comparator is the difference between two rungs of a ladder, so `>25bps`
  // and `50+ bps` keep the fact that they are open-ended.
  [/[>≥+]|\bor (?:above|more|higher|greater)\b|\bat least\b/g, ' over '],
  [/[<≤]|\bor (?:below|less|lower|fewer)\b|\bat most\b/g, ' under '],
];

/** Lower-case, strip accents and punctuation, collapse whitespace. */
export function normalise(text: string): string {
  let out = text
    .normalize('NFKD')
    .replace(/[̀-ͯ]/g, '')
    .toLowerCase()
    .replace(/[^a-z0-9.%$+<>≥≤-]+/g, ' ')
    // A unit welded to its number is one token to a string comparison and two
    // to a reader: `25bps` has to meet `25 bps` somewhere. Split before folding
    // phrases, or `bps` in `25bps` is still inside a word when `\bbps\b` looks
    // for it.
    .replace(/(\d)([a-z])/g, '$1 $2')
    .replace(/([a-z])(\d)/g, '$1 $2');

  for (const [pattern, replacement] of PHRASES) out = out.replace(pattern, replacement);

  return out.replace(/\s+/g, ' ').trim();
}

/**
 * A title's meaningful terms.
 *
 * Plurals are folded (`rates` → `rate`) and synonyms applied, so the three
 * venues' phrasings converge before they are compared. Months become numbers
 * because `Oct 2026` and `October` are the same expiry to a trader and two
 * unrelated strings to a set intersection.
 */
export function tokenise(text: string): string[] {
  const out: string[] = [];

  for (const word of normalise(text).split(' ')) {
    if (!word) continue;

    const bare = word.replace(/^[-.]+|[-.]+$/g, '');
    if (!bare || STOPWORDS.has(bare)) continue;

    const month = MONTHS[bare];
    if (month) {
      out.push(month);
      continue;
    }

    const singular =
      bare.length > 3 && bare.endsWith('s') && !bare.endsWith('ss') ? bare.slice(0, -1) : bare;

    out.push(SYNONYMS[bare] ?? SYNONYMS[singular] ?? singular);
  }

  return out;
}

/**
 * A venue identifier reduced to letters and digits.
 *
 * Kalshi's `KXFEDDECISION`, Polymarket's `fed-decision` and Polymarket US's
 * `usfed-fomc` are the same name under three conventions; flattening them lets
 * one be tested for containment in another without a word list that could
 * split `FEDDECISION` in the first place.
 */
export function identityKey(id: string): string {
  return id
    .toLowerCase()
    .replace(/^kx/, '')
    .replace(/^us(?=[a-z])/, '')
    .replace(/[^a-z0-9]/g, '')
    // A trailing season or year is part of the listing, not of the series.
    .replace(/(19|20)\d{2}$/, '');
}

/** Every number in a title, including years and strike levels. */
export function numbersIn(text: string): number[] {
  const found = normalise(text).match(/-?\d+(?:\.\d+)?/g) ?? [];
  return found.map(Number).filter((n) => Number.isFinite(n));
}

/**
 * The numbers that distinguish one *series* from another, which is every
 * number except the year.
 *
 * A series is named by its exemplar event, and that event's title carries a
 * date the series itself does not have: Kalshi's `Fed decision in Oct 2026?`
 * against Polymarket's `Fed Decision in September?` is one series seen at two
 * expiries. Everything else a title counts is part of its identity — `2nd
 * place` and `3rd place` are different questions, and so are `Best Picture` and
 * `Best Animated Feature`. Month *names* survive as words here, so only the
 * year has to be excluded.
 */
export function significantNumbers(text: string): number[] {
  return numbersIn(text).filter((n) => !(Number.isInteger(n) && n >= 1900 && n <= 2100));
}

/* ---------------------------------------------------------------- scoring */

function dice(a: Set<string>, b: Set<string>): number {
  if (a.size === 0 || b.size === 0) return 0;
  let shared = 0;
  for (const term of a) if (b.has(term)) shared++;
  return (2 * shared) / (a.size + b.size);
}

/** What a venue offers the matcher about one series. */
export interface SeriesDescriptor {
  /** The venue's own identifier: `KXFEDDECISION`, `fomc`, `usfed-fomc`. */
  id: string;
  /** The series' title, or a representative event's. */
  title: string;
  /** Anything else worth matching on — a category, a sample strike label. */
  context?: string;
}

export interface MatchScore {
  /** 0..1. */
  score: number;
  confidence: MatchConfidence;
  /** Terms both sides carried, in the order the first side stated them. */
  shared: string[];
  /** One line naming why, for the panel to print verbatim. */
  reason: string;
}

/** Where a text score stops being worth showing at all. */
export const MATCH_FLOOR = 0.34;

export function confidenceOf(score: number): MatchConfidence {
  if (score >= 0.8) return 'strong';
  if (score >= 0.6) return 'likely';
  return 'weak';
}

/**
 * How alike two listings read, before any arithmetic on the numbers in them.
 *
 * Titles carry the signal, so they carry the weight; the identifiers are a
 * corroborator, because when two exchanges independently arrive at
 * `fed-decision` and `feddecision` that is worth more than any single shared
 * word. What counts as a *disagreeing* number differs between a series and an
 * expiry, so that judgement belongs to the two callers below, and each applies
 * it exactly once.
 */
function similarity(a: SeriesDescriptor, b: SeriesDescriptor): MatchScore {
  const left = tokenise(`${a.title} ${a.context ?? ''}`);
  const right = tokenise(`${b.title} ${b.context ?? ''}`);
  const leftSet = new Set(left);
  const rightSet = new Set(right);

  const shared = left.filter((t, i) => rightSet.has(t) && left.indexOf(t) === i);
  let score = dice(leftSet, rightSet);

  const keyA = identityKey(a.id);
  const keyB = identityKey(b.id);
  const identical = keyA.length >= 4 && keyA === keyB;
  const contains =
    !identical &&
    keyA.length >= 5 &&
    keyB.length >= 5 &&
    (keyA.includes(keyB) || keyB.includes(keyA));

  if (identical) score = score * 0.6 + 0.4;
  else if (contains) score = score * 0.75 + 0.25;

  // Two titles that agree on one common word and nothing else are not a match,
  // however that word scored: "NFL Champion" and "NFL Rookie of the Year".
  // Only applied where there was room to share more — a two-word contract label
  // ("No change") has one word to offer, and offering it is not weak evidence.
  const roomToShare = leftSet.size + rightSet.size >= 5;
  if (shared.length < 2 && roomToShare && !identical && !contains) score *= 0.5;

  let reason = identical
    ? `both listed as "${keyA}"`
    : contains
      ? `identifiers overlap (${a.id} / ${b.id})`
      : shared.length
        ? `shared terms: ${shared.slice(0, 4).join(', ')}`
        : 'no shared terms';

  score = Math.min(1, score);
  return { score, confidence: confidenceOf(score), shared, reason };
}

/**
 * Reward agreeing numbers, punish disagreeing ones.
 *
 * Set overlap cannot see that the one word two titles differ on is the whole
 * question: "Big Brother season 28, 2nd place" and "…3rd place" share five
 * words out of six. Disagreement is halved rather than zeroed, because one side
 * may simply be counting something the other does not mention.
 */
function weighNumbers(base: MatchScore, left: number[], right: number[]): MatchScore {
  if (left.length === 0 || right.length === 0) return base;

  const rightSet = new Set(right);
  const leftSet = new Set(left);
  const onlyLeft = left.filter((n) => !rightSet.has(n));
  const onlyRight = right.filter((n) => !leftSet.has(n));
  const agreed = left.length - onlyLeft.length;

  // Every number on both sides accounted for: this is the same rung.
  if (agreed > 0 && onlyLeft.length === 0 && onlyRight.length === 0) {
    const score = Math.min(1, base.score * 0.85 + 0.15);
    return { ...base, score, confidence: confidenceOf(score) };
  }

  // Some agree and some do not, which is the shape of the near miss this whole
  // check exists for: "Big Brother Season 28, 2nd place" and "…3rd place" agree
  // on the season and differ on the only number that is the question. Treating
  // a partial hit as a hit is what lets those two through.
  const score = base.score * (agreed > 0 ? 0.65 : 0.5);
  return {
    ...base,
    score,
    confidence: confidenceOf(score),
    reason: `${base.reason}; numbers differ (${onlyLeft.slice(0, 3).join('/') || '—'} vs ${onlyRight.slice(0, 3).join('/') || '—'})`,
  };
}

/**
 * Score two venues' series against each other.
 *
 * Years are ignored, because a series recurs: `KXFEDDECISION` is the same
 * series whether its next event is October's or January's, and its exemplar
 * event's title says so. Every other number counts — a "2nd place" market and a
 * "3rd place" market are two questions however alike they read.
 */
export function scoreSeries(a: SeriesDescriptor, b: SeriesDescriptor): MatchScore {
  return weighNumbers(
    similarity(a, b),
    significantNumbers(a.title),
    significantNumbers(b.title),
  );
}

/**
 * Score two specific events, or two contracts — one expiry against another.
 *
 * Same reading as {@link scoreSeries}, except that here the year is decisive
 * too. October's Fed meeting and January's share every word in their titles, so
 * without counting the date the terminal would happily quote one against the
 * other.
 */
export function scoreEvent(a: SeriesDescriptor, b: SeriesDescriptor): MatchScore {
  return weighNumbers(similarity(a, b), numbersIn(a.title), numbersIn(b.title));
}

/**
 * Pair up two venues' contracts within one event.
 *
 * Greedy, best-first: the strongest pair is taken, both sides are struck off,
 * and the next strongest is considered. A ladder is a set of near-identical
 * labels ("25 bps decrease", "50+ bps decrease"), so pairing each label with
 * its own best partner independently would happily map two of one venue's rungs
 * onto one of the other's.
 */
export function pairLabels<A extends { label: string }, B extends { label: string }>(
  left: A[],
  right: B[],
  floor = MATCH_FLOOR,
): { left: A; right: B; match: MatchScore }[] {
  const candidates: { left: A; right: B; match: MatchScore }[] = [];

  for (const l of left) {
    for (const r of right) {
      const match = scoreEvent({ id: l.label, title: l.label }, { id: r.label, title: r.label });
      if (match.score >= floor) candidates.push({ left: l, right: r, match });
    }
  }

  candidates.sort((x, y) => y.match.score - x.match.score);

  const usedLeft = new Set<A>();
  const usedRight = new Set<B>();
  const paired: { left: A; right: B; match: MatchScore }[] = [];

  for (const candidate of candidates) {
    if (usedLeft.has(candidate.left) || usedRight.has(candidate.right)) continue;
    usedLeft.add(candidate.left);
    usedRight.add(candidate.right);
    paired.push(candidate);
  }

  return paired;
}
