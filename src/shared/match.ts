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
  // `city` is in half the place names on the board and distinguishes none of
  // them: Oklahoma City and Panama City differ by the *other* word.
  'city',
  // Clock furniture from Kalshi's dated titles ("on Aug 21, 2026 at 5pm EDT").
  'am', 'pm', 'et', 'edt', 'est', 'ct', 'cdt', 'cst', 'pt', 'pdt', 'pst', 'utc', 'gmt',
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
  // `federal` deliberately does NOT fold onto `fed`. It appears in "federal
  // crime", "federal government", "federal court" and a dozen other markets
  // that have nothing to do with the Federal Reserve, and folding it paired a
  // federal-charges market with the Fed's year-end target rate. "Federal
  // Reserve" still reaches `fed` through `reserve`.
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
  hype: 'hyperliquid',
  zec: 'zcash',
  // Index and commodity tickers. Kalshi titles by ticker, Polymarket spells the
  // instrument out and puts the ticker in parentheses.
  spx: 'sp500',
  inx: 'sp500',
  ndx: 'nasdaq100',
  gc: 'gold',
  cl: 'wti',
  // Both venues describe the same quantity with different verbs: Polymarket
  // asks what a price will *hit*, Kalshi how *high* it will get.
  hit: 'high',
  maximum: 'high',
  minimum: 'low',
  best: 'top',
  sptfy: 'spotify',
  gpu: 'nvidia',
  rental: 'hourly',
  wealth: 'networth',
  gop: 'republican',
  republicans: 'republican',
  dem: 'democrat',
  dems: 'democrat',
  democrats: 'democrat',
  democratic: 'democrat',
  potus: 'president',
  presidential: 'president',
  // Central bank tickers. Polymarket US names its rate books `boj`, `boe`,
  // `bcb`; Kalshi spells them out. Each side is folded onto one distinctive
  // token, so the bank is matched on its identity rather than on the words
  // "bank" and "decision", which every one of them shares.
  boj: 'bankofjapan',
  boe: 'bankofengland',
  boc: 'bankofcanada',
  bcb: 'bankofbrazil',
  bcbbrazil: 'bankofbrazil',
  boi: 'bankofisrael',
  bok: 'bankofkorea',
  cbr: 'bankofrussia',
  banxico: 'bankofmexico',
  ecb: 'ecb',
  rbnz: 'bankofnewzealand',
  rba: 'bankofaustralia',
  snb: 'bankofswitzerland',
  pboc: 'bankofchina',
  rbi: 'bankofindia',
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
  low: 'low',
  lowest: 'low',
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

/**
 * Month names, folded to a marked number so `Oct` and `October` agree.
 *
 * Marked rather than bare, because a month means different things to the two
 * callers: an *event* in October is not the same event as one in January, but a
 * *series* whose next event falls in October is the same series as one whose
 * next event falls in January. The `m` prefix keeps that distinction available
 * instead of collapsing it into an ordinary number.
 */
const MONTHS: Record<string, string> = {
  jan: 'm1', january: 'm1', feb: 'm2', february: 'm2', mar: 'm3', march: 'm3',
  apr: 'm4', april: 'm4', may: 'm5', jun: 'm6', june: 'm6', jul: 'm7', july: 'm7',
  aug: 'm8', august: 'm8', sep: 'm9', sept: 'm9', september: 'm9', oct: 'm10', october: 'm10',
  nov: 'm11', november: 'm11', dec: 'm12', december: 'm12',
};

const isMonth = (token: string): boolean => /^m(?:[1-9]|1[0-2])$/.test(token);

/**
 * The two tables above, as maps.
 *
 * A market title is arbitrary text, and looking arbitrary text up in an object
 * literal reaches the prototype: "F1 Constructors Champion" tokenises through
 * `constructor`, and `SYNONYMS['constructor']` is a function, not a synonym.
 * A `Map` has no such inheritance.
 */
const SYNONYM_LOOKUP = new Map(Object.entries(SYNONYMS));
const MONTH_LOOKUP = new Map(Object.entries(MONTHS));

