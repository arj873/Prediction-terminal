/**
 * Option pricing, implied volatility and the Greeks.
 *
 * The model is Black-76 — European options on a *forward* — rather than the
 * spot-and-a-risk-free-rate form of Black-Scholes, and that choice is the whole
 * design. A terminal has no business asking its user for a risk-free rate and a
 * dividend yield: both are unobservable, both are wrong by the time you have
 * typed them, and a Greek computed from a guessed carry is a guessed Greek.
 *
 * The forward is not a guess. It is quoted, in two different ways:
 *
 *   * **Crypto.** Deribit publishes `underlying_price` per expiry. That *is* the
 *     forward its own marks are struck against.
 *   * **Equities.** Put-call parity says `C - K` is linear in the strike:
 *     `C - P = DF·(F - K)`. Regressing the near-the-money call-put spread
 *     against the strike therefore recovers the discount factor (the slope) and
 *     the forward (the intercept) from quoted prices alone — no rate input, no
 *     dividend calendar. {@link fitForward} does exactly that.
 *
 * Given `(spot, forward, discountFactor)` the carry is fully determined —
 * `r = -ln(DF)/τ` and `q = r - ln(F/S)/τ` — so the classical Greeks fall out
 * without anything ever having been assumed. When parity cannot be fitted the
 * caller is told so explicitly rather than being handed a number derived from a
 * default rate that looks identical to a real one.
 *
 * Conventions match how a trader reads them, which is also what Deribit
 * publishes, so the two are directly comparable:
 *
 * | Greek | Units |
 * | --- | --- |
 * | delta | per 1 unit of underlying (spot delta, not forward delta) |
 * | gamma | delta per 1 unit of underlying |
 * | vega  | per **1 volatility point** (a move from 40% to 41%) |
 * | theta | per **calendar day** |
 * | rho   | per **1 percentage point** of rate |
 *
 * Nothing here touches the network or the DOM. Like `implied.ts`, it is checked
 * against arithmetic and against the venue's own published Greeks.
 */

import type { OptionGreeks, OptionType } from './types.js';

/** Seconds in a 365-day year. Act/365 is the convention every options desk quotes on. */
const YEAR_SECONDS = 365 * 86400;

/** Below this many years to expiry the model degenerates and Greeks blow up. */
const MIN_YEARS = 1e-8;

/** Volatility search bounds. 500% covers 0DTE crypto; 0.01% is effectively zero. */
const MIN_VOL = 1e-4;
const MAX_VOL = 5;

/* ------------------------------------------------------------ distributions */

/** Standard normal pdf. */
export function normalPdf(x: number): number {
  return Math.exp(-0.5 * x * x) / Math.sqrt(2 * Math.PI);
}

/**
 * Standard normal cdf, to full double precision (Hart's rational
 * approximation, in West's arrangement).
 *
 * The obvious choice — Abramowitz & Stegun 7.1.26 — is accurate to 1.5e-7
 * absolute, which sounds far finer than any tick size and is not. In the wings
 * `N(d1)` and `N(d2)` are themselves of order 1e-8, so an *absolute* error of
 * 1.5e-7 is larger than the quantities being subtracted, and the price of a far
 * out-of-the-money contract becomes mostly approximation error. That is the one
 * place it matters most: the wings are where a volatility smile is read, and a
 * mispriced wing implies a wrong vol there.
 *
 * This version is accurate to ~1e-15 relative in the tails as well as the body,
 * for about the same arithmetic.
 */
export function normalCdf(x: number): number {
  const z = Math.abs(x);

  // Beyond 37 standard deviations the tail underflows a double anyway.
  if (z > 37) return x > 0 ? 1 : 0;

  const e = Math.exp((-z * z) / 2);
  let tail: number;

  if (z < 7.071067811865475) {
    let numerator = 3.52624965998911e-2 * z + 0.700383064443688;
    numerator = numerator * z + 6.37396220353165;
    numerator = numerator * z + 33.912866078383;
    numerator = numerator * z + 112.079291497871;
    numerator = numerator * z + 221.213596169931;
    numerator = numerator * z + 220.206867912376;

    let denominator = 8.83883476483184e-2 * z + 1.75566716318264;
    denominator = denominator * z + 16.064177579207;
    denominator = denominator * z + 86.7807322029461;
    denominator = denominator * z + 296.564248779674;
    denominator = denominator * z + 637.333633378831;
    denominator = denominator * z + 793.826512519948;
    denominator = denominator * z + 440.413735824752;

    tail = (e * numerator) / denominator;
  } else {
    // Continued fraction, which is the stable form far out in the tail.
    let b = z + 0.65;
    b = z + 4 / b;
    b = z + 3 / b;
    b = z + 2 / b;
    b = z + 1 / b;
    tail = e / (b * 2.506628274631);
  }

  return x > 0 ? 1 - tail : tail;
}

