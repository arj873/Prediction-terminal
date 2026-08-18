/**
 * Which chart a headline's symbol tag opens.
 *
 * The wire tags a crypto story with a *pair* rather than a coin, so the naive
 * reading sends the row to the equity chart and prices nothing. The cases below
 * are the two shapes that arrive and the one that must not be mistaken for the
 * other.
 */

import { describe, expect, it } from 'vitest';

import { chartCommand } from './news';

describe('chartCommand', () => {
  it('sends an equity tag to the equity chart', () => {
    expect(chartCommand('NVDA')).toBe('STK NVDA');
    expect(chartCommand('BRK.B')).toBe('STK BRK.B');
  });

  it('sends a crypto pair to the crypto chart, without the quote currency', () => {
    // `STK BTCUSD` prices nothing — the wire tags crypto stories as pairs.
    expect(chartCommand('BTCUSD')).toBe('CRY BTC');
    expect(chartCommand('BTC/USD')).toBe('CRY BTC');
    expect(chartCommand('ETH-USD')).toBe('CRY ETH');
    expect(chartCommand('SOLUSDT')).toBe('CRY SOL');
  });
});
