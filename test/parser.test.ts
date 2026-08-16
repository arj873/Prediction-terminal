/**
 * Command-line parsing tests.
 *
 * The parser is the terminal's whole input surface, so the cases that matter
 * are the ambiguous ones: quoted arguments, `1d` meaning a bar size in one
 * position and a look-back in another, and arguments arriving out of order.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  isIsoDate,
  looksLikeSymbol,
  looksLikeTicker,
  parse,
  parseDuration,
  parseInterval,
  tokenize,
} from '../src/client/terminal/parser.js';
import {
  guessAssetClass,
  parseChartArgs,
  parseNewsArgs,
  parseSpotArgs,
} from '../src/client/terminal/registry.js';

describe('tokenize', () => {
  it('splits on whitespace', () => {
    assert.deepEqual(tokenize('GP KXFED-1 1h'), ['GP', 'KXFED-1', '1h']);
  });

  it('keeps double-quoted arguments whole', () => {
    assert.deepEqual(tokenize('FSRCH "real gdp"'), ['FSRCH', 'real gdp']);
  });

  it('keeps single-quoted arguments whole', () => {
    assert.deepEqual(tokenize("FSRCH 'real gdp'"), ['FSRCH', 'real gdp']);
  });

  it('treats an unterminated quote as running to end of line', () => {
    // Half-typed input is normal at a prompt; it must not throw.
    assert.deepEqual(tokenize('FSRCH "real gdp'), ['FSRCH', 'real gdp']);
  });

  it('collapses runs of whitespace', () => {
    assert.deepEqual(tokenize('  SRCH   fed   rate  '), ['SRCH', 'fed', 'rate']);
  });

  it('preserves an explicitly empty quoted argument', () => {
    assert.deepEqual(tokenize('CMD ""'), ['CMD', '']);
  });

  it('returns nothing for a blank line', () => {
    assert.deepEqual(tokenize('   '), []);
  });
});

describe('parse', () => {
  it('upper-cases the verb but preserves argument casing', () => {
    const result = parse('srch Fed Decision');
    assert.equal(result.verb, 'SRCH');
    assert.deepEqual(result.args, ['Fed', 'Decision']);
  });

  it('collects --key=value flags', () => {
    const result = parse('GP X --interval=1h --fit');
    assert.deepEqual(result.args, ['X']);
    assert.deepEqual(result.flags, { interval: '1h', fit: 'true' });
  });

  it('does not treat a lone -- or a negative number as a flag', () => {
    assert.deepEqual(parse('CMD -- -5').args, ['--', '-5']);
  });

  it('returns an empty verb for a blank line', () => {
    assert.equal(parse('   ').verb, '');
  });
});

describe('parseInterval', () => {
  it('maps aliases to Kalshi period minutes', () => {
    assert.equal(parseInterval('1m'), 1);
    assert.equal(parseInterval('1h'), 60);
    assert.equal(parseInterval('1d'), 1440);
    assert.equal(parseInterval('daily'), 1440);
    assert.equal(parseInterval('60'), 60);
  });

  it('is case-insensitive', () => {
    assert.equal(parseInterval('1H'), 60);
  });

  it('returns null for anything else', () => {
    assert.equal(parseInterval('1y'), null);
    assert.equal(parseInterval('banana'), null);
    assert.equal(parseInterval(undefined), null);
  });
});

describe('parseDuration', () => {
  it('converts durations to seconds', () => {
    assert.equal(parseDuration('30d'), 30 * 86400);
    assert.equal(parseDuration('6h'), 6 * 3600);
    assert.equal(parseDuration('1y'), 31_536_000);
    assert.equal(parseDuration('2w'), 2 * 604800);
  });

  it('reads mo as months, not minutes', () => {
    assert.equal(parseDuration('3mo'), 3 * 2_592_000);
    assert.equal(parseDuration('3m'), 3 * 60);
  });

  it('rejects zero, negatives and nonsense', () => {
    assert.equal(parseDuration('0d'), null);
    assert.equal(parseDuration('-5d'), null);
    assert.equal(parseDuration('d'), null);
    assert.equal(parseDuration(undefined), null);
  });
});

describe('isIsoDate / looksLikeTicker', () => {
  it('accepts real ISO dates only', () => {
    assert.equal(isIsoDate('2025-06-14'), true);
    assert.equal(isIsoDate('2025-13-45'), false);
    assert.equal(isIsoDate('06-14-2025'), false);
    assert.equal(isIsoDate(undefined), false);
  });

  it('recognises Kalshi-shaped tickers', () => {
    assert.equal(looksLikeTicker('KXFEDDECISION-27JAN-H26'), true);
    assert.equal(looksLikeTicker('KXHIGHNY-26AUG16-B82.5'), true);
    assert.equal(looksLikeTicker('-bad'), false);
    assert.equal(looksLikeTicker('X'), false);
  });
});

describe('parseChartArgs', () => {
  it('defaults to hourly bars over 30 days', () => {
    const result = parseChartArgs(['KXFED-1']);
    assert.deepEqual(result.ref, { venue: 'kalshi', id: 'KXFED-1' });
    assert.equal(result.interval, 60);
    assert.equal(result.lookbackSeconds, 30 * 86400);
    assert.equal(result.style, 'candle');
  });

  it('reads an unprefixed ticker as Kalshi, upper-cased', () => {
    assert.deepEqual(parseChartArgs(['kxfed-1']).ref, { venue: 'kalshi', id: 'KXFED-1' });
  });

  it('reads a venue prefix, and keeps a Polymarket slug lower-case', () => {
    assert.deepEqual(parseChartArgs(['pm:Fed-Decision-In-October']).ref, {
      venue: 'polymarket',
      id: 'fed-decision-in-october',
    });
    assert.deepEqual(parseChartArgs(['pmus:usfed-fomc-2026-10-28']).ref, {
      venue: 'polymarket-us',
      id: 'usfed-fomc-2026-10-28',
    });
  });

  it('reads interval, range and style in any order', () => {
    const a = parseChartArgs(['X-1', '1d', '1y', 'line']);
    const b = parseChartArgs(['X-1', 'line', '1y', '1d']);
    assert.deepEqual(a, b);
    assert.equal(a.interval, 1440);
    assert.equal(a.lookbackSeconds, 31_536_000);
    assert.equal(a.style, 'line');
  });

  it('reads a leading 1d as the bar size, not the window', () => {
    // `1d` is both a valid interval and a valid duration. Someone typing
    // `GP X 1d` means daily bars far more often than a one-day window.
    const result = parseChartArgs(['X-1', '1d']);
    assert.equal(result.interval, 1440);
    assert.equal(result.lookbackSeconds, 365 * 86400);
  });

  it('reads a second interval-shaped token as the window', () => {
    const result = parseChartArgs(['X-1', '1h', '1d']);
    assert.equal(result.interval, 60);
    assert.equal(result.lookbackSeconds, 86400);
  });

  it('picks a default window that matches the bar size', () => {
    assert.equal(parseChartArgs(['X-1', '1m']).lookbackSeconds, 6 * 3600);
    assert.equal(parseChartArgs(['X-1', '1d']).lookbackSeconds, 365 * 86400);
  });

  it('accepts area as a synonym for line', () => {
    assert.equal(parseChartArgs(['X-1', 'area']).style, 'line');
  });

  it('rejects a missing ticker', () => {
    assert.throws(() => parseChartArgs([]), /Missing <ticker>/);
  });

  it('rejects an argument it cannot classify', () => {
    assert.throws(() => parseChartArgs(['X-1', 'weekly']), /Unrecognised argument "weekly"/);
  });
});

describe('looksLikeSymbol', () => {
  it('accepts the shapes a market symbol actually takes', () => {
    // Looser than a Kalshi ticker on purpose: cash indices carry a caret, FX
    // pairs an equals, and `F` is a real NYSE listing.
    for (const symbol of ['AAPL', '^GSPC', 'EURUSD=X', 'BTC-USD', 'BRK.B', 'F']) {
      assert.ok(looksLikeSymbol(symbol), symbol);
    }
  });

  it('rejects tokens that are not symbols at all', () => {
    for (const token of ['', '--flag', '/', undefined]) {
      assert.ok(!looksLikeSymbol(token as string));
    }
  });
});

describe('parseSpotArgs', () => {
  const stock = { assetClass: 'stock' as const, withPicker: false };

  it('reads the symbol and defaults the rest', () => {
    const result = parseSpotArgs(['aapl'], stock);
    assert.equal(result.symbol, 'AAPL');
    assert.equal(result.interval, 60);
    assert.equal(result.lookbackSeconds, 30 * 86400);
    assert.equal(result.style, 'candle');
    assert.equal(result.method, 'median');
    assert.deepEqual(result.overlays, []);
  });

  it('accepts interval, window and style in any order', () => {
    const a = parseSpotArgs(['AAPL', '1d', '1y', 'line'], stock);
    const b = parseSpotArgs(['AAPL', 'line', '1y', '1d'], stock);
    assert.deepEqual(a, b);
    assert.equal(a.interval, 1440);
    assert.equal(a.lookbackSeconds, 31_536_000);
    assert.equal(a.style, 'line');
  });

  it('collects hyphenated arguments as Kalshi event tickers to overlay', () => {
    const result = parseSpotArgs(['BTC', 'KXBTCD-26AUG1617', '1h', '7d'], {
      assetClass: 'crypto',
      withPicker: true,
    });
    assert.deepEqual(result.overlays, ['KXBTCD-26AUG1617']);
    assert.equal(result.interval, 60);
    assert.equal(result.lookbackSeconds, 7 * 86400);
  });

  it('reads the first argument as the symbol even when it is hyphenated', () => {
    // `BTC-USD` is a symbol, not an event ticker to overlay.
    const result = parseSpotArgs(['BTC-USD'], { assetClass: 'crypto', withPicker: false });
    assert.equal(result.symbol, 'BTC-USD');
    assert.deepEqual(result.overlays, []);
  });

  it('takes the implied method as a bare word', () => {
    assert.equal(parseSpotArgs(['BTC', 'mean'], stock).method, 'mean');
    assert.equal(parseSpotArgs(['BTC', 'median'], stock).method, 'median');
  });

  it('rejects a missing symbol', () => {
    assert.throws(() => parseSpotArgs([], stock), /Missing <symbol>/);
  });

  it('rejects an argument it cannot classify', () => {
    assert.throws(() => parseSpotArgs(['AAPL', 'weekly'], stock), /Unrecognised argument "weekly"/);
  });
});

describe('parseNewsArgs', () => {
  it('defaults to the whole wire', () => {
    assert.deepEqual(parseNewsArgs([]), { symbols: [], limit: 30, days: 7 });
  });

  it('reads a bare integer as a headline count, not a symbol', () => {
    // `30` satisfies the symbol shape too, so the order these are claimed in is
    // the whole difference between 30 headlines and a ticker called 30.
    const result = parseNewsArgs(['NVDA', '50']);
    assert.deepEqual(result.symbols, ['NVDA']);
    assert.equal(result.limit, 50);
  });

  it('reads a duration as the look-back window', () => {
    assert.equal(parseNewsArgs(['NVDA', '30d']).days, 30);
    assert.equal(parseNewsArgs(['NVDA', '1w']).days, 7);
    // Sub-day windows round up rather than asking for zero days of news.
    assert.equal(parseNewsArgs(['NVDA', '6h']).days, 1);
  });

  it('takes symbols, count and window in any order', () => {
    assert.deepEqual(parseNewsArgs(['aapl', '10', 'msft', '14d']), {
      symbols: ['AAPL', 'MSFT'],
      limit: 10,
      days: 14,
    });
  });

  it('de-duplicates symbols so the panel id is stable', () => {
    assert.deepEqual(parseNewsArgs(['AAPL', 'aapl']).symbols, ['AAPL']);
  });

  it('rejects a count the upstream cannot serve', () => {
    assert.throws(() => parseNewsArgs(['NVDA', '500']), /between 1 and 50/);
    assert.throws(() => parseNewsArgs(['NVDA', '0']), /between 1 and 50/);
  });

  it('rejects an argument it cannot classify', () => {
    assert.throws(() => parseNewsArgs(['NVDA', '--']), /Unrecognised argument/);
  });
});

describe('guessAssetClass', () => {
  it('claims the symbols Kalshi lists crypto ladders for', () => {
    assert.equal(guessAssetClass('BTC'), 'crypto');
    assert.equal(guessAssetClass('eth'), 'crypto');
    assert.equal(guessAssetClass('BTC-USD'), 'crypto');
  });

  it('treats everything else as an equity', () => {
    assert.equal(guessAssetClass('AAPL'), 'stock');
    assert.equal(guessAssetClass('^GSPC'), 'stock');
    assert.equal(guessAssetClass('SPX'), 'stock');
  });
});