/* ------------------------------------------------------------------- inputs */

/**
 * Everything the model needs, all of it observable.
 *
 * `spot` is separate from `forward` on purpose: the difference between them is
 * the carry, and the carry is what turns a forward delta into the spot delta a
 * hedger actually trades.
 */
export interface BlackInputs {
  /** Underlying price now. */
  spot: number;
  /** Forward price for this expiry. */
  forward: number;
  strike: number;
  /** Time to expiry in years, act/365. */
  years: number;
  /** Volatility as a decimal — `0.42`, not `42`. */
  vol: number;
  /** `e^(-rT)`. `1` means undiscounted. */
  discount: number;
  type: OptionType;
}

interface Terms {
  d1: number;
  d2: number;
  sqrtT: number;
  /** `DF·F`, which equals `S·e^(-qT)` — the discounted forward. */
  dfF: number;
  /** Continuous rate implied by the discount factor. */
  rate: number;
  /** Continuous carry (dividend/borrow) yield implied by spot vs forward. */
  carry: number;
}

function terms(inputs: BlackInputs): Terms | null {
  const { spot, forward, strike, years, vol, discount } = inputs;
  if (
    !Number.isFinite(forward) ||
    !Number.isFinite(strike) ||
    !Number.isFinite(spot) ||
    forward <= 0 ||
    strike <= 0 ||
    spot <= 0 ||
    years <= MIN_YEARS ||
    vol <= 0 ||
    discount <= 0
  ) {
    return null;
  }

  const sqrtT = Math.sqrt(years);
  const d1 = (Math.log(forward / strike) + 0.5 * vol * vol * years) / (vol * sqrtT);
  const rate = -Math.log(discount) / years;

  return {
    d1,
    d2: d1 - vol * sqrtT,
    sqrtT,
    dfF: discount * forward,
    rate,
    // F = S·e^((r-q)T)  ⇒  q = r - ln(F/S)/T. Recovered rather than assumed.
    carry: rate - Math.log(forward / spot) / years,
  };
}

/* ------------------------------------------------------------------ pricing */

/**
 * Black-76 value, in the underlying's currency.
 *
 * Returns the intrinsic value at zero vol or zero time rather than `null`: an
 * expiring option is still worth something, and a chain that blanked its last
 * column on expiry day would be hiding the most-traded contracts on the board.
 */
export function blackPrice(inputs: BlackInputs): number | null {
  const { forward, strike, years, vol, discount, type } = inputs;
  if (!Number.isFinite(forward) || !Number.isFinite(strike) || forward <= 0 || strike <= 0) {
    return null;
  }

  if (years <= MIN_YEARS || vol <= 0) {
    const intrinsic = type === 'call' ? forward - strike : strike - forward;
    return Math.max(0, intrinsic) * (Number.isFinite(discount) && discount > 0 ? discount : 1);
  }

  const t = terms(inputs);
  if (!t) return null;

  return type === 'call'
    ? discount * (forward * normalCdf(t.d1) - strike * normalCdf(t.d2))
    : discount * (strike * normalCdf(-t.d2) - forward * normalCdf(-t.d1));
}

/**
 * The full Greek set, in trader units.
 *
 * Every one is the classical Black-Scholes partial re-expressed through
 * `(S, F, DF)`; the identity `S·e^(-qT) = DF·F` is what lets the whole set be
 * written without `r` and `q` ever appearing as inputs.
 */
export function blackGreeks(inputs: BlackInputs): OptionGreeks {
  const empty: OptionGreeks = { delta: null, gamma: null, vega: null, theta: null, rho: null };
  const t = terms(inputs);
  if (!t) return empty;

  const { spot, strike, years, vol, discount, type } = inputs;
  const { d1, d2, sqrtT, dfF, rate, carry } = t;
  const pdf = normalPdf(d1);

  // Shared across both option types.
  const gamma = (dfF * pdf) / (spot * spot * vol * sqrtT);
  const vega = dfF * pdf * sqrtT;
  const decay = (-dfF * pdf * vol) / (2 * sqrtT);

  if (type === 'call') {
    const nd1 = normalCdf(d1);
    const nd2 = normalCdf(d2);
    return {
      delta: (dfF * nd1) / spot,
      gamma,
      vega: vega / 100,
      theta: (decay - rate * strike * discount * nd2 + carry * dfF * nd1) / 365,
      rho: (strike * years * discount * nd2) / 100,
    };
  }

  const nmd1 = normalCdf(-d1);
  const nmd2 = normalCdf(-d2);
  return {
    delta: -(dfF * nmd1) / spot,
    gamma,
    vega: vega / 100,
    theta: (decay + rate * strike * discount * nmd2 - carry * dfF * nmd1) / 365,
    rho: -(strike * years * discount * nmd2) / 100,
  };
}

