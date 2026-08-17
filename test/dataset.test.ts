/**
 * The data-source table, and reading a `[source:]id` reference.
 *
 * The reference parser is the one piece of this feature every command touches,
 * and the failure it must never have is a *silent* one: sending an ECB key to
 * FRED because the prefix was not recognised would 404 with a message about
 * FRED, which sends a reader looking in the wrong place entirely.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  DATA_SOURCES,
  DATA_SOURCE_IDS,
  DEFAULT_DATA_SOURCE,
  SERIES_SOURCE_IDS,
  dataSourceInfo,
  formatDataRef,
  isDataSource,
  normaliseDataId,
  parseDataRef,
  parseDataSource,
} from '../src/shared/dataset.js';

describe('the data source table', () => {
  it('covers all twelve publishers', () => {
    assert.equal(DATA_SOURCES.length, 12);
    for (const id of [
      'bls', 'congress', 'cftc', 'ecb', 'imf', 'fed',
      'fred', 'datagov', 'oecd', 'polygon', 'sec', 'eia',
    ]) {
      assert.ok(isDataSource(id), `${id} is missing from the table`);
    }
  });

  it('names exactly one default, and it prints bare', () => {
    const defaults = DATA_SOURCES.filter((s) => s.isDefault);
    assert.equal(defaults.length, 1);
    assert.equal(DEFAULT_DATA_SOURCE, 'fred');
    assert.ok(dataSourceInfo('fred').bareRef);
  });

  it('gives every source a unique prefix and code', () => {
    assert.equal(new Set(DATA_SOURCES.map((s) => s.prefix)).size, DATA_SOURCES.length);
    assert.equal(new Set(DATA_SOURCES.map((s) => s.code)).size, DATA_SOURCES.length);
  });

  it('never lets two sources claim the same alias', () => {
    const seen = new Map<string, string>();
    for (const source of DATA_SOURCES) {
      for (const alias of source.aliases) {
        const owner = seen.get(alias);
        assert.equal(owner, undefined, `"${alias}" is claimed by both ${owner} and ${source.id}`);
        seen.set(alias, source.id);
      }
    }
  });

  it('derives the series-only list from the table', () => {
    assert.deepEqual(SERIES_SOURCE_IDS, ['fred', 'bls', 'ecb', 'imf', 'oecd', 'fed', 'eia', 'cftc']);
    assert.ok(!SERIES_SOURCE_IDS.includes('sec'));
    assert.ok(SERIES_SOURCE_IDS.every((id) => DATA_SOURCE_IDS.includes(id)));
  });

  it('gives every id example the shape its own parser accepts', () => {
    for (const source of DATA_SOURCES) {
      const ref = parseDataRef(`${source.id}:${source.idExample}`);
      assert.equal(ref.source, source.id);
      assert.equal(ref.id, normaliseDataId(source.id, source.idExample));
    }
  });
});

describe('parseDataSource', () => {
  it('reads canonical names and aliases', () => {
    assert.equal(parseDataSource('bls'), 'bls');
    assert.equal(parseDataSource('BLS'), 'bls');
    assert.equal(parseDataSource('  ecb  '), 'ecb');
    assert.equal(parseDataSource('federalreserve'), 'fed');
    assert.equal(parseDataSource('edgar'), 'sec');
    assert.equal(parseDataSource('cot'), 'cftc');
  });

  it('returns null for a token that names nothing', () => {
    assert.equal(parseDataSource('nasdaq'), null);
    assert.equal(parseDataSource(''), null);
  });

  it('declines ordinary English aliases in word context but accepts them as a prefix', () => {
    // `ECOS government spending` must search for two words, not filter to one
    // publisher and search for one — the same rule the venue table uses.
    for (const word of ['gov', 'energy', 'labor', 'fund', 'board', 'usa', 'bills']) {
      assert.equal(parseDataSource(word, 'word'), null, `"${word}" was claimed out of free text`);
      assert.notEqual(parseDataSource(word, 'prefix'), null, `"${word}:" should still work`);
    }
  });

  it('still claims unambiguous aliases in word context', () => {
    assert.equal(parseDataSource('ecb', 'word'), 'ecb');
    assert.equal(parseDataSource('oecd', 'word'), 'oecd');
    assert.equal(parseDataSource('eia', 'word'), 'eia');
  });
});

describe('parseDataRef', () => {
  it('sends an unprefixed reference to FRED, unchanged in meaning', () => {
    assert.deepEqual(parseDataRef('UNRATE'), { source: 'fred', id: 'UNRATE' });
    assert.deepEqual(parseDataRef('unrate'), { source: 'fred', id: 'UNRATE' });
  });

  it('reads a source prefix', () => {
    assert.deepEqual(parseDataRef('bls:LNS14000000'), { source: 'bls', id: 'LNS14000000' });
    assert.deepEqual(parseDataRef('labor:lns14000000'), { source: 'bls', id: 'LNS14000000' });
  });

  it('preserves case where case is part of the identifier', () => {
    // An SDMX key is case-sensitive: folding it either way 404s the request.
    const ref = parseDataRef('ecb:EXR/D.USD.EUR.SP00.A');
    assert.deepEqual(ref, { source: 'ecb', id: 'EXR/D.USD.EUR.SP00.A' });
    assert.deepEqual(parseDataRef('oecd:DSD_KEI@DF_KEI/USA.M.CP.GR._Z._Z.GY').id,
      'DSD_KEI@DF_KEI/USA.M.CP.GR._Z._Z.GY');
  });

  it('keeps slashes inside an id rather than treating them as structure', () => {
    assert.equal(parseDataRef('fed:H15/RIFLGFCY10_N.B').id, 'H15/RIFLGFCY10_N.B');
    assert.equal(parseDataRef('cftc:legacy/GOLD/noncomm_net').id, 'legacy/GOLD/noncomm_net');
  });

  it('reports an unknown prefix rather than sending it to the default source', () => {
    assert.throws(() => parseDataRef('bloomberg:SPX'), /Unknown source prefix "bloomberg"/);
  });

  it('rejects a prefix with no identifier after it', () => {
    assert.throws(() => parseDataRef('bls:'), /names a source but no series id/);
    assert.throws(() => parseDataRef('   '), /Missing series id/);
  });

  it('honours an explicit fallback source', () => {
    assert.deepEqual(parseDataRef('LNS14000000', 'bls'), { source: 'bls', id: 'LNS14000000' });
  });
});

describe('formatDataRef', () => {
  it('prints the default source bare and everything else prefixed', () => {
    assert.equal(formatDataRef({ source: 'fred', id: 'UNRATE' }), 'UNRATE');
    assert.equal(formatDataRef({ source: 'bls', id: 'LNS14000000' }), 'bls:LNS14000000');
  });

  it('round-trips every source', () => {
    for (const source of DATA_SOURCES) {
      const ref = { source: source.id, id: normaliseDataId(source.id, source.idExample) };
      assert.deepEqual(parseDataRef(formatDataRef(ref)), ref);
    }
  });
});
