/**
 * Implied-price maths tests.
 *
 * This is the one part of the feature that can be checked against arithmetic
 * rather than a live market, so it is checked hard. The ladders below are
 * shaped like the real ones — Kalshi's `KXBTCD` (an "or above" ladder) and
 * `KXBTC` (a mutually-exclusive range ladder) over the same underlying and the
 * same expiry — because the two need opposite handling and getting that wrong
 * is silent: both shapes still produce a plausible-looking number.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  enforceMonotone,
  impliedMean,
  impliedMedian,
  impliedPrice,
  legFromStrike,
  quoteProbability,
  survivalKnots,
  tailMass,
  type ImpliedLeg,
} from '../src/shared/implied.js';

/** `P(price > k)` at each strike, to 4dp. */
function survivals(knots: { strike: number; survival: number }[]): [number, number][] {
  return knots.map((k) => [k.strike, Math.round(k.survival * 10_000) / 10_000]);
}

describe('quoteProbability', () => {
  it('takes the mid of a two-sided book', () => {
    assert.equal(quoteProbability(0.58, 0.6, 0.59), 0.59);
  });

  it('halves a lone ask, because the true value lies in [0, ask]', () => {
    // Somebody will sell at 1c and nobody will buy at any price. Reading that
    // as a 1c mid is what makes 80 dead wings add up to real probability mass.
    assert.equal(quoteProbability(null, 0.01, null), 0.005);
  });

  it('splits the difference between a lone bid and certainty', () => {
    assert.equal(quoteProbability(0.99, null, null), 0.995);
  });

  it('falls back to the last trade when the book is empty', () => {
    assert.equal(quoteProbability(null, null, 0.42), 0.42);
  });

  it('treats a zero quote as absent, not as worthless', () => {
    // Kalshi reports an empty book side as "0.0000".
    assert.equal(quoteProbability(0, 0, 0), null);
    assert.equal(quoteProbability(0, 0, 0.3), 0.3);
  });

  it('rejects out-of-range prices', () => {
    assert.equal(quoteProbability(null, null, 1.4), null);
    assert.equal(quoteProbability(null, null, -0.2), null);
  });
});

describe('legFromStrike', () => {
  it('maps an "or above" contract to a lower-bounded region', () => {
    assert.deepEqual(legFromStrike('greater', 63000, null, 0.5), { lo: 63000, hi: null, p: 0.5 });
    assert.deepEqual(legFromStrike('greater_or_equal', 63000, null, 0.5), {
      lo: 63000,
      hi: null,
      p: 0.5,
    });
  });

  it('maps an "or below" contract to an upper-bounded region', () => {
    assert.deepEqual(legFromStrike('less', null, 52750, 0.01), { lo: null, hi: 52750, p: 0.01 });
  });

  it('maps a range contract to a bounded bucket', () => {
    assert.deepEqual(legFromStrike('between', 62750, 62999.99, 0.3), {
      lo: 62750,
      hi: 62999.99,
      p: 0.3,
    });
  });

  it('rejects strikes that are not numeric price levels', () => {
    // Kalshi's `structured` and `custom` strikes are rules, not numbers.
    assert.equal(legFromStrike('structured' as never, null, null, 0.5), null);
    assert.equal(legFromStrike(null, null, null, 0.5), null);
    assert.equal(legFromStrike('greater', null, null, 0.5), null);
  });
});

