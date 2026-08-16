/**
 * Option pricing and Greeks.
 *
 * Like the implied-price maths, this is the part of the feature whose output
 * looks entirely plausible when it is wrong: a delta computed with the wrong
 * carry, a vega scaled per unit instead of per volatility point, or a theta
 * that forgot to divide by 365 all produce a number of about the right size in
 * about the right place. So it is checked three ways, and each catches things
 * the others cannot:
 *
 *   1. **Against arithmetic.** Put-call parity, intrinsic bounds, and the
 *      identities every option price has to satisfy.
 *   2. **Against finite differences.** Every Greek is re-derived by numerically
 *      differentiating {@link blackPrice} through the *economic* variables
 *      `(S, r, q, τ, σ)`, rebuilding the forward and discount factor at each
 *      bump. This is what pins the units down — a vega quoted per unit rather
 *      than per point is off by exactly 100 and nothing else notices.
 *   3. **Against Deribit's own published Greeks**, from a captured live
 *      response. An exchange's risk system is the only external oracle
 *      available for this, and it is a good one.
 *
 * The third check is what caught the real bug here. Deribit publishes an
 * `interest_rate` of `0.0` on every instrument while quoting a ten-month
 * forward 3.9% above the index. Trusting the field over the forward left every
 * long-dated delta on the board disagreeing with Deribit's own by that amount.
 */

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { describe, it } from 'node:test';
import {
  blackGreeks,
  blackPrice,
  breakeven,
  equityExpiryInstant,
  fitForward,
  impliedVol,
  maxPain,
  moneyness,
  normalCdf,
  yearsToExpiry,
  type BlackInputs,
} from '../src/shared/greeks.js';

/* ------------------------------------------------------------------ helpers */

/** A textbook set: spot 100, 2% carry-adjusted forward, 25% vol, 6 months. */
const BASE: BlackInputs = {
  spot: 100,
  forward: 101,
  strike: 100,
  years: 0.5,
  vol: 0.25,
  discount: Math.exp(-0.04 * 0.5),
  type: 'call',
};

/**
 * Price as a function of the economic variables, rebuilding the forward and the
 * discount factor from them.
 *
 * This is the whole point of the finite-difference checks: bumping `forward`
 * while holding `discount` fixed is a *different* derivative from bumping the
 * rate, and using the first as a check on the second is how a wrong test
 * convinces you that correct code is broken.
 */
function value(
  spot: number,
  rate: number,
  carry: number,
  strike: number,
  years: number,
  vol: number,
  type: 'call' | 'put',
): number {
  return blackPrice({
    spot,
    forward: spot * Math.exp((rate - carry) * years),
    strike,
    years,
    vol,
    discount: Math.exp(-rate * years),
    type,
  })!;
}

function closeTo(actual: number, expected: number, tolerance: number, what: string): void {
  assert.ok(
    Math.abs(actual - expected) <= tolerance,
    `${what}: expected ${expected}, got ${actual} (tolerance ${tolerance})`,
  );
}

/* ------------------------------------------------------------------ pricing */

describe('blackPrice', () => {
  it('satisfies put-call parity', () => {
    const call = blackPrice({ ...BASE, type: 'call' })!;
    const put = blackPrice({ ...BASE, type: 'put' })!;
    // C - P = DF·(F - K), exactly, for any vol.
    closeTo(call - put, BASE.discount * (BASE.forward - BASE.strike), 1e-9, 'parity');
  });

  it('collapses to discounted intrinsic at zero volatility', () => {
    const call = blackPrice({ ...BASE, vol: 0, type: 'call' })!;
    closeTo(call, BASE.discount * (BASE.forward - BASE.strike), 1e-9, 'zero-vol call');
    assert.equal(blackPrice({ ...BASE, vol: 0, type: 'put' }), 0);
  });

  it('collapses to intrinsic at expiry rather than returning null', () => {
    // A contract expiring today is still worth something, and a chain that
    // blanked its last column on expiry day would hide the busiest board of
    // the week.
    const call = blackPrice({ ...BASE, years: 0, strike: 90, type: 'call' })!;
    closeTo(call, (BASE.forward - 90) * BASE.discount, 1e-9, 'expiry intrinsic');
  });

  it('is increasing in volatility', () => {
    let previous = 0;
    for (const vol of [0.05, 0.1, 0.25, 0.5, 1, 2]) {
      const price = blackPrice({ ...BASE, vol })!;
      assert.ok(price > previous, `price should rise with vol, ${price} <= ${previous}`);
      previous = price;
    }
  });

  it('stays inside its no-arbitrage bounds', () => {
    for (const strike of [50, 80, 100, 130, 200]) {
      for (const type of ['call', 'put'] as const) {
        const price = blackPrice({ ...BASE, strike, type })!;
        const intrinsic =
          Math.max(0, type === 'call' ? BASE.forward - strike : strike - BASE.forward) *
          BASE.discount;
        const ceiling = BASE.discount * (type === 'call' ? BASE.forward : strike);
        assert.ok(price >= intrinsic - 1e-9, `${type} ${strike} below intrinsic`);
        assert.ok(price <= ceiling + 1e-9, `${type} ${strike} above ceiling`);
      }
    }
  });

  it('rejects nonsensical inputs rather than returning a number', () => {
    assert.equal(blackPrice({ ...BASE, forward: 0 }), null);
    assert.equal(blackPrice({ ...BASE, strike: -10 }), null);
  });
});

