/**
 * The venue grammar against its parity contract.
 *
 * `venue.ts` is one of two implementations of the same rules — the other is
 * `crates/core/src/venue.rs` — so the cases live in data rather than in either
 * language's test file. `contract/venue-cases.json` describes its own schema;
 * this suite is the TypeScript half reading it. The Rust half reads the same
 * file, so a rule that drifts on one side fails on the other instead of
 * surfacing as a 404 the day someone types `pm:` at the prompt.
 */

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import type { Venue } from '$gen';
import { formatRef, normaliseId, parseRef, parseVenue } from './venue';

interface ParseRefCase {
  input: string;
  fallback: Venue;
  venue: Venue;
  id: string;
}

interface ParseRefErrorCase {
  input: string;
  fallback: Venue;
  error: 'unknown-prefix' | 'empty-identifier';
  messageContains: string[];
}

interface FormatRefCase {
  venue: Venue;
  id: string;
  text: string;
}

interface NormaliseIdCase {
  venue: Venue;
  input: string;
  id: string;
}

interface ParseVenueCase {
  token: string;
  context: 'prefix' | 'word';
  venue: Venue | null;
}

interface Contract {
  parseRef: ParseRefCase[];
  parseRefErrors: ParseRefErrorCase[];
  formatRef: FormatRefCase[];
  normaliseId: NormaliseIdCase[];
  parseVenue: ParseVenueCase[];
}

const CONTRACT_PATH = new URL('../../../../contract/venue-cases.json', import.meta.url);
const cases = JSON.parse(readFileSync(CONTRACT_PATH, 'utf8')) as Contract;

/**
 * Which failure a message reports.
 *
 * The TypeScript half throws a plain `Error`, so the kind is read back off the
 * wording. Rust models the same two failures as `RefError` variants; the
 * contract names the kind so neither side has to know the other's mechanism.
 */
function errorKind(message: string): string {
  if (message.startsWith('Unknown venue prefix')) return 'unknown-prefix';
  if (message.includes('names a venue but no')) return 'empty-identifier';
  return `unrecognised (${message})`;
}

describe('venue contract', () => {
  it('covers every case list', () => {
    // A truncated or renamed file must fail here rather than silently passing
    // zero assertions below.
    expect(cases.parseRef.length).toBeGreaterThan(0);
    expect(cases.parseRefErrors.length).toBeGreaterThan(0);
    expect(cases.formatRef.length).toBeGreaterThan(0);
    expect(cases.normaliseId.length).toBeGreaterThan(0);
    expect(cases.parseVenue.length).toBeGreaterThan(0);
  });
});

describe('parseRef', () => {
  for (const c of cases.parseRef) {
    it(`reads ${JSON.stringify(c.input)} against ${c.fallback} as ${c.venue}:${c.id}`, () => {
      expect(parseRef(c.input, c.fallback)).toEqual({ venue: c.venue, id: c.id });
    });
  }
});

describe('parseRef errors', () => {
  for (const c of cases.parseRefErrors) {
    it(`rejects ${JSON.stringify(c.input)} as ${c.error}`, () => {
      let message: string | null = null;
      try {
        parseRef(c.input, c.fallback);
      } catch (err) {
        message = (err as Error).message;
      }

      expect(message, `${JSON.stringify(c.input)} should not parse`).not.toBe(null);
      expect(errorKind(message as string)).toBe(c.error);
      for (const fragment of c.messageContains) {
        expect(message as string).toContain(fragment);
      }
    });
  }
});

describe('formatRef', () => {
  for (const c of cases.formatRef) {
    it(`prints ${c.venue}:${c.id} as ${c.text}`, () => {
      expect(formatRef({ venue: c.venue, id: c.id })).toBe(c.text);
    });
  }

  it('round-trips through parseRef', () => {
    // Every printed reference has to read back as the same reference, or a
    // panel id built from one would not match the market it was cut from.
    for (const c of cases.formatRef) {
      expect(parseRef(c.text)).toEqual({ venue: c.venue, id: c.id });
    }
  });
});

describe('normaliseId', () => {
  for (const c of cases.normaliseId) {
    it(`folds ${JSON.stringify(c.input)} to ${c.id} at ${c.venue}`, () => {
      expect(normaliseId(c.venue, c.input)).toBe(c.id);
    });
  }
});

describe('parseVenue', () => {
  for (const c of cases.parseVenue) {
    const expected = c.venue ?? 'nothing';
    it(`reads ${JSON.stringify(c.token)} in ${c.context} context as ${expected}`, () => {
      expect(parseVenue(c.token, c.context)).toBe(c.venue);
    });
  }
});