/**
 * Multi-word phrases the venues use for one idea, folded before tokenising.
 *
 * These cannot go in {@link SYNONYMS}, which maps single words: Kalshi's `Fed
 * maintains rate`, Polymarket's `No change` and Polymarket US's `Unchanged` are
 * the same rung of the same ladder, and no word-for-word mapping connects them.
 */
const PHRASES: [RegExp, string][] = [
  // A central bank's name is its identity; the words around it are shared by
  // every other central bank on the board.
  [/\bbank of japan\b|\bboj\b/g, ' bankofjapan '],
  [/\bbank of england\b|\bboe\b/g, ' bankofengland '],
  [/\bbank of canada\b|\bboc\b/g, ' bankofcanada '],
  [/\b(?:central )?bank of brazil\b|\bbcb\b/g, ' bankofbrazil '],
  [/\bbank of israel\b|\bboi\b/g, ' bankofisrael '],
  [/\bbank of korea\b|\bbok\b/g, ' bankofkorea '],
  [/\bbank of russia\b|\bcbr\b/g, ' bankofrussia '],
  [/\bbank of mexico\b|\bbanxico\b/g, ' bankofmexico '],
  [/\breserve bank of new zealand\b|\brbnz\b/g, ' bankofnewzealand '],
  [/\breserve bank of australia\b|\brba\b/g, ' bankofaustralia '],
  [/\beuropean central bank\b|\becb\b/g, ' ecb '],
  // Weather states its direction, and that direction is the whole question. It
  // needs a token of its own: `high` alone also means "how high will BTC get",
  // and one word cannot guard both.
  [/\b(?:highest|high|max|maximum) temperature\b/g, ' hightemp '],
  [/\b(?:lowest|low|min|minimum) temperature\b/g, ' lowtemp '],
  [/\bgrand theft auto\b|\bgta\b/g, ' gta '],
  [/\ball[- ]time high\b/g, ' high '],
  [/\bnet worth\b|\bhow rich\b/g, ' networth '],
  [/\bcircuitbreaker\b/g, ' circuit breaker '],
  [/\bmarketwide\b/g, ' market wide '],
  // `sandp` is what PUNCTUATED_PHRASES leaves behind for `S&P`; folding it here
  // puts it past the letter/digit splitter, where `sp500` can survive intact
  // and meet the same token SYNONYMS folds `spx` and `inx` onto.
  [/\bsandp\s*500\b|\bsandp\b/g, ' sp500 '],
  [/\bnasdaq[- ]?100\b/g, ' nasdaq100 '],
  // A city named in full has to meet the same city named short. SYNONYMS maps
  // the abbreviations onto these tokens (`nyc` → `newyork`); without the
  // spelled-out side folded to match, `NYC` and `New York City` shared no term
  // at all — and `york` is a place qualifier, so the lopsided-qualifier penalty
  // then fired on top and drove the pair below the match floor.
  [/\bnew york city\b|\bnew york\b/g, ' newyork '],
  [/\blos angeles\b/g, ' losangeles '],
  [/\bsan francisco\b/g, ' sanfrancisco '],
  [/\bnew jersey\b/g, ' newjersey '],
  [/\bnew hampshire\b/g, ' newhampshire '],
  [/\bnew mexico\b/g, ' newmexico '],
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

/**
 * Phrases whose own punctuation is what identifies them.
 *
 * These have to fold before the strip below removes the very characters they
 * match on, and all three had been sitting after it — dead, silently. `s&p 500`
 * arrived as `s p 500`, so an S&P title could never meet the `sp500` that
 * `SYNONYMS` folds `spx` and `inx` onto; `up/down` arrived as `up down`, where
 * `up` is a stopword and only `down` survived; and Kalshi's live-strike suffix
 * `· $1,883.54 target` — the noise this rule exists to delete, and which
 * outweighs every content word in a short-interval title — arrived already
 * broken into pieces the pattern could not span.
 */
const PUNCTUATED_PHRASES: [RegExp, string][] = [
  // Spelled out rather than folded straight to `sp500`, because the letter/digit
  // split further down would only tear that back into `sp 500`. The all-letter
  // form survives it and is folded with the rest, after the splitter has run.
  [/\bs\s*&\s*p\b/g, ' sandp '],
  [/\bup\s*\/\s*down\b/g, ' up or down '],
  [/·?\s*\$[\d,.]+\s*target\b/g, ' '],
];

/** Lower-case, strip accents and punctuation, collapse whitespace. */
export function normalise(text: string): string {
  let out = text
    .normalize('NFKD')
    .replace(/[̀-ͯ]/g, '')
    .toLowerCase();

  for (const [pattern, replacement] of PUNCTUATED_PHRASES) out = out.replace(pattern, replacement);

  out = out
    // Keep a grouped number whole. The strip below turns every comma into a
    // space, so `$63,000` became the two tokens `$63` and `000` — and `000`
    // reads as the number zero. Two strikes thousands apart therefore both
    // reported the single number 0, which `compareNumbers` scored as perfect
    // agreement and *rewarded*.
    .replace(/(\d),(?=\d{3}(?:,|\b))/g, '$1')
    .replace(/[^a-z0-9.%$+<>≥≤]+/g, ' ')
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

    const month = MONTH_LOOKUP.get(bare);
    if (month) {
      out.push(month);
      continue;
    }

    const singular =
      bare.length > 3 && bare.endsWith('s') && !bare.endsWith('ss') ? bare.slice(0, -1) : bare;

    out.push(SYNONYM_LOOKUP.get(bare) ?? SYNONYM_LOOKUP.get(singular) ?? singular);
  }

  return out;
}