/* ------------------------------------------------------------------- greeks */

describe('blackGreeks', () => {
  it('reproduces every Greek by finite difference, in the stated units', () => {
    const cases: { strike: number; years: number; vol: number; type: 'call' | 'put' }[] = [];
    for (const strike of [70, 90, 100, 110, 140]) {
      for (const years of [0.02, 0.25, 1.5]) {
        for (const vol of [0.15, 0.4, 0.9]) {
          for (const type of ['call', 'put'] as const) cases.push({ strike, years, vol, type });
        }
      }
    }

    const spot = 100;
    const rate = 0.043;
    const carry = 0.011;

    for (const { strike, years, vol, type } of cases) {
      const forward = spot * Math.exp((rate - carry) * years);
      const greeks = blackGreeks({
        spot,
        forward,
        strike,
        years,
        vol,
        discount: Math.exp(-rate * years),
        type,
      });

      const dS = spot * 1e-5;
      const price = (s: number, r: number, q: number, t: number, v: number): number =>
        value(s, r, q, strike, t, v, type);

      // Gamma is checked as the derivative of *delta*, not as the second
      // derivative of price. A second difference divides by `h²`, and there is
      // no bump size that works for both an at-the-money weekly (where gamma is
      // sharply peaked, so a wide bump truncates) and a deep in-the-money one
      // (where gamma is ~1e-10, so a narrow bump is pure cancellation noise).
      // Differentiating delta once is numerically clean at every strike, and
      // since delta is itself checked against price just below, the chain
      // gamma → delta → price still ties every Greek back to the price.
      const deltaAt = (s: number): number =>
        blackGreeks({
          spot: s,
          forward: s * Math.exp((rate - carry) * years),
          strike,
          years,
          vol,
          discount: Math.exp(-rate * years),
          type,
        }).delta!;

      const fd = {
        delta: (price(spot + dS, rate, carry, years, vol) - price(spot - dS, rate, carry, years, vol)) / (2 * dS),
        gamma: (deltaAt(spot + dS) - deltaAt(spot - dS)) / (2 * dS),
        // Per volatility *point*, hence the /100.
        vega: (price(spot, rate, carry, years, vol + 1e-6) - price(spot, rate, carry, years, vol - 1e-6)) / 2e-6 / 100,
        // Per calendar *day*, and time to expiry shrinks as the day passes.
        theta: -(price(spot, rate, carry, years + 1e-6, vol) - price(spot, rate, carry, years - 1e-6, vol)) / 2e-6 / 365,
        // Per *percentage point* of rate.
        rho: (price(spot, rate + 1e-7, carry, years, vol) - price(spot, rate - 1e-7, carry, years, vol)) / 2e-7 / 100,
      };

      const label = `${type} K=${strike} T=${years} v=${vol}`;
      closeTo(greeks.delta!, fd.delta, Math.max(1e-8, Math.abs(fd.delta) * 1e-4), `delta ${label}`);
      closeTo(greeks.gamma!, fd.gamma, Math.max(1e-12, Math.abs(fd.gamma) * 1e-4), `gamma ${label}`);
      closeTo(greeks.vega!, fd.vega, Math.max(1e-10, Math.abs(fd.vega) * 1e-4), `vega ${label}`);
      closeTo(greeks.theta!, fd.theta, Math.max(1e-10, Math.abs(fd.theta) * 1e-4), `theta ${label}`);
      closeTo(greeks.rho!, fd.rho, Math.max(1e-10, Math.abs(fd.rho) * 1e-4), `rho ${label}`);
    }
  });

  it('keeps delta in its sign-correct range', () => {
    for (const strike of [50, 100, 200]) {
      const call = blackGreeks({ ...BASE, strike, type: 'call' });
      const put = blackGreeks({ ...BASE, strike, type: 'put' });
      assert.ok(call.delta! > 0 && call.delta! < 1, `call delta out of range at ${strike}`);
      assert.ok(put.delta! < 0 && put.delta! > -1, `put delta out of range at ${strike}`);
    }
  });

  it('gives a call and a put at the same strike identical gamma and vega', () => {
    const call = blackGreeks({ ...BASE, type: 'call' });
    const put = blackGreeks({ ...BASE, type: 'put' });
    closeTo(call.gamma!, put.gamma!, 1e-12, 'gamma');
    closeTo(call.vega!, put.vega!, 1e-12, 'vega');
  });

  it('drives a deep in-the-money call delta toward the discounted carry factor', () => {
    const greeks = blackGreeks({ ...BASE, strike: 1, vol: 0.05 });
    // e^(-qT), which is what a spot delta saturates at rather than exactly 1.
    const carryFactor = (BASE.discount * BASE.forward) / BASE.spot;
    closeTo(greeks.delta!, carryFactor, 1e-6, 'saturated delta');
  });

  it('returns nulls rather than zeroes when the model cannot speak', () => {
    const expired = blackGreeks({ ...BASE, years: 0 });
    assert.deepEqual(expired, { delta: null, gamma: null, vega: null, theta: null, rho: null });
    assert.equal(blackGreeks({ ...BASE, vol: 0 }).delta, null);
  });
});

