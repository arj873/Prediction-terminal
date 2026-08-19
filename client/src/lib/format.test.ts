/**
 * Formatter tests.
 *
 * These are display invariants rather than arithmetic: the panels are
 * column-aligned, so what matters is that a missing figure prints as `--` and
 * never as `NaN` or `0`, that a column's width is derived the same way for
 * every row, and that the two halves of a subtraction print at one precision.
 * Each case below is a rule the module's own comments state.
 */

import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  EM_DASH,
  cents,
  compact,
  countdown,
  day,
  direction,
  group,
  metric,
  money,
  percent,
  price,
  rankMove,
  signedCents,
  signedPercent,
  signedPrice,
  stamp,
} from './format';

describe('missing data', () => {
  /** Every numeric formatter, so a new one cannot quietly skip the rule. */
  const numeric: [string, (value: number | null | undefined) => string][] = [
    ['cents', cents],
    ['percent', percent],
    ['signedCents', signedCents],
    ['compact', compact],
    ['group', group],
    ['price', price],
    ['signedPrice', signedPrice],
    ['signedPercent', signedPercent],
    ['metric', metric],
    ['money', money],
  ];

  for (const [name, format] of numeric) {
    it(`${name} prints EM_DASH for null, undefined and NaN`, () => {
      // A blank where a price should be is a bug you want to see.
      expect(format(null)).toBe(EM_DASH);
      expect(format(undefined)).toBe(EM_DASH);
      expect(format(Number.NaN)).toBe(EM_DASH);
      expect(format(Number.POSITIVE_INFINITY)).toBe(EM_DASH);
    });
  }

  it('the time formatters print EM_DASH too', () => {
    expect(stamp(null)).toBe(EM_DASH);
    expect(stamp('')).toBe(EM_DASH);
    expect(stamp('not a date')).toBe(EM_DASH);
    expect(day(null)).toBe(EM_DASH);
    expect(day('not a date')).toBe(EM_DASH);
    expect(countdown(null)).toBe(EM_DASH);
    expect(countdown('not a date')).toBe(EM_DASH);
  });

  it('pads the dash to the column width it was given', () => {
    expect(cents(null, 6)).toBe('    --');
    expect(compact(null, 8)).toBe('      --');
  });
});

describe('cents', () => {
  it('reads a dollar price as cents', () => {
    expect(cents(0.42)).toBe('42¢');
    expect(cents(1)).toBe('100¢');
    expect(cents(0)).toBe('0¢');
  });

  it('shows a decimal only for a sub-cent tick', () => {
    expect(cents(0.425)).toBe('42.5¢');
    expect(cents(0.005)).toBe('0.5¢');
  });

  it('right-aligns to the column width', () => {
    expect(cents(0.42, 6)).toBe('   42¢');
  });
});

describe('signedCents', () => {
  it('makes the sign explicit, and holds the column when flat', () => {
    expect(signedCents(0.03)).toBe('+3¢');
    expect(signedCents(-0.12)).toBe('-12¢');
    // A leading space where the `+`/`-` would be, so a flat row still lines up.
    expect(signedCents(0)).toBe(' 0¢');
  });
});

describe('compact', () => {
  it('keeps a figure to six characters', () => {
    expect(compact(12345.6)).toBe('12.3K');
    expect(compact(1234567)).toBe('1.23M');
    expect(compact(2_500_000_000)).toBe('2.50B');
  });

  it('leaves anything under ten thousand at full precision', () => {
    expect(compact(9999)).toBe('9999');
    expect(compact(0)).toBe('0');
    expect(compact(0.5)).toBe('0.50');
  });

  it('carries the sign outside the suffix', () => {
    expect(compact(-1234567)).toBe('-1.23M');
    expect(compact(-12345.6)).toBe('-12.3K');
  });
});

describe('price', () => {
  it('picks its decimals from the magnitude', () => {
    // One formatter serves BTC at 63,058.21 and EUR/USD at 1.0842.
    expect(price(63058.21)).toBe('63,058.21');
    expect(price(1.0842)).toBe('1.0842');
    expect(price(0.008421)).toBe('0.008421');
  });

  it('takes its precision from the reference when given one', () => {
    // An $8.57 basis on a $63,000 instrument is a large-instrument number that
    // happens to be small: formatted from its own magnitude it would print
    // `-8.5714` beside `63,049.41`, two precisions for one subtraction.
    expect(price(-8.5714, 63058.21)).toBe('-8.57');
    expect(price(-8.5714)).toBe('-8.5714');
  });
});

describe('signedPrice', () => {
  it('matches price decimals and makes a gain explicit', () => {
    expect(signedPrice(8.5714, 63058.21)).toBe('+8.57');
    expect(signedPrice(-8.5714, 63058.21)).toBe('-8.57');
    expect(signedPrice(8.5714)).toBe('+8.5714');
  });
});

describe('signedPercent', () => {
  it('signs a percentage change', () => {
    expect(signedPercent(1.24)).toBe('+1.24%');
    expect(signedPercent(-0.3)).toBe('-0.30%');
  });
});

describe('percent', () => {
  it('reads a contract price as a probability', () => {
    expect(percent(0.42)).toBe('42.0%');
    expect(percent(0.42, 0)).toBe('42%');
  });
});

describe('countdown', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  function at(offsetSeconds: number): string {
    const now = new Date('2026-08-16T12:00:00.000Z');
    vi.setSystemTime(now);
    return new Date(now.getTime() + offsetSeconds * 1000).toISOString();
  }

  it('drops a unit as the close approaches', () => {
    vi.useFakeTimers();
    expect(countdown(at(3 * 86400 + 4 * 3600 + 5 * 60))).toBe('3d 04h');
    expect(countdown(at(4 * 3600 + 12 * 60 + 30))).toBe('04h 12m');
    expect(countdown(at(12 * 60 + 30))).toBe('12m 30s');
  });

  it('reads CLOSED once the target has passed', () => {
    vi.useFakeTimers();
    expect(countdown(at(-1))).toBe('CLOSED');
    expect(countdown(at(-86400))).toBe('CLOSED');
    // Exactly on the close counts as closed, not as `00m 00s`.
    expect(countdown(at(0))).toBe('CLOSED');
  });
});

describe('rankMove', () => {
  it('keeps a debut distinct from a hold', () => {
    // `move` reads a null as a debut, which is right for Billboard and wrong
    // for a chart that simply has no movement column. This reads the flag.
    expect(rankMove(null, true)).toEqual({ text: 'NEW', tone: 'new' });
    expect(rankMove(null, false)).toEqual({ text: '—', tone: 'dim' });
    expect(rankMove(0, false)).toEqual({ text: '=', tone: 'dim' });
  });

  it('tones a move by its direction', () => {
    expect(rankMove(5, false)).toEqual({ text: '+5', tone: 'up' });
    expect(rankMove(-3, false)).toEqual({ text: '-3', tone: 'down' });
  });

  it('lets the debut flag win over a move value', () => {
    expect(rankMove(4, true)).toEqual({ text: 'NEW', tone: 'new' });
  });
});

describe('direction', () => {
  it('names the class that colours a row', () => {
    expect(direction(1.5)).toBe('up');
    expect(direction(-0.1)).toBe('down');
  });

  it('treats zero and no data alike, as flat', () => {
    expect(direction(0)).toBe('flat');
    expect(direction(null)).toBe('flat');
    expect(direction(undefined)).toBe('flat');
    expect(direction(Number.NaN)).toBe('flat');
  });
});