/* -------------------------------------------------------- implied volatility */

/**
 * Invert {@link blackPrice} for the volatility.
 *
 * Safeguarded Newton: take the Newton step when it stays inside the bracket and
 * bisect when it does not. Pure Newton is the wrong tool alone here because
 * vega collapses to zero in both wings, and a deep out-of-the-money contract
 * quoted at one tick is *exactly* where a chain most wants an answer.
 *
 * Returns `null` when no volatility can produce the price — a quote below
 * intrinsic or above the forward is arbitrage or a stale print, not a vol.
 * Reporting it as `0` or as the bracket edge would draw a smile that dives to
 * the floor on the strikes with the widest spreads.
 */
export function impliedVol(
  price: number | null,
  inputs: Omit<BlackInputs, 'vol'>,
): number | null {
  const { forward, strike, years, discount, type } = inputs;
  if (price === null || !Number.isFinite(price) || price <= 0) return null;
  if (years <= MIN_YEARS || forward <= 0 || strike <= 0 || discount <= 0) return null;

  // No vol can price below intrinsic or above the maximum payoff.
  const intrinsic = Math.max(0, type === 'call' ? forward - strike : strike - forward) * discount;
  const ceiling = discount * (type === 'call' ? forward : strike);
  if (price <= intrinsic || price >= ceiling) return null;

  let lo = MIN_VOL;
  let hi = MAX_VOL;
  let vol = 0.5;

  // A price outside the bracket's reach has no solution to find.
  if ((blackPrice({ ...inputs, vol: hi }) ?? 0) < price) return null;

  // Convergence is judged *relative* to the price. An absolute tolerance looks
  // reasonable and silently breaks the wings: a far out-of-the-money contract
  // can be worth 1e-39, and every volatility from 1% to 500% then satisfies
  // `|error| < 1e-10` — so the solver stops at whichever one it happened to try
  // and reports a vol that has nothing to do with the quote.
  const tolerance = price * 1e-10;

  for (let i = 0; i < 64; i++) {
    const value = blackPrice({ ...inputs, vol });
    if (value === null) return null;

    const diff = value - price;
    if (Math.abs(diff) <= tolerance) return vol;

    if (diff > 0) hi = vol;
    else lo = vol;

    const vega = dPriceDVol({ ...inputs, vol });
    const step = vega > 1e-12 ? vol - diff / vega : Number.NaN;

    // Newton when it lands inside the bracket; bisection when it bolts.
    vol = Number.isFinite(step) && step > lo && step < hi ? step : 0.5 * (lo + hi);

    if (hi - lo < 1e-12) break;
  }

  // A solution pinned to a bracket edge is the solver giving up, not an answer.
  return vol > MIN_VOL * 2 && vol < MAX_VOL - 1e-6 ? vol : null;
}

/** Raw `∂V/∂σ` — the un-scaled vega the solver needs, not the per-point one. */
function dPriceDVol(inputs: BlackInputs): number {
  const t = terms(inputs);
  if (!t) return 0;
  return t.dfF * normalPdf(t.d1) * t.sqrtT;
}

/* ------------------------------------------------------- forward from parity */

/** A strike where both legs are quoted, so parity has something to say. */
export interface ParityPair {
  strike: number;
  callPrice: number;
  putPrice: number;
}

export interface ForwardFit {
  forward: number;
  /** `e^(-rT)`. */
  discount: number;
  /** Continuous rate implied by the fit. */
  rate: number;
  /** Continuous dividend/borrow yield implied by the fit. */
  carry: number;
  /** Strikes the regression actually used. */
  used: number;
}

/**
 * Recover the forward and the discount factor from quoted option prices.
 *
 * `C - P = DF·(F - K)` is linear in `K` with slope `-DF` and intercept `DF·F`,
 * so an ordinary least-squares line through the call-put spread hands back
 * both. Using several strikes rather than the single at-the-money pair matters:
 * each spread carries two bid/ask spreads of noise, and the regression averages
 * that out instead of inheriting whichever strike happened to be crossed.
 *
 * Only near-the-money strikes are eligible, for a reason beyond noise. Listed
 * equity options are **American**, and parity is a European identity — a deep
 * in-the-money put can carry an early-exercise premium that parity reads as a
 * distorted forward. That premium is negligible around the money, which is also
 * where the books are tightest, so the same window fixes both problems.
 *
 * Returns `null` when the fit cannot be trusted, rather than a plausible
 * number. The caller is expected to say so out loud.
 */