/* ------------------------------------------- the exchange as an oracle */

interface DeribitFixture {
  tickers: {
    timestamp: number;
    index_price: number;
    underlying_price: number;
    mark_price: number;
    mark_iv: number;
    greeks: { delta: number; gamma: number; vega: number; theta: number; rho: number };
  }[];
  instruments: { instrument_name: string; strike: number; expiration_timestamp: number; option_type: string }[];
}

const deribit = JSON.parse(
  readFileSync(new URL('./fixtures/deribit-greeks.json', import.meta.url), 'utf8'),
) as DeribitFixture;

describe("against Deribit's own published Greeks", () => {
  it('has a fixture of live contracts spanning moneyness and maturity', () => {
    assert.ok(deribit.tickers.length >= 10);
    assert.equal(deribit.tickers.length, deribit.instruments.length);
  });

  it('reproduces delta and price from the forward-implied discount factor', () => {
    for (const [index, ticker] of deribit.tickers.entries()) {
      const instrument = deribit.instruments[index]!;
      const years = (instrument.expiration_timestamp - ticker.timestamp) / 1000 / (365 * 86400);
      const type = instrument.option_type === 'put' ? 'put' : 'call';

      const inputs: BlackInputs = {
        spot: ticker.index_price,
        forward: ticker.underlying_price,
        strike: instrument.strike,
        years,
        vol: ticker.mark_iv / 100,
        // The correction this test exists to lock in: the discount factor comes
        // from the spot/forward basis, not from Deribit's `interest_rate: 0.0`.
        discount: ticker.index_price / ticker.underlying_price,
        type,
      };

      const greeks = blackGreeks(inputs);
      const price = blackPrice(inputs)!;
      const theirPrice = ticker.mark_price * ticker.index_price;

      closeTo(greeks.delta!, ticker.greeks.delta, 1e-3, `delta ${instrument.instrument_name}`);
      // Deribit rounds its published gamma to five decimals, which on a $63,000
      // underlying is coarser than gamma itself.
      closeTo(greeks.gamma!, ticker.greeks.gamma, 1e-5, `gamma ${instrument.instrument_name}`);
      // Absolute dollars, not relative: `mark_iv` is published to two decimal
      // places, and one hundredth of a vol point is worth a couple of dollars
      // on a long-dated BTC contract. Every price lands inside the bid/ask.
      closeTo(price, theirPrice, 5, `price ${instrument.instrument_name}`);
    }
  });

  it('would disagree with Deribit if the interest-rate field were believed', () => {
    // The regression guard. With `discount: 1` — what `interest_rate: 0.0`
    // implies — the longest-dated contract's delta is out by several percent.
    const index = deribit.instruments.findIndex(
      (instrument, i) =>
        (instrument.expiration_timestamp - deribit.tickers[i]!.timestamp) / 1000 / (365 * 86400) > 0.5,
    );
    assert.ok(index >= 0, 'fixture should contain a long-dated contract');

    const ticker = deribit.tickers[index]!;
    const instrument = deribit.instruments[index]!;
    const years = (instrument.expiration_timestamp - ticker.timestamp) / 1000 / (365 * 86400);

    const naive = blackGreeks({
      spot: ticker.index_price,
      forward: ticker.underlying_price,
      strike: instrument.strike,
      years,
      vol: ticker.mark_iv / 100,
      discount: 1,
      type: instrument.option_type === 'put' ? 'put' : 'call',
    });

    assert.ok(
      Math.abs(naive.delta! - ticker.greeks.delta) > 0.01,
      'an undiscounted delta should visibly disagree with the exchange',
    );
  });
});

