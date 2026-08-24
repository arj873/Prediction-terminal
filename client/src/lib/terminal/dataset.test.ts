/**
 * The data-source grammar against its parity contract.
 *
 * `dataset.ts` is one of two implementations of the same rules — the other is
 * `crates/core/src/dataset.rs` — so the cases live in data rather than in either
 * language's test file. `contract/source-cases.json` describes its own schema;
 * this suite is the TypeScript half reading it, and
 * `crates/core/tests/source_contract.rs` is the Rust half reading the same file.
 * A rule that drifts on one side fails on the other instead of surfacing as a
 * 404 the day someone types `ecb:` at the prompt.
 */

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import type { DataSource } from '$gen';
import { formatDataRef, normaliseDataId, parseDataRef, parseDataSource } from './dataset';

interface ParseRefCase {
  input: string;
  fallback: DataSource;
  source: DataSource;
  id: string;
}

interface ParseRefErrorCase {
  input: string;
  fallback: DataSource;
  error: 'unknown-prefix' | 'empty-identifier' | 'missing-reference';
  messageContains: string[];
}

interface FormatRefCase {
  source: DataSource;
  id: string;
  text: string;
}

interface NormaliseIdCase {
  source: DataSource;
  input: string;
  id: string;
}

interface ParseSourceCase {
  token: string;
  context: 'prefix' | 'word';
  source: DataSource | null;
}

interface Contract {
  parseRef: ParseRefCase[];
  parseRefErrors: ParseRefErrorCase[];
  formatRef: FormatRefCase[];
  normaliseId: NormaliseIdCase[];
  parseSource: ParseSourceCase[];
}

const CONTRACT_PATH = new URL('../../../../contract/source-cases.json', import.meta.url);
const cases = JSON.parse(readFileSync(CONTRACT_PATH, 'utf8')) as Contract;

/**
 * Which failure a message reports.
 *
 * The TypeScript half throws a plain `Error`, so the kind is read back off the
 * wording. Rust models the same three failures as `DataRefError` variants; the
 * contract names the kind so neither side has to know the other's mechanism.
 */
function errorKind(message: string): string {
  if (message.startsWith('Unknown source prefix')) return 'unknown-prefix';
  if (message.includes('names a source but no')) return 'empty-identifier';
  if (message === 'Missing series id') return 'missing-reference';
  return `unrecognised (${message})`;
}

describe('source contract', () => {
  it('covers every case list', () => {
    // A truncated or renamed file must fail here rather than silently passing
    // zero assertions below.
    expect(cases.parseRef.length).toBeGreaterThan(0);
    expect(cases.parseRefErrors.length).toBeGreaterThan(0);
    expect(cases.formatRef.length).toBeGreaterThan(0);
    expect(cases.normaliseId.length).toBeGreaterThan(0);
    expect(cases.parseSource.length).toBeGreaterThan(0);
  });
});

describe('parseDataRef', () => {
  for (const c of cases.parseRef) {
    it(`reads ${JSON.stringify(c.input)} against ${c.fallback} as ${c.source}:${c.id}`, () => {
      expect(parseDataRef(c.input, c.fallback)).toEqual({ source: c.source, id: c.id });
    });
  }
});

describe('parseDataRef errors', () => {
  for (const c of cases.parseRefErrors) {
    it(`rejects ${JSON.stringify(c.input)} as ${c.error}`, () => {
      let message: string | null = null;
      try {
        parseDataRef(c.input, c.fallback);
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

describe('formatDataRef', () => {
  for (const c of cases.formatRef) {
    it(`prints ${c.source}:${c.id} as ${c.text}`, () => {
      expect(formatDataRef({ source: c.source, id: c.id })).toBe(c.text);
    });
  }

  it('round-trips through parseDataRef', () => {
    // Every printed reference has to read back as the same reference, or a
    // panel id built from one would not match the series it was cut from.
    for (const c of cases.formatRef) {
      expect(parseDataRef(c.text)).toEqual({ source: c.source, id: c.id });
    }
  });
});

describe('normaliseDataId', () => {
  for (const c of cases.normaliseId) {
    it(`folds ${JSON.stringify(c.input)} to ${c.id} at ${c.source}`, () => {
      expect(normaliseDataId(c.source, c.input)).toBe(c.id);
    });
  }
});

describe('parseDataSource', () => {
  for (const c of cases.parseSource) {
    const expected = c.source ?? 'nothing';
    it(`reads ${JSON.stringify(c.token)} in ${c.context} context as ${expected}`, () => {
      expect(parseDataSource(c.token, c.context)).toBe(c.source);
    });
  }
});