export function fitForward(
  pairs: ParityPair[],
  spot: number,
  years: number,
  window = 0.1,
  assumedRate = 0.04,
): ForwardFit | null {
  if (!Number.isFinite(spot) || spot <= 0 || years <= MIN_YEARS) return null;

  // The window comparison is deliberately tolerant at its boundary. Strikes
  // land on round numbers, so a strike exactly `window` away from spot is
  // common — and `110 / 100 - 1` is `0.10000000000000009` in binary floating
  // point, which a strict `<= 0.1` throws away. Losing a boundary strike can
  // drop a sparse board below the three pairs a regression needs, turning a
  // perfectly fittable forward into an assumed one.
  const limit = window * (1 + 1e-9);

  const eligible = pairs.filter(
    (p) =>
      Number.isFinite(p.strike) &&
      Number.isFinite(p.callPrice) &&
      Number.isFinite(p.putPrice) &&
      p.strike > 0 &&
      p.callPrice > 0 &&
      p.putPrice > 0 &&
      Math.abs(p.strike / spot - 1) <= limit,
  );

  if (eligible.length < 3) return null;

  let sumK = 0;
  let sumY = 0;
  let sumKK = 0;
  let sumKY = 0;
  for (const p of eligible) {
    const y = p.callPrice - p.putPrice;
    sumK += p.strike;
    sumY += y;
    sumKK += p.strike * p.strike;
    sumKY += p.strike * y;
  }

  const n = eligible.length;
  const denominator = n * sumKK - sumK * sumK;
  // Every eligible strike was the same number — a one-strike ladder in disguise.
  if (Math.abs(denominator) < 1e-9) return null;

  const slope = (n * sumKY - sumK * sumY) / denominator;
  const intercept = (sumY - slope * sumK) / n;

  const discount = -slope;
  const forward = intercept / discount;
  const rate = -Math.log(discount) / years;

  // Sanity gates. A regression through crossed or stale quotes can produce an
  // arithmetically fine line that is financially nonsense.
  const usable =
    Number.isFinite(discount) &&
    discount > 0 &&
    Number.isFinite(forward) &&
    forward > 0 &&
    Math.abs(forward / spot - 1) <= 0.3 &&
    // The gate that matters most in practice, and the one that is easy to miss.
    // The slope is a discount factor, so over a *one-day* expiry it is ~0.999,
    // and a couple of cents of bid/ask noise on it annualises to a 30%+ rate.
    // A negative fitted rate is the same failure in the other direction: it
    // means the regression put the discount factor above 1, which no USD
    // expiry does. Neither is a small error — the discount factor scales every
    // price on the board, and a 1.6% scaling moves at-the-money implied
    // volatility by about two points.
    //
    // The forward survives both cases: it is the *intercept*, which stays well
    // determined even when the slope is noise.
    rate >= MIN_PLAUSIBLE_RATE &&
    rate <= MAX_PLAUSIBLE_RATE;

  if (usable) {
    return {
      forward,
      discount,
      rate,
      carry: rate - Math.log(forward / spot) / years,
      used: n,
    };
  }

  // The two-parameter fit failed, but parity still knows where the forward is.
  // Pin the discount factor to the assumed rate and solve for the forward
  // alone: `F = K + (C - P) / DF`, averaged across the eligible strikes. One
  // parameter is far better conditioned than two, and it keeps the dividend and
  // borrow information that a bare `F = S/DF` would throw away.
  const pinnedDiscount = Math.exp(-assumedRate * years);
  let total = 0;
  for (const p of eligible) total += p.strike + (p.callPrice - p.putPrice) / pinnedDiscount;
  const pinnedForward = total / n;

  if (!Number.isFinite(pinnedForward) || pinnedForward <= 0) return null;
  if (Math.abs(pinnedForward / spot - 1) > 0.3) return null;

  return {
    forward: pinnedForward,
    discount: pinnedDiscount,
    rate: assumedRate,
    carry: assumedRate - Math.log(pinnedForward / spot) / years,
    used: n,
  };
}

