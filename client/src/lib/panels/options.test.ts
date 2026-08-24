/**
 * The option panels' units.
 *
 * Every one of these is a scaling that looks right when it is wrong. A vol
 * printed one `* 100` too far reads as `4335.0%` — obviously broken. A vega
 * quoted per unit rather than per point reads as a plausible number that is
 * exactly 100 times too large, and nothing on screen says so. So the units are
 * pinned here rather than trusted to three copies of a formatter.
 */

import { describe, expect, it } from 'vitest';

import { forwardTone, forwardWord, greek, rate, skew, vol } from './options';

describe('vol', () => {
  it('prints a decimal fraction as a percentage, scaling exactly once', () => {
    // 0.4335 is a 43.35% volatility — an ordinary one for Bitcoin. Scaling it
    // twice gives `4335.0%`, which is what this exists to stop.
    expect(vol(0.4335)).toBe('43.4%');
    expect(vol(0.4335, 2)).toBe('43.35%');
    expect(vol(0.08)).toBe('8.0%');
  });

  it('says nothing rather than zero when there is no volatility', () => {
    expect(vol(null)).toBe('--');
    expect(vol(undefined)).toBe('--');
  });
});

describe('rate and skew', () => {
  it('prints a carry as a percentage of the same kind', () => {
    expect(rate(0.0459)).toBe('4.59%');
    expect(rate(-0.0121)).toBe('-1.21%');
    expect(rate(null)).toBe('--');
  });

  it('signs a risk reversal, because its sign is the whole reading', () => {
    // Positive means the downside wing is bid.
    expect(skew(0.0312)).toBe('+3.12%');
    expect(skew(-0.0088)).toBe('-0.88%');
    expect(skew(null)).toBe('--');
  });
});

describe('greek', () => {
  it('keeps a small Greek legible and a large one honest', () => {
    expect(greek(0.35047, 4)).toBe('0.3505');
    expect(greek(-0.421, 3)).toBe('-0.421');
    // Gamma on a $77,000 underlying is genuinely of order 1e-5.
    expect(greek(0.0000102, 6)).toBe('0.000010');
  });

  it('distinguishes an unknown sensitivity from a zero one', () => {
    expect(greek(null, 4)).toBe('--');
    expect(greek(0, 4)).toBe('0.0000');
  });
});

describe('forwardWord and forwardTone', () => {
  it('names each of the three qualities of answer', () => {
    expect(forwardWord('venue')).toBe('venue');
    expect(forwardWord('parity')).toBe('parity');
    expect(forwardWord('assumed')).toBe('assumed');
  });

  it('warns only on the one that was not observed', () => {
    // A venue forward and a fitted one are both quotes; an assumed one is not,
    // and every Greek derived from it is a guess.
    expect(forwardTone('venue')).toBe('dim');
    expect(forwardTone('parity')).toBe('dim');
    expect(forwardTone('assumed')).toBe('warn');
  });
});
