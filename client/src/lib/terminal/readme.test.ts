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
import { DATA_SOURCES } from './dataset';
import { VENUES } from './venue';

const README = readFileSync(new URL('../../../../README.md', import.meta.url), 'utf8');

/** The counts the README spells out in words rather than digits. */
const WORDS: Record<string, number> = {
  three: 3,
  four: 4,
  five: 5,
  six: 6,
  seven: 7,
  eight: 8,
  nine: 9,
  ten: 10,
  eleven: 11,
  twelve: 12,
  thirteen: 13,
};

describe('the README', () => {
  it('states the number of verbs the command table actually has', () => {
    const claim = /read (\d+) verbs/.exec(README);
    expect(claim, 'the README should still make the claim this test checks').not.toBeNull();
    expect(Number(claim![1])).toBe(COMMANDS.length);
  });

  it('states the number of publishers the registry actually holds', () => {
    const claim = /(\w+) data publishers/.exec(README);
    expect(claim, 'the README should still make the claim this test checks').not.toBeNull();
    expect(WORDS[claim![1]!.toLowerCase()]).toBe(DATA_SOURCES.length);
  });

  it('lists every venue prefix a reader could type', () => {
    // The prefix table is what someone reads before typing `gem:` for the first
    // time, so a venue missing from it is a venue nobody finds.
    const missing = VENUES.filter((venue) => !README.includes(`\`${venue.prefix}\``));
    expect(missing.map((v) => v.prefix)).toEqual([]);
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
