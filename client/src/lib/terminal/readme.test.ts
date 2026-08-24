/**
 * The README, checked against the command table it describes.
 *
 * Prose rots silently. This one line — "type `HELP` and read N verbs" — has now
 * been wrong twice: it said 35 when there were 39, and 39 when there were 40. It
 * is a small thing to be wrong about and a fair signal of how much else in a
 * README can be trusted, so it is checked rather than maintained by hand.
 *
 * The same idea as `contract/venue-cases.json`, applied to documentation: a
 * claim that can be derived from the code should be, and where it cannot be
 * derived it should at least be pinned.
 */

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

import { COMMANDS } from './commands';

const README = readFileSync(new URL('../../../../README.md', import.meta.url), 'utf8');

describe('the README', () => {
  it('states the number of verbs the command table actually has', () => {
    const claim = /read (\d+) verbs/.exec(README);
    expect(claim, 'the README should still make the claim this test checks').not.toBeNull();
    expect(Number(claim![1])).toBe(COMMANDS.length);
  });

  it('documents every command group it lists commands for', () => {
    // Each verb should appear somewhere in the README — a command nobody can
    // find is barely shipped.
    const missing = COMMANDS.map((c) => c.verb).filter(
      (verb) => !new RegExp(`\`${verb}\``).test(README),
    );
    expect(missing).toEqual([]);
  });
});