/**
 * A token that states a quantity.
 *
 * The currency mark and the percent sign are part of how a strike is written,
 * not part of the number: `$63000` and `3.75%` are quantities, and a matcher
 * that cannot read them cannot tell two rungs of a ladder apart. Requiring bare
 * digits meant every dollar strike and every rate strike was invisible to the
 * numeric comparison — so the Fed ladder, the case this module exists for,
 * was only ever compared as words.
 */
const isNumeric = (token: string): boolean => /^\$?-?\d+(?:\.\d+)?%?$/.test(token);

/** The quantity a numeric token states, with its notation removed. */
const numericValue = (token: string): number => Number(token.replace(/[$%]/g, ''));

/**
 * A title's terms with its numbers taken out.
 *
 * Dates dominate a title's token count and say nothing about what is being
 * asked: "Highest temperature in Oklahoma City on Aug 16, 2026?" is five parts
 * date and two parts question, so it scored 0.74 against Panama City. Numbers
 * are not discarded — {@link weighNumbers} reads them separately, where a
 * disagreement can be judged on its own terms instead of being averaged into a
 * bag of words.
 */
export function contentTokens(text: string): string[] {
  return tokenise(text).filter((token) => !isNumeric(token) && !isMonth(token));
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

/**
 * Every number in a title, including years and strike levels.
 *
 * Read off the *tokenised* form so a month named in words counts as its number:
 * `Oct 2026` and `October` have to agree on the month, and only tokenising
 * makes them comparable.
 */
export function numbersIn(text: string): number[] {
  return tokenise(text)
    .filter((token) => isNumeric(token) || isMonth(token))
    .map((token) => (isMonth(token) ? Number(token.slice(1)) : numericValue(token)));
}

const isYear = (n: number): boolean => Number.isInteger(n) && n >= 1900 && n <= 2100;

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
  return tokenise(text)
    .filter((token) => isNumeric(token) && !isYear(Number(token)))
    .map(Number);
}

/* ---------------------------------------------------------------- scoring */

