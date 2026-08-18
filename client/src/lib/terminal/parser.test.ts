/**
 * Command-line parsing tests.
 *
 * The parser is the terminal's whole input surface, so the cases that matter
 * are the ambiguous ones: quoted arguments, `1d` meaning a bar size in one
 * position and a look-back in another, and arguments arriving out of order.
 *
 * The suites that read `1d` positionally — `parseChartArgs`, `parseSpotArgs`,
 * `parseNewsArgs`, `guessAssetClass` — live in the command registry, which
 * reaches the panels and so has not crossed to this client yet. They come back
 * with it, unchanged.
 */

import { describe, expect, it } from 'vitest';
import {
  isIsoDate,
  looksLikeSymbol,
  looksLikeTicker,
  parse,
  parseDuration,
  parseInterval,
  tokenize,
} from './parser';

describe('tokenize', () => {
  it('splits on whitespace', () => {
    expect(tokenize('GP KXFED-1 1h')).toEqual(['GP', 'KXFED-1', '1h']);
  });

  it('keeps double-quoted arguments whole', () => {
    expect(tokenize('FSRCH "real gdp"')).toEqual(['FSRCH', 'real gdp']);
  });

  it('keeps single-quoted arguments whole', () => {
    expect(tokenize("FSRCH 'real gdp'")).toEqual(['FSRCH', 'real gdp']);
  });

  it('treats an unterminated quote as running to end of line', () => {
    // Half-typed input is normal at a prompt; it must not throw.
    expect(tokenize('FSRCH "real gdp')).toEqual(['FSRCH', 'real gdp']);
  });

  it('collapses runs of whitespace', () => {
    expect(tokenize('  SRCH   fed   rate  ')).toEqual(['SRCH', 'fed', 'rate']);
  });

  it('preserves an explicitly empty quoted argument', () => {
    expect(tokenize('CMD ""')).toEqual(['CMD', '']);
  });

  it('returns nothing for a blank line', () => {
    expect(tokenize('   ')).toEqual([]);
  });
});

describe('parse', () => {
  it('upper-cases the verb but preserves argument casing', () => {
    const result = parse('srch Fed Decision');
    expect(result.verb).toBe('SRCH');
    expect(result.args).toEqual(['Fed', 'Decision']);
  });

  it('collects --key=value flags', () => {
    const result = parse('GP X --interval=1h --fit');
    expect(result.args).toEqual(['X']);
    expect(result.flags).toEqual({ interval: '1h', fit: 'true' });
  });

  it('does not treat a lone -- or a negative number as a flag', () => {
    expect(parse('CMD -- -5').args).toEqual(['--', '-5']);
  });

  it('returns an empty verb for a blank line', () => {
    expect(parse('   ').verb).toBe('');
  });
});

describe('parseInterval', () => {
  it('maps aliases to Kalshi period minutes', () => {
    expect(parseInterval('1m')).toBe(1);
    expect(parseInterval('1h')).toBe(60);
    expect(parseInterval('1d')).toBe(1440);
    expect(parseInterval('daily')).toBe(1440);
    expect(parseInterval('60')).toBe(60);
  });

  it('is case-insensitive', () => {
    expect(parseInterval('1H')).toBe(60);
  });

  it('returns null for anything else', () => {
    expect(parseInterval('1y')).toBe(null);
    expect(parseInterval('banana')).toBe(null);
    expect(parseInterval(undefined)).toBe(null);
  });
});

describe('parseDuration', () => {
  it('converts durations to seconds', () => {
    expect(parseDuration('30d')).toBe(30 * 86400);
    expect(parseDuration('6h')).toBe(6 * 3600);
    expect(parseDuration('1y')).toBe(31_536_000);
    expect(parseDuration('2w')).toBe(2 * 604800);
  });

  it('reads mo as months, not minutes', () => {
    expect(parseDuration('3mo')).toBe(3 * 2_592_000);
    expect(parseDuration('3m')).toBe(3 * 60);
  });

  it('rejects zero, negatives and nonsense', () => {
    expect(parseDuration('0d')).toBe(null);
    expect(parseDuration('-5d')).toBe(null);
    expect(parseDuration('d')).toBe(null);
    expect(parseDuration(undefined)).toBe(null);
  });
});

describe('isIsoDate / looksLikeTicker', () => {
  it('accepts real ISO dates only', () => {
    expect(isIsoDate('2025-06-14')).toBe(true);
    expect(isIsoDate('2025-13-45')).toBe(false);
    expect(isIsoDate('06-14-2025')).toBe(false);
    expect(isIsoDate(undefined)).toBe(false);
  });

  it('recognises Kalshi-shaped tickers', () => {
    expect(looksLikeTicker('KXFEDDECISION-27JAN-H26')).toBe(true);
    expect(looksLikeTicker('KXHIGHNY-26AUG16-B82.5')).toBe(true);
    expect(looksLikeTicker('-bad')).toBe(false);
    expect(looksLikeTicker('X')).toBe(false);
  });
});

describe('looksLikeSymbol', () => {
  it('accepts the shapes a market symbol actually takes', () => {
    // Looser than a Kalshi ticker on purpose: cash indices carry a caret, FX
    // pairs an equals, and `F` is a real NYSE listing.
    for (const symbol of ['AAPL', '^GSPC', 'EURUSD=X', 'BTC-USD', 'BRK.B', 'F']) {
      expect(looksLikeSymbol(symbol), symbol).toBe(true);
    }
  });

  it('rejects tokens that are not symbols at all', () => {
    for (const token of ['', '--flag', '/', undefined]) {
      expect(looksLikeSymbol(token as string)).toBe(false);
    }
  });
});
