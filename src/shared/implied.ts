/**
 * Turning a Kalshi strike ladder into an implied price for the underlying.
 *
 * A binary contract quoted at `p` is a market forecast of `P(settlement lands
 * in this contract's region) = p`. A *ladder* of them over the same underlying
 * and expiry therefore quotes a whole probability distribution, and the number
 * traders want out of it — "where does the market think BTC closes?" — is a
 * location statistic of that distribution.
 *
 * The pipeline is four steps, in order:
 *
 *   1. legs → survival knots  `S(k) = P(price > k)`, from any mix of
 *      above/below/range contracts.
 *   2. monotone fit           a survival function cannot rise with the strike;
 *      quotes sometimes say it does.
 *   3. locate                 the interpolated 50% crossing (median), or the
 *      bucket-weighted mean.
 *   4. report the tail        mass outside the quoted strikes, so a caller can
 *      tell a confident number from an extrapolated one.
 *
 * Nothing here touches the network or the DOM: it is the one part of this
 * feature that can be checked against arithmetic rather than a live market.
 */

import type { ImpliedMethod, StrikeType } from './types.js';

/**
 * One contract's contribution: the region it pays out on, and its price.
 *
 * `lo`/`hi` are `null` for an unbounded side, so `{lo: 63000, hi: null}` is
 * "$63,000 or above" and `{lo: null, hi: 52750}` is "$52,750 or below".
 */
export interface ImpliedLeg {
  lo: number | null;
  hi: number | null;
  /** Probability, 0..1. */
  p: number;
  /** Present for diagnostics; the maths only reads `lo`/`hi`. */
  ticker?: string;
}

/** A point on the estimated survival function. */
export interface SurvivalKnot {
  strike: number;
  /** `P(price > strike)`, 0..1, non-increasing in `strike` after the fit. */
  survival: number;
}

export interface ImpliedResult {
  /** The implied price, or `null` when the ladder cannot support one. */
  value: number | null;
  method: ImpliedMethod;
  knots: SurvivalKnot[];
  /** Probability mass below the lowest strike plus mass above the highest. */
  tailMass: number;
  /** Why `value` is `null`, for the panel to show instead of an empty chart. */
  reason?: 'no_strikes' | 'no_crossing';
}

/* --------------------------------------------------------------- quoting */

/**
 * The market's probability for one contract, given its book.
 *
 * A two-sided book gives the mid. A one-sided book is the interesting case:
 * an offer at 1¢ with no bid does *not* mean 0.5¢ — it means somebody will
 * sell at 1¢ and nobody will buy at any price, so the true value lies in
 * `[0, ask]`, and its midpoint is the honest reading. Treating a lone ask as a
 * mid is what makes deep out-of-the-money strikes contribute phantom
 * probability, which on an 80-strike ladder adds up to a badly skewed
 * distribution.
 *
 * Last price is the final fallback: it is a real trade, but a stale one.
 */
export function quoteProbability(
  bid: number | null,
  ask: number | null,
  last: number | null,
): number | null {
  const b = valid(bid);
  const a = valid(ask);
  if (b !== null && a !== null) return clamp01((b + a) / 2);
  if (a !== null) return clamp01(a / 2);
  if (b !== null) return clamp01((b + 1) / 2);
  const l = valid(last);
  return l === null ? null : clamp01(l);
}

/** Kalshi reports an empty book side as `0`, which is "absent", not "worthless". */
function valid(value: number | null): number | null {
  return value === null || !Number.isFinite(value) || value <= 0 || value > 1 ? null : value;
}

function clamp01(value: number): number {
  return Math.min(1, Math.max(0, value));
}

/** Map a Kalshi strike type and its bounds onto a leg's payout region. */
export function legFromStrike(
  strikeType: StrikeType | null,
  floorStrike: number | null,
  capStrike: number | null,
  p: number,
  ticker?: string,
): ImpliedLeg | null {
  const lo = Number.isFinite(floorStrike) ? floorStrike : null;
  const hi = Number.isFinite(capStrike) ? capStrike : null;

  switch (strikeType) {
    case 'greater':
    case 'greater_or_equal':
      // The half-cent offset between `>` and `>=` is far below the tick size of
      // any ladder Kalshi lists; both are the same knot.
      return lo === null ? null : { lo, hi: null, p, ...(ticker ? { ticker } : {}) };
    case 'less':
    case 'less_or_equal':
      return hi === null ? null : { lo: null, hi, p, ...(ticker ? { ticker } : {}) };
    case 'between':
      return lo === null || hi === null ? null : { lo, hi, p, ...(ticker ? { ticker } : {}) };
    default:
      // `structured` and `custom` strikes are not numeric price levels.
      return null;
  }
}