/**
 * The band an annualised rate from a parity fit is allowed to fall in.
 *
 * Not a view on where rates can go — a filter on where a *regression through
 * option quotes* can be trusted. Everything here is USD-denominated (Deribit
 * settles against USD indices, the equity board is US listed), so a discount
 * factor above 1 is a fitting artefact rather than a market. The small negative
 * allowance absorbs ordinary quote noise without admitting the artefacts.
 */
const MIN_PLAUSIBLE_RATE = -0.02;
const MAX_PLAUSIBLE_RATE = 0.25;

/* ---------------------------------------------------------------- moneyness */

/** Years between two unix-second instants, act/365, floored at zero. */
export function yearsToExpiry(expirySeconds: number, nowSeconds: number): number {
  return Math.max(0, (expirySeconds - nowSeconds) / YEAR_SECONDS);
}

/**
 * `K/S`. Above 1 is a high strike, whichever way the contract pays.
 *
 * Deliberately not "percent out of the money": that flips sign between calls
 * and puts, and a smile has to plot both on one axis.
 */
export function moneyness(strike: number, spot: number): number | null {
  if (!Number.isFinite(strike) || !Number.isFinite(spot) || spot <= 0) return null;
  return strike / spot;
}

/** Value at expiry if the underlying never moves from `spot`. */
export function intrinsicValue(type: OptionType, strike: number, spot: number): number | null {
  if (!Number.isFinite(strike) || !Number.isFinite(spot)) return null;
  return Math.max(0, type === 'call' ? spot - strike : strike - spot);
}

/**
 * Where the underlying has to be at expiry for this contract to return its
 * premium — the number that decides whether a trade is worth putting on.
 */
export function breakeven(type: OptionType, strike: number, premium: number | null): number | null {
  if (premium === null || !Number.isFinite(premium) || !Number.isFinite(strike)) return null;
  return type === 'call' ? strike + premium : strike - premium;
}

/**
 * The strike where the most option value expires worthless — "max pain".
 *
 * For each candidate settlement price, total what every open contract would pay
 * out; the minimum is the price that costs writers least. It is a positioning
 * read rather than a forecast, and the panel labels it as one, but it is the
 * single most-asked-for number off an open-interest ladder.
 */
export function maxPain(
  strikes: { strike: number; callOpenInterest: number; putOpenInterest: number }[],
): { strike: number; payout: number } | null {
  const rungs = strikes.filter((s) => Number.isFinite(s.strike) && s.strike > 0);
  if (rungs.length === 0) return null;

  let best: { strike: number; payout: number } | null = null;

  for (const candidate of rungs) {
    let payout = 0;
    for (const rung of rungs) {
      payout += Math.max(0, candidate.strike - rung.strike) * (rung.callOpenInterest || 0);
      payout += Math.max(0, rung.strike - candidate.strike) * (rung.putOpenInterest || 0);
    }
    if (best === null || payout < best.payout) best = { strike: candidate.strike, payout };
  }

  return best;
}

/* --------------------------------------------------------------- expiry time */

/**
 * The instant a US listed option stops trading: 16:00 America/New_York.
 *
 * Hard-coding 20:00 UTC would be an hour out for five months of the year, and
 * an hour is not a rounding error on an expiry-day chain — it is a third of the
 * remaining life of a 0DTE contract, and theta and gamma both scale on it. The
 * offset is read from the runtime's own timezone database instead of a DST rule
 * table that would need maintaining.
 */
export function equityExpiryInstant(isoDate: string): number | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(isoDate.trim());
  if (!match) return null;
  const [, year, month, day] = match;

  // Start from 16:00 "UTC" on the date, ask New York what wall-clock time that
  // is, and shift by the difference. One correction is exact for every case
  // except the ambiguous hour of a DST transition, which is 06:00 local and
  // never an expiry.
  const naive = Date.UTC(Number(year), Number(month) - 1, Number(day), 16, 0, 0);
  const offsetMs = newYorkOffsetMs(naive);
  return Math.floor((naive + offsetMs) / 1000);
}

const NEW_YORK = new Intl.DateTimeFormat('en-US', {
  timeZone: 'America/New_York',
  hour12: false,
  year: 'numeric',
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
});

/** How far behind UTC New York is at `utcMs`, in milliseconds (positive). */
function newYorkOffsetMs(utcMs: number): number {
  const parts = NEW_YORK.formatToParts(new Date(utcMs));
  const read = (type: string): number => Number(parts.find((p) => p.type === type)?.value ?? '0');
  const local = Date.UTC(
    read('year'),
    read('month') - 1,
    read('day'),
    // `hour12: false` renders midnight as 24 in some ICU versions.
    read('hour') % 24,
    read('minute'),
    read('second'),
  );
  return utcMs - local;
}