describe('survivalKnots', () => {
  it('reads a nested "or above" ladder as the survival function directly', () => {
    // Each contract already *is* P(price > k). Summing them would be wrong.
    const legs: ImpliedLeg[] = [
      { lo: 100, hi: null, p: 0.9 },
      { lo: 110, hi: null, p: 0.5 },
      { lo: 120, hi: null, p: 0.1 },
    ];
    assert.deepEqual(survivals(survivalKnots(legs)), [
      [100, 0.9],
      [110, 0.5],
      [120, 0.1],
    ]);
  });

  it('accumulates a disjoint range ladder from the top down', () => {
    const legs: ImpliedLeg[] = [
      { lo: 100, hi: 110, p: 0.2 },
      { lo: 110, hi: 120, p: 0.5 },
      { lo: 120, hi: 130, p: 0.3 },
    ];
    // P(>120) = 0.3, P(>110) = 0.8, P(>100) = 1.0
    assert.deepEqual(survivals(survivalKnots(legs)), [
      [100, 1],
      [110, 0.8],
      [120, 0.3],
    ]);
  });

  it('normalises a disjoint ladder whose quotes sum past 1', () => {
    // Bid/ask spreads inflate the raw sum. On a real 300-bucket ETH ladder the
    // un-normalised curve moved the implied price by more than $200.
    const legs: ImpliedLeg[] = [
      { lo: 100, hi: 110, p: 0.4 },
      { lo: 110, hi: 120, p: 1.0 },
      { lo: 120, hi: 130, p: 0.6 },
    ];
    assert.deepEqual(survivals(survivalKnots(legs)), [
      [100, 1],
      [110, 0.8],
      [120, 0.3],
    ]);
  });

  it('averages duplicate strikes rather than double-counting them', () => {
    const legs: ImpliedLeg[] = [
      { lo: 100, hi: null, p: 0.8 },
      { lo: 100, hi: null, p: 0.6 },
    ];
    assert.deepEqual(survivals(survivalKnots(legs)), [[100, 0.7]]);
  });

  it('returns nothing when no leg carries a usable bound', () => {
    assert.deepEqual(survivalKnots([]), []);
    assert.deepEqual(survivalKnots([{ lo: null, hi: null, p: 0.5 }]), []);
  });
});

describe('enforceMonotone', () => {
  it('leaves an already non-increasing curve alone', () => {
    const knots = [
      { strike: 1, survival: 0.9 },
      { strike: 2, survival: 0.5 },
      { strike: 3, survival: 0.2 },
    ];
    assert.deepEqual(enforceMonotone(knots), knots);
  });

  it('pools adjacent violators into their weighted mean', () => {
    // 0.4 then 0.6 is impossible: P(>2) cannot exceed P(>1). Both become 0.5,
    // which is the closest non-increasing curve rather than a directional guess.
    const fixed = enforceMonotone([
      { strike: 1, survival: 0.9 },
      { strike: 2, survival: 0.4 },
      { strike: 3, survival: 0.6 },
      { strike: 4, survival: 0.1 },
    ]);
    assert.deepEqual(survivals(fixed), [
      [1, 0.9],
      [2, 0.5],
      [3, 0.5],
      [4, 0.1],
    ]);
  });

  it('pools a run of violators across more than two points', () => {
    const fixed = enforceMonotone([
      { strike: 1, survival: 0.2 },
      { strike: 2, survival: 0.5 },
      { strike: 3, survival: 0.8 },
    ]);
    // The whole run is inconsistent, so it collapses to one level: their mean.
    assert.deepEqual(survivals(fixed), [
      [1, 0.5],
      [2, 0.5],
      [3, 0.5],
    ]);
  });

  it('clamps into [0, 1]', () => {
    const fixed = enforceMonotone([
      { strike: 1, survival: 1.4 },
      { strike: 2, survival: -0.3 },
    ]);
    assert.deepEqual(survivals(fixed), [
      [1, 1],
      [2, 0],
    ]);
  });
});

describe('impliedMedian', () => {
  it('interpolates the 50% crossing between bracketing strikes', () => {
    // Survival falls 0.8 -> 0.2 across [100, 110]; half of that drop is at 105.
    const value = impliedMedian([
      { strike: 100, survival: 0.8 },
      { strike: 110, survival: 0.2 },
    ]);
    assert.equal(value, 105);
  });

  it('returns the strike itself when it sits exactly at 50%', () => {
    const value = impliedMedian([
      { strike: 100, survival: 0.9 },
      { strike: 110, survival: 0.5 },
      { strike: 120, survival: 0.1 },
    ]);
    assert.equal(value, 110);
  });

  it('returns null when the ladder never crosses 50%', () => {
    // Every strike is above the money: the median is outside the quoted range
    // and inventing one would be worse than saying so.
    assert.equal(
      impliedMedian([
        { strike: 100, survival: 0.3 },
        { strike: 110, survival: 0.1 },
      ]),
      null,
    );
  });
});