/* -------------------------------------------------------- survival curve */

/**
 * Legs → survival knots.
 *
 * Two ladder shapes reach here and they need opposite treatment:
 *
 * **Nested** (`BTC $63,000 or above`, `$63,250 or above`, …). Each contract
 * already *is* `P(price > k)`, so its price is the knot. Probabilities overlap
 * by construction and must not be summed.
 *
 * **Disjoint** (`BTC $62,750 to $62,999`, `$63,000 to $63,249`, …). These tile
 * the line, so they are a probability mass function and the survival at each
 * bucket floor is the sum of every bucket at or above it.
 *
 * A disjoint ladder is also *normalised* to sum to 1. Bid/ask spreads make the
 * raw sum drift above 1 — on a 300-bucket ETH ladder the drift moved the
 * implied price by more than $200, which is the difference between a useful
 * overlay and a misleading one. Normalising is not cosmetic: mutual exclusivity
 * is a fact about the event, so the constraint is real information.
 */
export function survivalKnots(legs: ImpliedLeg[]): SurvivalKnot[] {
  const usable = legs.filter((l) => Number.isFinite(l.p) && (l.lo !== null || l.hi !== null));
  if (usable.length === 0) return [];

  /*
   * What tells the two shapes apart is a *bounded* leg, not the absence of a
   * `hi`. A leg with both bounds is a bucket and belongs to a mass function; a
   * leg with one bound is a cumulative claim and already states a survival.
   *
   * Testing `every(l => l.hi === null)` instead meant a single "or below" rung
   * — which is one-sided, and every bit as cumulative as its "or above"
   * siblings — dragged an otherwise nested ladder down the disjoint path, where
   * overlapping cumulative prices were summed and normalised as if they were
   * disjoint masses. It moved a BTC ladder's median by about 2%, silently, and
   * `selectStrikes` can drop or restore that one rung between polls, so a
   * single overlay could flip interpretation mid-chart.
   */
  const bucketed = usable.some((l) => l.lo !== null && l.hi !== null);

  if (!bucketed) {
    const knots = usable.map((l) =>
      l.lo !== null
        ? // "k or above" quotes P(price > k) directly.
          { strike: l.lo, survival: clamp01(l.p) }
        : // "k or below" quotes the complement of the same thing.
          { strike: l.hi!, survival: clamp01(1 - l.p) },
    );
    return collapseDuplicates(knots);
  }

  const total = usable.reduce((sum, l) => sum + l.p, 0);
  const scale = total > 0 ? 1 / total : 0;

  // Walk the buckets from the top down, accumulating mass. Each bucket's floor
  // is a strike whose survival is everything stacked above it.
  const descending = [...usable]
    .filter((l): l is ImpliedLeg & { lo: number } => l.lo !== null)
    .sort((a, b) => b.lo - a.lo);

  const knots: SurvivalKnot[] = [];
  let running = 0;
  for (const leg of descending) {
    running += leg.p * scale;
    knots.push({ strike: leg.lo, survival: clamp01(running) });
  }

  /*
   * Close the top of a bounded ladder.
   *
   * Emitting a knot only at each bucket's floor throws away the cap of the
   * highest bucket, so `(120, 130]` left the curve ending at 120 with the
   * bucket's own mass still sitting on it. `tailMass` then read that mass as
   * unbracketed and reported 30% on a ladder that brackets everything — and
   * `tailMass` is the number the panel offers a reader for judging how much of
   * the estimate is assumption. Nothing lies above a bounded cap, so survival
   * there is zero, and saying that costs one knot.
   */
  const highest = descending[0];
  if (highest?.hi !== null && highest?.hi !== undefined) {
    knots.push({ strike: highest.hi, survival: 0 });
  }

  // The `less` leg contributes only mass below the lowest floor, which the
  // accumulation above already accounts for; it needs no knot of its own.
  return collapseDuplicates(knots.reverse());
}

/** Average duplicate strikes and sort ascending, so knots are a clean function. */
function collapseDuplicates(knots: SurvivalKnot[]): SurvivalKnot[] {
  const byStrike = new Map<number, { sum: number; n: number }>();
  for (const k of knots) {
    const entry = byStrike.get(k.strike) ?? { sum: 0, n: 0 };
    entry.sum += k.survival;
    entry.n += 1;
    byStrike.set(k.strike, entry);
  }
  return [...byStrike.entries()]
    .map(([strike, { sum, n }]) => ({ strike, survival: sum / n }))
    .sort((a, b) => a.strike - b.strike);
}

