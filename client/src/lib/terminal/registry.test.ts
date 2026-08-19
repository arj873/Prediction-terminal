/**
 * Argument-grammar tests.
 *
 * These are the suites that read a token positionally-but-not-quite: `1d` is a
 * bar size in one place and a look-back in another, `30` is a headline count
 * before it is a ticker, and after the first argument nothing has a fixed
 * position at all. Every case below is a rule someone would otherwise have to
 * rediscover from a chart that came out wrong.
 */

import { describe, expect, it } from 'vitest';
import { guessAssetClass, parseChartArgs, parseNewsArgs, parseSpotArgs } from './registry';

describe('parseChartArgs', () => {
  it('defaults to hourly bars over 30 days', () => {
    const result = parseChartArgs(['KXFED-1']);
    expect(result.ref).toEqual({ venue: 'kalshi', id: 'KXFED-1' });
    expect(result.interval).toBe(60);
    expect(result.lookbackSeconds).toBe(30 * 86400);
    expect(result.style).toBe('candle');
  });

  it('reads an unprefixed ticker as Kalshi, upper-cased', () => {
    expect(parseChartArgs(['kxfed-1']).ref).toEqual({ venue: 'kalshi', id: 'KXFED-1' });
  });

  it('reads a venue prefix, and keeps a Polymarket slug lower-case', () => {
    expect(parseChartArgs(['pm:Fed-Decision-In-October']).ref).toEqual({
      venue: 'polymarket',
      id: 'fed-decision-in-october',
    });
    expect(parseChartArgs(['pmus:usfed-fomc-2026-10-28']).ref).toEqual({
      venue: 'polymarket-us',
      id: 'usfed-fomc-2026-10-28',
    });
  });

  it('reads interval, range and style in any order', () => {
    const a = parseChartArgs(['X-1', '1d', '1y', 'line']);
    const b = parseChartArgs(['X-1', 'line', '1y', '1d']);
    expect(a).toEqual(b);
    expect(a.interval).toBe(1440);
    expect(a.lookbackSeconds).toBe(31_536_000);
    expect(a.style).toBe('line');
  });

  it('reads a leading 1d as the bar size, not the window', () => {
    // `1d` is both a valid interval and a valid duration. Someone typing
    // `GP X 1d` means daily bars far more often than a one-day window.
    const result = parseChartArgs(['X-1', '1d']);
    expect(result.interval).toBe(1440);
    expect(result.lookbackSeconds).toBe(365 * 86400);
  });

  it('reads a second interval-shaped token as the window', () => {
    const result = parseChartArgs(['X-1', '1h', '1d']);
    expect(result.interval).toBe(60);
    expect(result.lookbackSeconds).toBe(86400);
  });

  it('picks a default window that matches the bar size', () => {
    expect(parseChartArgs(['X-1', '1m']).lookbackSeconds).toBe(6 * 3600);
    expect(parseChartArgs(['X-1', '1d']).lookbackSeconds).toBe(365 * 86400);
  });

  it('accepts area as a synonym for line', () => {
    expect(parseChartArgs(['X-1', 'area']).style).toBe('line');
  });

  it('rejects a missing ticker', () => {
    expect(() => parseChartArgs([])).toThrow(/Missing <ticker>/);
  });

  it('rejects an argument it cannot classify', () => {
    expect(() => parseChartArgs(['X-1', 'weekly'])).toThrow(/Unrecognised argument "weekly"/);
  });
});