describe('impliedMean', () => {
  it('weights each bucket by its mass and places it at the midpoint', () => {
    // Buckets: (100,110] 0.2 @105, (110,120] 0.5 @115, plus the two tails.
    // Tails: 0.2 below 100 placed at 95, 0.1 above 120 placed at 125.
    // 0.2*95 + 0.2*105 + 0.5*115 + 0.1*125 = 19 + 21 + 57.5 + 12.5 = 110
    const value = impliedMean([
      { strike: 100, survival: 0.8 },
      { strike: 110, survival: 0.6 },
      { strike: 120, survival: 0.1 },
    ]);
    assert.equal(value, 110);
  });

  it('needs at least two knots to have a bucket at all', () => {
    assert.equal(impliedMean([{ strike: 100, survival: 0.5 }]), null);
  });
});

describe('tailMass', () => {
  it('sums the mass below the lowest and above the highest strike', () => {
    const mass = tailMass([
      { strike: 100, survival: 0.97 },
      { strike: 120, survival: 0.02 },
    ]);
    assert.equal(Math.round(mass * 1000) / 1000, 0.05);
  });

  it('is total when there are no strikes to bracket anything', () => {
    assert.equal(tailMass([]), 1);
  });
});

describe('impliedPrice', () => {
  it('prices a nested "or above" ladder', () => {
    const legs = [
      { lo: 62500, hi: null, p: 0.98 },
      { lo: 63000, hi: null, p: 0.6 },
      { lo: 63500, hi: null, p: 0.15 },
      { lo: 64000, hi: null, p: 0.02 },
    ];
    const result = impliedPrice(legs, 'median');
    // Crossing lies between 63000 (0.6) and 63500 (0.15): 63000 + 500*(0.1/0.45)
    assert.ok(result.value !== null);
    assert.equal(Math.round(result.value), 63111);
    assert.equal(result.method, 'median');
  });

  it('prices a disjoint range ladder to the same place as the nested one', () => {
    // Same distribution, expressed as buckets instead of as an "or above"
    // ladder. The two shapes must not disagree.
    const nested = impliedPrice(
      [
        { lo: 100, hi: null, p: 0.9 },
        { lo: 110, hi: null, p: 0.6 },
        { lo: 120, hi: null, p: 0.2 },
      ],
      'median',
    );
    const buckets = impliedPrice(
      [
        { lo: 100, hi: 110, p: 0.3 },
        { lo: 110, hi: 120, p: 0.4 },
        { lo: 120, hi: 130, p: 0.2 },
      ],
      'median',
    );
    // Bucket masses normalise to 1/3, 4/9, 2/9 → survival 1.0, 0.667, 0.222.
    assert.ok(nested.value !== null && buckets.value !== null);
    assert.equal(Math.round(nested.value), 113);
    assert.equal(Math.round(buckets.value), 114);
  });

  it('recovers a sane median from a ladder full of dead one-sided wings', () => {
    // The failure this guards: 40 wings quoted "no bid, 1c ask" around a live
    // centre. Read as mids they contribute 0.2 of phantom mass and drag the
    // crossing; halved and normalised, the centre holds.
    const legs: ImpliedLeg[] = [];
    for (let strike = 900; strike < 1000; strike += 10) {
      legs.push({ lo: strike, hi: strike + 10, p: quoteProbability(null, 0.01, null)! });
    }
    legs.push({ lo: 1000, hi: 1010, p: 0.5 }, { lo: 1010, hi: 1020, p: 0.45 });
    for (let strike = 1020; strike < 1120; strike += 10) {
      legs.push({ lo: strike, hi: strike + 10, p: quoteProbability(null, 0.01, null)! });
    }

    const result = impliedPrice(legs, 'median');
    assert.ok(result.value !== null);
    assert.ok(
      result.value > 1000 && result.value < 1020,
      `expected the crossing inside the live buckets, got ${result.value}`,
    );
  });

  it('reports why it could not produce a number', () => {
    assert.equal(impliedPrice([], 'median').reason, 'no_strikes');
    assert.equal(
      impliedPrice(
        [
          { lo: 100, hi: null, p: 0.3 },
          { lo: 110, hi: null, p: 0.1 },
        ],
        'median',
      ).reason,
      'no_crossing',
    );
  });

  it('never lets a crossing land outside the quoted strikes', () => {
    const result = impliedPrice(
      [
        { lo: 100, hi: null, p: 0.9 },
        { lo: 110, hi: null, p: 0.55 },
        { lo: 120, hi: null, p: 0.45 },
        { lo: 130, hi: null, p: 0.1 },
      ],
      'median',
    );
    assert.ok(result.value !== null);
    assert.ok(result.value >= 100 && result.value <= 130);
  });
});