/**
 * Force the survival curve to be non-increasing.
 *
 * `P(price > k)` cannot grow as `k` grows, but quoted ladders violate it
 * constantly — adjacent strikes are quoted by different participants at
 * different moments, and a 1¢ tick is wider than the true gap between
 * neighbouring strikes. Left uncorrected, a violation puts a spurious 50%
 * crossing in the middle of the ladder and the implied line jumps.
 *
 * This is pool-adjacent-violators, which returns the closest non-increasing
 * curve in the least-squares sense — it corrects the inconsistency without
 * choosing a direction to be wrong in, which a simpler running-minimum or
 * running-maximum pass would.
 */
export function enforceMonotone(knots: SurvivalKnot[]): SurvivalKnot[] {
  if (knots.length < 2) return knots;

  const values: number[] = [];
  const weights: number[] = [];

  for (const knot of knots) {
    values.push(knot.survival);
    weights.push(1);
    // Merge backwards while the previous block sits *below* this one.
    while (values.length > 1 && values[values.length - 2]! < values[values.length - 1]!) {
      const v2 = values.pop()!;
      const w2 = weights.pop()!;
      const v1 = values.pop()!;
      const w1 = weights.pop()!;
      values.push((v1 * w1 + v2 * w2) / (w1 + w2));
      weights.push(w1 + w2);
    }
  }

  const out: SurvivalKnot[] = [];
  let index = 0;
  for (let block = 0; block < values.length; block++) {
    const value = clamp01(values[block]!);
    for (let n = 0; n < weights[block]!; n++) {
      out.push({ strike: knots[index]!.strike, survival: value });
      index++;
    }
  }
  return out;
}

/* ------------------------------------------------------------- estimators */

/**
 * The strike where the market puts even odds of finishing above or below.
 *
 * This is the default because it needs nothing from outside the quoted ladder:
 * the crossing is bracketed by two real strikes with real quotes. Its cost is
 * that a market whose whole ladder sits on one side of 50% has no crossing at
 * all, and this returns `null` rather than inventing one.
 */
export function impliedMedian(knots: SurvivalKnot[]): number | null {
  for (let i = 0; i < knots.length - 1; i++) {
    const a = knots[i]!;
    const b = knots[i + 1]!;
    if (a.survival >= 0.5 && b.survival <= 0.5) {
      if (a.survival === b.survival) return a.strike;
      const t = (a.survival - 0.5) / (a.survival - b.survival);
      return a.strike + t * (b.strike - a.strike);
    }
  }
  return null;
}

/**
 * Probability-weighted mean of the distribution.
 *
 * Between consecutive strikes the mass `S(k_i) - S(k_i+1)` is placed at the
 * midpoint. The tails are the honest weakness: mass below the lowest strike and
 * above the highest has no bracket, so each is placed half an average strike
 * step beyond the edge. That guess is why {@link ImpliedResult.tailMass} is
 * reported — with 1% in the tails the mean is solid, with 30% it is mostly the
 * assumption talking.
 */
export function impliedMean(knots: SurvivalKnot[]): number | null {
  if (knots.length < 2) return null;

  const first = knots[0]!;
  const last = knots[knots.length - 1]!;
  const step = (last.strike - first.strike) / (knots.length - 1);

  let total = 0;
  for (let i = 0; i < knots.length - 1; i++) {
    const a = knots[i]!;
    const b = knots[i + 1]!;
    total += (a.survival - b.survival) * ((a.strike + b.strike) / 2);
  }

  total += (1 - first.survival) * (first.strike - step / 2);
  total += last.survival * (last.strike + step / 2);
  return total;
}

/** Mass the quoted strikes do not bracket: below the floor plus above the cap. */
export function tailMass(knots: SurvivalKnot[]): number {
  if (knots.length === 0) return 1;
  const below = 1 - knots[0]!.survival;
  const above = knots[knots.length - 1]!.survival;
  return clamp01(below + above);
}

/**
 * Run the whole pipeline: legs in, one implied price out.
 *
 * Returns the intermediate knots too, because a panel that can show *why* a
 * number moved is worth far more than one that only shows the number.
 */
export function impliedPrice(legs: ImpliedLeg[], method: ImpliedMethod = 'median'): ImpliedResult {
  const knots = enforceMonotone(survivalKnots(legs));

  if (knots.length < 2) {
    return { value: null, method, knots, tailMass: tailMass(knots), reason: 'no_strikes' };
  }

  const value = method === 'mean' ? impliedMean(knots) : impliedMedian(knots);
  const result: ImpliedResult = { value, method, knots, tailMass: tailMass(knots) };
  // A median with no 50% crossing means the ladder does not straddle the
  // market — the strikes are all above, or all below, where price is trading.
  if (value === null) result.reason = 'no_crossing';
  return result;
}
