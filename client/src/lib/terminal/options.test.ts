/**
 * The option commands, and the panel identity they imply.
 *
 * `panelId` is the identity rule: two commands producing the same id are the
 * same panel, so re-issuing one re-cuts what is on screen instead of tiling a
 * second copy. For options that rule has to distinguish an expiry — two expiries
 * of one underlying are two different boards — while *not* distinguishing the
 * casing someone happened to type.
 */

import { describe, expect, it } from 'vitest';

import { COMMAND_INDEX } from './commands';
import { panelId } from './registry';

describe('panelId for the option panels', () => {
  it('treats two expiries of one underlying as two boards', () => {
    expect(panelId.optionChain('BTC', '2026-12-25')).not.toBe(
      panelId.optionChain('BTC', '2027-03-26'),
    );
    expect(panelId.optionVol('SPY', '30d')).not.toBe(panelId.optionVol('SPY', '60d'));
    expect(panelId.optionPositioning('AAPL', '1')).not.toBe(panelId.optionPositioning('AAPL', '2'));
  });

  it('does not distinguish the casing someone happened to type', () => {
    expect(panelId.optionChain('btc')).toBe(panelId.optionChain('BTC'));
    expect(panelId.optionQuote('btc-25dec26-104000-c')).toBe(
      panelId.optionQuote('BTC-25DEC26-104000-C'),
    );
  });

  it('keeps the four views over one board apart', () => {
    // OPT, VOL and OI are three readings of the same expiry, and each is its
    // own panel — a reader wants the chain and the smile side by side.
    const ids = new Set([
      panelId.optionChain('BTC'),
      panelId.optionVol('BTC'),
      panelId.optionPositioning('BTC'),
    ]);
    expect(ids.size).toBe(3);
  });

  it('gives the front expiry a stable id of its own', () => {
    // No expiry means the front month, and asking twice must not tile twice.
    expect(panelId.optionChain('BTC')).toBe(panelId.optionChain('BTC'));
    expect(panelId.optionChain('BTC')).not.toBe(panelId.optionChain('BTC', '2026-12-25'));
  });
});

describe('the option commands', () => {
  it('reaches all four verbs and their aliases', () => {
    for (const verb of ['OPT', 'OPD', 'VOL', 'OI', 'CHAIN', 'SMILE', 'PAIN']) {
      expect(COMMAND_INDEX.get(verb), verb).toBeDefined();
    }
    // The aliases reach the same command, not a copy of it.
    expect(COMMAND_INDEX.get('CHAIN')).toBe(COMMAND_INDEX.get('OPT'));
    expect(COMMAND_INDEX.get('SMILE')).toBe(COMMAND_INDEX.get('VOL'));
    expect(COMMAND_INDEX.get('PAIN')).toBe(COMMAND_INDEX.get('OI'));
  });

  it('needs a symbol, and says which argument is missing', () => {
    for (const verb of ['OPT', 'VOL', 'OI']) {
      const command = COMMAND_INDEX.get(verb)!;
      expect(command.usage).toContain('<symbol>');
    }
    // A contract, not a symbol: OPD is routed on the identifier's shape.
    expect(COMMAND_INDEX.get('OPD')!.usage).toContain('<contract>');
  });

  it('advertises an example of each identifier shape a reader may paste', () => {
    const examples = COMMAND_INDEX.get('OPD')!.examples ?? [];
    // A Deribit instrument name and an OCC symbol are not confusable, which is
    // what lets one command take either.
    expect(examples.some((e) => e.includes('-C') || e.includes('-P'))).toBe(true);
    expect(examples.some((e) => /[A-Z]+\d{6}[CP]\d{8}/.test(e))).toBe(true);
  });
});