/* ------------------------------------------------------- implied volatility */

describe('impliedVol', () => {
  it('round-trips a price back to the volatility that made it', () => {
    let checked = 0;

    for (const vol of [0.05, 0.2, 0.55, 1.2, 3]) {
      for (const strike of [60, 100, 160]) {
        for (const type of ['call', 'put'] as const) {
          const inputs = { ...BASE, strike, vol, type };
          const price = blackPrice(inputs)!;
          const intrinsic =
            Math.max(0, type === 'call' ? BASE.forward - strike : strike - BASE.forward) *
            BASE.discount;

          // A contract with no time value left carries no recoverable
          // volatility — at 5% vol a 41-point in-the-money call prices to its
          // intrinsic value in double precision, and every vol from 1% to 10%
          // produces that same number. Those cases are asserted below as
          // `null`, which is the honest answer; only contracts with real
          // extrinsic value can round-trip.
          if (price - intrinsic < 1e-6) continue;

          const solved = impliedVol(price, inputs);
          assert.ok(solved !== null, `no solution for vol=${vol} K=${strike} ${type}`);
          closeTo(solved, vol, 1e-6, `round trip vol=${vol} K=${strike} ${type}`);
          checked++;
        }
      }
    }

    assert.ok(checked >= 20, `expected a broad sweep, only checked ${checked}`);
  });

  it('has no answer for a contract trading at intrinsic', () => {
    // Deep in the money at a low volatility: every vol prices the same, so
    // there is nothing to solve and `null` is the truthful result.
    const inputs = { ...BASE, strike: 60, vol: 0.05, type: 'call' as const };
    const price = blackPrice(inputs)!;
    assert.equal(impliedVol(price, inputs), null);
  });

  it('still recovers the volatility of a vanishingly cheap contract', () => {
    // 160-strike call on a 101 forward at 5% vol is worth about 1.7e-39, and
    // the volatility is still perfectly well defined. This is the case that
    // caught the solver judging convergence on an *absolute* price tolerance:
    // under `|error| < 1e-10`, every vol from 1% to 500% "converged", and the
    // answer was whichever one the iteration happened to try first.
    const inputs = { ...BASE, strike: 160, vol: 0.05, type: 'call' as const };
    const price = blackPrice(inputs)!;
    assert.ok(price > 0 && price < 1e-30, `expected a vanishing price, got ${price}`);

    const solved = impliedVol(price, inputs);
    assert.ok(solved !== null);
    closeTo(solved, 0.05, 1e-6, 'vol of a vanishing contract');
  });

  it('solves the wings, where vega is nearly zero', () => {
    // A far out-of-the-money contract is exactly where a bare Newton solver
    // diverges, and exactly where a chain still wants a number.
    const inputs = { ...BASE, strike: 400, vol: 0.8, type: 'call' as const };
    const price = blackPrice(inputs)!;
    const solved = impliedVol(price, inputs);
    assert.ok(solved !== null);
    closeTo(solved, 0.8, 1e-5, 'deep OTM');
  });

  it('returns null below intrinsic and above the ceiling', () => {
    const inputs = { ...BASE, strike: 50, type: 'call' as const };
    const intrinsic = (BASE.forward - 50) * BASE.discount;
    assert.equal(impliedVol(intrinsic * 0.9, inputs), null);
    assert.equal(impliedVol(BASE.discount * BASE.forward * 1.01, inputs), null);
  });

  it('treats a missing or non-positive price as unanswerable', () => {
    assert.equal(impliedVol(null, BASE), null);
    assert.equal(impliedVol(0, BASE), null);
    assert.equal(impliedVol(-1, BASE), null);
  });

  it('returns null rather than pinning to a bracket edge', () => {
    // A price implying a volatility beyond the search range is not a 500% vol,
    // it is a bad quote — and drawing it on a smile would ruin the axis.
    assert.equal(impliedVol(BASE.discount * BASE.forward * 0.999999, { ...BASE, strike: 100 }), null);
  });
});