/**
 * How much two token sets have in common, allowing for one being terser.
 *
 * Dice alone — `2|A∩B| / (|A|+|B|)` — punishes a short title for being short,
 * and the three venues are wildly asymmetric about length. Kalshi writes
 * "Bank of Japan rate decision in September" where Polymarket US writes "BoJ
 * Decision"; Kalshi writes "Emmy Winner: Outstanding Lead Actor in a Comedy
 * Series" where Polymarket US writes "Lead Actor, Comedy". Both pairs are
 * *identical questions* that Dice scores in the 0.5s purely on word count, and
 * that is where the terser side is usually the one carrying the meaning.
 *
 * The overlap coefficient — `|A∩B| / min(|A|,|B|)` — has the opposite flaw: it
 * reads any subset as a perfect match, so "NFL Champion" would score 1.0
 * against "NFL Champion Rookie of the Year". Blending the two keeps Dice's
 * scepticism about loose subsets while letting a genuinely terser title compete.
 */
function overlap(a: Set<string>, b: Set<string>): number {
  if (a.size === 0 || b.size === 0) return 0;

  let shared = 0;
  for (const term of a) if (b.has(term)) shared++;

  const coefficient = shared / Math.min(a.size, b.size);
  const dice = (2 * shared) / (a.size + b.size);
  return 0.6 * dice + 0.4 * coefficient;
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

/**
 * Words that carve a family of markets into its members.
 *
 * These are the opposite of a shared term: when one side says `supporting` and
 * the other does not, the six words they agree on stop mattering, because
 * "Best Actor" and "Best Supporting Actor" are two awards with two winners.
 * Set overlap cannot see that — one word out of six barely moves a Dice
 * coefficient — so presence on exactly one side is scored in its own right.
 *
 * Only tokens whose absence is *meaningful* belong here. `high` qualifies
 * because every temperature market on all three venues states whether it is the
 * day's high or its low, so a title that omits it is not a title about
 * temperature at all. A word that one venue simply happens to leave implicit
 * would cause a correct pair to be rejected, and does not belong.
 *
 * Written in post-{@link tokenise} form: `highest` arrives here as `high`.
 */
const QUALIFIERS: readonly string[] = [
  // Award categories, where the qualifier *is* the category. The Emmys alone
  // run twenty of these, differing by three words and nothing else.
  'supporting',
  'lead',
  'guest',
  'animated',
  'documentary',
  'adapted',
  'original',
  'comedy',
  'drama',
  'variety',
  'anthology',
  'actor',
  'actress',
  // Temperature markets, which always state their direction.
  'hightemp',
  'lowtemp',
];

/**
 * Places and offices, which are the whole question in an election market.
 *
 * "South Carolina Senate winner?" and "South Dakota Senate election winner"
 * share three words out of four; so do "Minnesota Senate winner?" and
 * "Minnesota Governor winner". A bag of words cannot see that `carolina` and
 * `dakota` — or `senate` and `governor` — are the entire distinction, and the
 * board is nine-tenths election markets, so it gets these wrong at scale.
 *
 * Only the distinctive word of each state is listed: `carolina` tells North
 * from South once `north`/`south` are themselves discriminating.
 */
const PLACES_AND_OFFICES = new Set([
  // Distinctive words of US state names.
  'alabama', 'alaska', 'arizona', 'arkansas', 'california', 'colorado',
  'connecticut', 'delaware', 'florida', 'georgia', 'hawaii', 'idaho', 'illinois',
  'indiana', 'iowa', 'kansas', 'kentucky', 'louisiana', 'maine', 'maryland',
  'massachusetts', 'michigan', 'minnesota', 'mississippi', 'missouri', 'montana',
  'nebraska', 'nevada', 'hampshire', 'jersey', 'mexico', 'york', 'carolina',
  'dakota', 'ohio', 'oklahoma', 'oregon', 'pennsylvania', 'rhode', 'tennessee',
  'texas', 'utah', 'vermont', 'virginia', 'washington', 'wisconsin', 'wyoming',
  // Compass words, which are what separate one Carolina, Dakota or Virginia
  // from the other.
  'north', 'south', 'east', 'west',
  // The office being contested.
  'senate', 'house', 'governor', 'president', 'mayor', 'parliament', 'congress',
  'attorney', 'chair', 'nominee', 'primary',
]);

/**
 * A token that carves a family into members.
 *
 * Central bank identities qualify by construction: every rate book on the board
 * says "bank", "rate" and "decision", so the bank's own name is the only part
 * that distinguishes eleven otherwise identical titles.
 */
function isQualifier(token: string): boolean {
  return QUALIFIERS.includes(token) || PLACES_AND_OFFICES.has(token) || token.startsWith('bankof');
}

/** Applied once per lopsided qualifier, so two of them compound. */
const QUALIFIER_PENALTY = 0.35;

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
  const left = contentTokens(`${a.title} ${a.context ?? ''}`);
  const right = contentTokens(`${b.title} ${b.context ?? ''}`);
  const leftSet = new Set(left);
  const rightSet = new Set(right);

  const shared = left.filter((t, i) => rightSet.has(t) && left.indexOf(t) === i);
  let score = overlap(leftSet, rightSet);

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

  // Agreeing only on a qualifier is not agreement. "Highest temperature in
  // Oklahoma City" and "Highest temperature in Panama City" share nothing but
  // the word that says which direction the thermometer is read in — the place,
  // which is the entire question, is the part they differ on.
  // …but only where there was something else to agree on. A terse label like
  // "Lead Actor, Comedy" is *made* of qualifiers, and has nothing else to offer.
  const substance = (tokens: Set<string>): string[] =>
    [...tokens].filter((term) => !isQualifier(term));

  const bothHaveSubstance = substance(leftSet).length > 0 && substance(rightSet).length > 0;
  const sharedSubstance = shared.filter((term) => !isQualifier(term));
  if (bothHaveSubstance && sharedSubstance.length === 0 && !identical && !contains) score *= 0.4;

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

  // A qualifier on one side and not the other is the whole question, whatever
  // the rest of the words did.
  const lopsided = [...new Set([...leftSet, ...rightSet])].filter(
    (q) => isQualifier(q) && leftSet.has(q) !== rightSet.has(q),
  );
  if (lopsided.length > 0) {
    score *= QUALIFIER_PENALTY ** lopsided.length;
    reason += `; only one side says ${lopsided.slice(0, 3).join('/')}`;
  }

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
  // Years and everything else answer different questions, and mixing them lets
  // one cover for the other: "Big Brother Season 28, 2nd place" and "…3rd
  // place" agree on the season and differ on the only number that is the
  // question. Judged together, the 28 hides the 2-against-3.
  const years = compareNumbers(left.filter(isYear), right.filter(isYear));
  const rest = compareNumbers(left.filter((n) => !isYear(n)), right.filter((n) => !isYear(n)));

  const factor = years.factor * rest.factor;
  if (factor === 1) return base;

  const score = Math.min(1, base.score * factor);
  const notes = [years.note, rest.note].filter(Boolean);
  return {
    ...base,
    score,
    confidence: confidenceOf(score),
    reason: notes.length ? `${base.reason}; ${notes.join(', ')}` : base.reason,
  };
}

/**
 * Weigh one class of number against its counterpart.
 *
 * A side that states none is silent rather than contradictory — Polymarket
 * writes "Fed Decision in October" where Kalshi writes "Fed decision in Oct
 * 2026?", and the missing year is an omission, not a disagreement.
 */
function compareNumbers(left: number[], right: number[]): { factor: number; note: string } {
  if (left.length === 0 || right.length === 0) return { factor: 1, note: '' };

  const rightSet = new Set(right);
  const leftSet = new Set(left);
  const onlyLeft = left.filter((n) => !rightSet.has(n));
  const onlyRight = right.filter((n) => !leftSet.has(n));

  if (onlyLeft.length === 0 && onlyRight.length === 0) return { factor: 1.12, note: '' };

  const agreed = left.length - onlyLeft.length;
  const differing = `${onlyLeft.slice(0, 2).join('/') || '—'} vs ${onlyRight.slice(0, 2).join('/') || '—'}`;
  // Partial agreement is not agreement, but it is not a flat contradiction
  // either: one side may simply count something the other leaves unsaid.
  return { factor: agreed > 0 ? 0.6 : 0.4, note: `numbers differ (${differing})` };
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