describe('parseSpotArgs', () => {
  const stock = { assetClass: 'stock' as const, withPicker: false };

  it('reads the symbol and defaults the rest', () => {
    const result = parseSpotArgs(['aapl'], stock);
    expect(result.symbol).toBe('AAPL');
    expect(result.interval).toBe(60);
    expect(result.lookbackSeconds).toBe(30 * 86400);
    expect(result.style).toBe('candle');
    expect(result.method).toBe('median');
    expect(result.overlays).toEqual([]);
  });

  it('accepts interval, window and style in any order', () => {
    const a = parseSpotArgs(['AAPL', '1d', '1y', 'line'], stock);
    const b = parseSpotArgs(['AAPL', 'line', '1y', '1d'], stock);
    expect(a).toEqual(b);
    expect(a.interval).toBe(1440);
    expect(a.lookbackSeconds).toBe(31_536_000);
    expect(a.style).toBe('line');
  });

  it('collects hyphenated arguments as Kalshi event tickers to overlay', () => {
    const result = parseSpotArgs(['BTC', 'KXBTCD-26AUG1617', '1h', '7d'], {
      assetClass: 'crypto',
      withPicker: true,
    });
    expect(result.overlays).toEqual(['KXBTCD-26AUG1617']);
    expect(result.interval).toBe(60);
    expect(result.lookbackSeconds).toBe(7 * 86400);
  });

  it('reads the first argument as the symbol even when it is hyphenated', () => {
    // `BTC-USD` is a symbol, not an event ticker to overlay.
    const result = parseSpotArgs(['BTC-USD'], { assetClass: 'crypto', withPicker: false });
    expect(result.symbol).toBe('BTC-USD');
    expect(result.overlays).toEqual([]);
  });

  it('takes the implied method as a bare word', () => {
    expect(parseSpotArgs(['BTC', 'mean'], stock).method).toBe('mean');
    expect(parseSpotArgs(['BTC', 'median'], stock).method).toBe('median');
  });

  it('rejects a missing symbol', () => {
    expect(() => parseSpotArgs([], stock)).toThrow(/Missing <symbol>/);
  });

  it('rejects an argument it cannot classify', () => {
    expect(() => parseSpotArgs(['AAPL', 'weekly'], stock)).toThrow(
      /Unrecognised argument "weekly"/,
    );
  });
});

describe('parseNewsArgs', () => {
  it('defaults to the whole wire', () => {
    expect(parseNewsArgs([])).toEqual({ symbols: [], limit: 30, days: 7 });
  });

  it('reads a bare integer as a headline count, not a symbol', () => {
    // `30` satisfies the symbol shape too, so the order these are claimed in is
    // the whole difference between 30 headlines and a ticker called 30.
    const result = parseNewsArgs(['NVDA', '50']);
    expect(result.symbols).toEqual(['NVDA']);
    expect(result.limit).toBe(50);
  });

  it('reads a duration as the look-back window', () => {
    expect(parseNewsArgs(['NVDA', '30d']).days).toBe(30);
    expect(parseNewsArgs(['NVDA', '1w']).days).toBe(7);
    // Sub-day windows round up rather than asking for zero days of news.
    expect(parseNewsArgs(['NVDA', '6h']).days).toBe(1);
  });

  it('takes symbols, count and window in any order', () => {
    expect(parseNewsArgs(['aapl', '10', 'msft', '14d'])).toEqual({
      symbols: ['AAPL', 'MSFT'],
      limit: 10,
      days: 14,
    });
  });

  it('de-duplicates symbols so the panel id is stable', () => {
    expect(parseNewsArgs(['AAPL', 'aapl']).symbols).toEqual(['AAPL']);
  });

  it('rejects a count the upstream cannot serve', () => {
    expect(() => parseNewsArgs(['NVDA', '500'])).toThrow(/between 1 and 50/);
    expect(() => parseNewsArgs(['NVDA', '0'])).toThrow(/between 1 and 50/);
  });

  it('rejects an argument it cannot classify', () => {
    expect(() => parseNewsArgs(['NVDA', '--'])).toThrow(/Unrecognised argument/);
  });
});

describe('guessAssetClass', () => {
  it('claims the symbols Kalshi lists crypto ladders for', () => {
    expect(guessAssetClass('BTC')).toBe('crypto');
    expect(guessAssetClass('eth')).toBe('crypto');
    expect(guessAssetClass('BTC-USD')).toBe('crypto');
  });

  it('treats everything else as an equity', () => {
    expect(guessAssetClass('AAPL')).toBe('stock');
    expect(guessAssetClass('^GSPC')).toBe('stock');
    expect(guessAssetClass('SPX')).toBe('stock');
  });
});