/* --------------------------------------------------------- forward from parity */

describe('fitForward', () => {
  /** Prices generated from a known forward and discount, so the answer is known. */
  function syntheticPairs(forward: number, discount: number, strikes: number[], vol = 0.3): {
    strike: number;
    callPrice: number;
    putPrice: number;
  }[] {
    return strikes.map((strike) => ({
      strike,
      callPrice: blackPrice({ spot: 100, forward, strike, years: 0.5, vol, discount, type: 'call' })!,
      putPrice: blackPrice({ spot: 100, forward, strike, years: 0.5, vol, discount, type: 'put' })!,
    }));
  }

  it('recovers the forward and the discount factor exactly', () => {
    const forward = 101.7;
    const discount = Math.exp(-0.045 * 0.5);
    const fit = fitForward(syntheticPairs(forward, discount, [95, 98, 100, 102, 105]), 100, 0.5);

    assert.ok(fit !== null);
    closeTo(fit.forward, forward, 1e-6, 'forward');
    closeTo(fit.discount, discount, 1e-9, 'discount');
    closeTo(fit.rate, 0.045, 1e-6, 'rate');
    // q = r - ln(F/S)/T, which for this forward is a small negative carry.
    closeTo(fit.carry, 0.045 - Math.log(forward / 100) / 0.5, 1e-6, 'carry');
    assert.equal(fit.used, 5);
  });

  it('is unmoved by noise on individual strikes, which one ATM pair is not', () => {
    const forward = 101.7;
    const discount = Math.exp(-0.045 * 0.5);
    const pairs = syntheticPairs(forward, discount, [96, 98, 100, 102, 104]);
    // Cross one strike's quotes by a tick, the way a stale book does.
    pairs[2]!.callPrice += 0.25;
    pairs[2]!.putPrice -= 0.25;

    const fit = fitForward(pairs, 100, 0.5);
    assert.ok(fit !== null);
    // The regression spreads the error over five strikes instead of inheriting
    // it whole, which is the entire reason it is a regression.
    assert.ok(Math.abs(fit.forward - forward) < 0.4, `forward moved to ${fit.forward}`);

    const single = 100 + (pairs[2]!.callPrice - pairs[2]!.putPrice) / discount;
    assert.ok(
      Math.abs(single - forward) > Math.abs(fit.forward - forward),
      'the single-strike reading should be worse than the fit',
    );
  });

  it('ignores strikes outside the near-the-money window', () => {
    // Where the American early-exercise premium lives, and where the books are
    // widest. A deep strike is excluded before it can distort the line.
    const fit = fitForward(
      syntheticPairs(101.7, Math.exp(-0.045 * 0.5), [40, 45, 50, 100, 101, 102]),
      100,
      0.5,
    );
    assert.ok(fit !== null);
    assert.equal(fit.used, 3);
  });

  it('declines to fit when there is not enough to fit', () => {
    assert.equal(fitForward(syntheticPairs(101, 0.98, [99, 101]), 100, 0.5), null);
    assert.equal(fitForward([], 100, 0.5), null);
  });

  it('rejects a fit that is arithmetically fine and financially absurd', () => {
    // Crossed quotes can produce a clean line through nonsense. Both gates fire
    // on real data often enough to be worth having.
    const absurd = [
      { strike: 98, callPrice: 1, putPrice: 60 },
      { strike: 100, callPrice: 1, putPrice: 40 },
      { strike: 102, callPrice: 1, putPrice: 20 },
    ];
    assert.equal(fitForward(absurd, 100, 0.5), null);
  });
});

/* ------------------------------------------------------------------ helpers */

describe('moneyness, breakeven and max pain', () => {
  it('reads moneyness as strike over spot, whichever way the contract pays', () => {
    closeTo(moneyness(110, 100)!, 1.1, 1e-12, 'moneyness');
    assert.equal(moneyness(110, 0), null);
  });

  it('puts a breakeven the right side of the strike for each leg', () => {
    assert.equal(breakeven('call', 100, 4.5), 104.5);
    assert.equal(breakeven('put', 100, 4.5), 95.5);
    assert.equal(breakeven('call', 100, null), null);
  });

  it('finds the settlement price that pays out least', () => {
    // All the open interest sits on the 100 calls, so writers lose nothing at
    // or below 100 — and the lowest-payout rung is the lowest strike.
    const pain = maxPain([
      { strike: 90, callOpenInterest: 0, putOpenInterest: 0 },
      { strike: 100, callOpenInterest: 1000, putOpenInterest: 0 },
      { strike: 110, callOpenInterest: 0, putOpenInterest: 0 },
    ]);
    assert.equal(pain?.strike, 90);
    assert.equal(pain?.payout, 0);
  });

  it('balances calls against puts', () => {
    const pain = maxPain([
      { strike: 90, callOpenInterest: 0, putOpenInterest: 500 },
      { strike: 100, callOpenInterest: 100, putOpenInterest: 100 },
      { strike: 110, callOpenInterest: 500, putOpenInterest: 0 },
    ]);
    // At 100: puts at 90 pay 0, calls at 110 pay 0, the 100s pay 0 — the middle
    // strike is where the two wings cancel.
    assert.equal(pain?.strike, 100);
  });

  it('has no answer for an empty ladder', () => {
    assert.equal(maxPain([]), null);
  });
});

describe('yearsToExpiry', () => {
  it('measures act/365 and floors at zero', () => {
    closeTo(yearsToExpiry(365 * 86400, 0), 1, 1e-12, 'one year');
    assert.equal(yearsToExpiry(0, 365 * 86400), 0);
  });
});

describe('equityExpiryInstant', () => {
  it('lands on 16:00 New York through both halves of the year', () => {
    // The reason this is not hard-coded to 20:00 UTC: for five months of the
    // year it would be an hour wrong, which on a 0DTE chain is a third of the
    // contract's remaining life.
    const summer = equityExpiryInstant('2026-09-18');
    assert.equal(new Date(summer! * 1000).toISOString(), '2026-09-18T20:00:00.000Z');

    const winter = equityExpiryInstant('2026-01-16');
    assert.equal(new Date(winter! * 1000).toISOString(), '2026-01-16T21:00:00.000Z');
  });

  it('handles the days either side of a DST switch', () => {
    // US DST ends on the first Sunday of November.
    const before = equityExpiryInstant('2026-10-30');
    const after = equityExpiryInstant('2026-11-06');
    assert.equal(new Date(before! * 1000).toISOString(), '2026-10-30T20:00:00.000Z');
    assert.equal(new Date(after! * 1000).toISOString(), '2026-11-06T21:00:00.000Z');
  });

  it('rejects anything that is not an ISO date', () => {
    assert.equal(equityExpiryInstant('Sep 18'), null);
    assert.equal(equityExpiryInstant(''), null);
  });
});

describe('normalCdf', () => {
  it('matches known values of the standard normal', () => {
    closeTo(normalCdf(0), 0.5, 1e-15, 'N(0)');
    closeTo(normalCdf(1), 0.841344746068543, 1e-15, 'N(1)');
    closeTo(normalCdf(-1.96), 0.0249978951482204, 1e-15, 'N(-1.96)');
    closeTo(normalCdf(2.5), 0.993790334674224, 1e-15, 'N(2.5)');
  });

  it('keeps its precision in the far tail, where a smile\'s wings live', () => {
    // The reason this is not the textbook Abramowitz & Stegun approximation.
    // A&S is accurate to 1.5e-7 *absolute*, and N(-8) is 6.2e-16 — so it would
    // return the tail as pure approximation error, and price the wing of every
    // smile from it. Relative accuracy is what matters out here.
    const relative = (got: number, expected: number): number =>
      Math.abs(got - expected) / Math.abs(expected);
    assert.ok(relative(normalCdf(-5), 2.86651571879194e-7) < 1e-9, 'N(-5)');
    assert.ok(relative(normalCdf(-8), 6.22096057427178e-16) < 1e-7, 'N(-8)');
    assert.ok(relative(normalCdf(-10), 7.61985302416053e-24) < 1e-7, 'N(-10)');
    assert.equal(normalCdf(-40), 0);
    assert.equal(normalCdf(40), 1);
  });

  it('is symmetric', () => {
    for (const x of [0.1, 0.7, 1.4, 2.6]) {
      closeTo(normalCdf(x) + normalCdf(-x), 1, 1e-9, `symmetry at ${x}`);
    }
  });
});
