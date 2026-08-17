/**
 * The venue registry: one table row per broker, and its capabilities declared
 * rather than discovered.
 *
 * Three brokers name the same question three ways, so every reference the
 * terminal handles is `[venue:]identifier` and this table is the only thing
 * that knows how to read one. The cases below encode the quirks that made it a
 * table in the first place:
 *
 *   - Kalshi 404s a lower-case ticker and Polymarket 404s an upper-case slug,
 *     so `KXFEDDECISION-26SEP-T3.75` and `pm:will-the-fed-…` must come back
 *     cased the way their own API answers to — folded here, once, never at the
 *     call site.
 *   - An unprefixed reference has to keep meaning Kalshi, and a Kalshi
 *     reference has to keep printing bare, because every command, example and
 *     README line in the terminal predates the other two venues.
 *   - `us`, `k`, `intl` and `international` are venue aliases *and* ordinary
 *     English. As a prefix they are unambiguous; claimed out of free text they
 *     turned `SRCH us election` into a filtered search of the wrong exchange.
 *   - A missing capability is a fact about the venue's public API, not an
 *     outage: Polymarket US publishes no price history, tape or ranking figure
 *     to an unauthenticated caller, and Polymarket publishes no open interest.
 *     `TOP` and `DES` read those flags to decide what to offer, so a row added
 *     without them filled in would render dead buttons and blame the venue for
 *     an empty board. The table-wide assertions here fail if that happens.
 *
 * The registry is asserted against the table's *actual* contents and
 * cross-checked against `sources/venues.ts`, so a fourth broker added to one
 * and not the other fails here rather than in a panel.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  DEFAULT_VENUE,
  VENUES,
  VENUE_IDS,
  formatRef,
  isVenue,
  normaliseId,
  parseRef,
  parseVenue,
  supports,
  supportsSort,
  venueInfo,
  type MoverSort,
  type Venue,
} from '../src/shared/venue.js';
import { sourceFor } from '../src/server/sources/venues.js';

/** Every ranking `TOP` knows how to ask for. A venue declares its subset. */
const ALL_SORTS: readonly MoverSort[] = [
  'volume',
  'gainers',
  'losers',
  'open_interest',
  'liquidity',
];

/** A real identifier at each venue, in the case that venue's API answers to. */
const REAL_ID: Record<Venue, string> = {
  kalshi: 'KXFEDDECISION-26SEP-T3.75',
  polymarket: 'will-the-fed-decrease-interest-rates-in-september-2026',
  'polymarket-us': 'tec-mlb-nlchamp-2026-09-27-lad',
};

/** The whole verb set `sources/venues.ts` promises for every broker. */
const VENUE_SOURCE_METHODS = [
  'getMarket',
  'getOrderBook',
  'getTrades',
  'getCandles',
  'getEvent',
  'listSeries',
  'search',
  'topMarkets',
  'corpusSnapshot',
  'warmCorpus',
] as const;

function servesEverything(venue: Venue): boolean {
  const caps = venueInfo(venue).capabilities;
  return (
    caps.candles &&
    caps.trades &&
    caps.seriesCategoryFilter &&
    caps.sorts.length === ALL_SORTS.length
  );
}

/* ------------------------------------------------------------------ table */

describe('the venue table', () => {
  it('lists the three brokers the terminal quotes, once each', () => {
    assert.deepEqual(VENUE_IDS, ['kalshi', 'polymarket', 'polymarket-us']);
    assert.deepEqual(
      VENUES.map((v) => v.id),
      VENUE_IDS,
    );
    assert.equal(new Set(VENUE_IDS).size, VENUE_IDS.length);
  });

  it('gives every venue the fields a header, a badge and a usage line read', () => {
    for (const v of VENUES) {
      assert.ok(v.label.length > 0, `${v.id} has no label`);
      assert.match(v.code, /^[A-Z]{2,5}$/, `${v.id} has no badge code`);
      assert.ok(v.idLabel.length > 0, `${v.id} does not say what it calls an identifier`);
      assert.ok(v.case === 'upper' || v.case === 'lower', `${v.id} declares no identifier case`);
      assert.match(v.site, /^https:\/\/[^/]+$/, `${v.id} has no public site`);
    }
  });

  it('keeps the badge codes and prefixes distinct, so a row is identifiable', () => {
    assert.equal(new Set(VENUES.map((v) => v.code)).size, VENUES.length);
    assert.equal(new Set(VENUES.map((v) => v.prefix)).size, VENUES.length);
  });

  it('declares a prefix that reads back as one of its own aliases', () => {
    // `formatRef` writes the prefix and `parseRef` reads it with `parseVenue`,
    // so a prefix that is not an alias would print a reference nothing parses.
    for (const v of VENUES) {
      assert.ok(v.prefix.endsWith(':'), `${v.id} prefix "${v.prefix}" has no colon`);
      const bare = v.prefix.slice(0, -1);
      assert.ok(v.aliases.includes(bare), `${v.id} prefix "${v.prefix}" is not an alias`);
      assert.equal(parseVenue(bare), v.id);
    }
  });

  it('accepts its own id as an alias, so the long form parses too', () => {
    for (const v of VENUES) {
      assert.ok(v.aliases.includes(v.id), `${v.id} is not an alias of itself`);
      assert.equal(parseVenue(v.id), v.id);
    }
  });

  it('gives every alias to exactly one venue', () => {
    // The alias map is built by flatMap, so a collision would silently hand the
    // token to whichever venue is later in the table.
    const seen = new Map<string, Venue>();
    for (const v of VENUES) {
      for (const alias of v.aliases) {
        assert.equal(seen.get(alias), undefined, `alias "${alias}" is claimed twice`);
        seen.set(alias, v.id);
      }
    }
  });

  it('keeps aliases in the folded form the lookup uses', () => {
    // `parseVenue` folds the token, never the table, so an alias with an upper
    // case letter or a stray space could never be matched.
    for (const v of VENUES) {
      for (const alias of v.aliases) {
        assert.equal(alias, alias.trim().toLowerCase(), `alias "${alias}" is not folded`);
      }
    }
  });

  it('names exactly one default venue, and prints only that one bare', () => {
    const defaults = VENUES.filter((v) => v.isDefault);
    assert.equal(defaults.length, 1);
    assert.equal(defaults[0]!.id, 'kalshi');
    assert.equal(DEFAULT_VENUE, 'kalshi');
    for (const v of VENUES) {
      if (v.bareRef) assert.equal(v.id, DEFAULT_VENUE, `${v.id} prints bare but is not the default`);
    }
  });

  it('fills in every capability flag for every venue', () => {
    // The point of the table: a broker added without these is a dead button in
    // DES and a venue silently missing from every TOP board.
    for (const v of VENUES) {
      const caps = v.capabilities;
      assert.equal(typeof caps.candles, 'boolean', `${v.id} does not declare candles`);
      assert.equal(typeof caps.trades, 'boolean', `${v.id} does not declare trades`);
      assert.equal(
        typeof caps.seriesCategoryFilter,
        'boolean',
        `${v.id} does not declare seriesCategoryFilter`,
      );
      assert.ok(Array.isArray(caps.sorts), `${v.id} does not declare its sorts`);
      assert.equal(new Set(caps.sorts).size, caps.sorts.length, `${v.id} repeats a sort`);
      for (const sort of caps.sorts) {
        assert.ok(ALL_SORTS.includes(sort), `${v.id} declares unknown sort "${sort}"`);
      }
    }
  });

  it('explains a capability it lacks, and explains nothing when it serves them all', () => {
    // The note is what a disabled GP/TAS button says instead of nothing, so a
    // venue missing a capability without one leaves the reader with a dead
    // control and no reason.
    for (const v of VENUES) {
      if (servesEverything(v.id)) {
        assert.equal(v.capabilities.note, undefined, `${v.id} serves everything but has a note`);
      } else {
        assert.ok(
          (v.capabilities.note ?? '').length > 20,
          `${v.id} is missing a capability but does not say why`,
        );
      }
    }
  });

  it('records what each broker actually publishes today', () => {
    assert.deepEqual(venueInfo('kalshi').capabilities, {
      candles: true,
      trades: true,
      seriesCategoryFilter: true,
      sorts: ALL_SORTS,
    });

    const pm = venueInfo('polymarket').capabilities;
    assert.equal(pm.candles, true);
    assert.equal(pm.trades, true);
    assert.equal(pm.seriesCategoryFilter, false);
    // Gamma reports open interest per event, never per market, so an OI board
    // would rank Polymarket on a figure it never published.
    assert.deepEqual(pm.sorts, ['volume', 'gainers', 'losers', 'liquidity']);

    const pmus = venueInfo('polymarket-us').capabilities;
    assert.equal(pmus.candles, false);
    assert.equal(pmus.trades, false);
    assert.equal(pmus.seriesCategoryFilter, false);
    // Its public catalogue carries no volume, change, liquidity or OI at all.
    assert.deepEqual(pmus.sorts, []);
  });
});

/* ------------------------------------------------------- lookup by venue id */

describe('venueInfo', () => {
  it('returns the table row itself for every venue', () => {
    for (const v of VENUES) {
      assert.equal(venueInfo(v.id), v);
    }
  });

  it('rejects an unknown id rather than handing back undefined', () => {
    // The return value is dereferenced straight away by every caller
    // (`venueInfo(v).code`), so an undefined here surfaces as a TypeError in a
    // panel rather than as the name of the venue nobody has.
    assert.throws(() => venueInfo('nasdaq' as Venue), /Unknown venue "nasdaq"/);
    assert.throws(() => venueInfo('' as Venue), /Unknown venue/);
  });

  it('rejects an alias — an alias is not an id', () => {
    // `pm` reaches a venue through parseVenue; the map keyed by id must not
    // also answer to it, or two spellings of the same venue would flow through
    // the routes and the cache keys.
    assert.throws(() => venueInfo('pm' as Venue), /Unknown venue/);
    assert.throws(() => venueInfo('Kalshi' as Venue), /Unknown venue/);
  });
});

describe('isVenue', () => {
  it('accepts every id in the table', () => {
    for (const id of VENUE_IDS) assert.equal(isVenue(id), true);
  });

  it('declines aliases, wrong case and empty strings', () => {
    assert.equal(isVenue('pm'), false);
    assert.equal(isVenue('Kalshi'), false);
    assert.equal(isVenue('POLYMARKET'), false);
    assert.equal(isVenue('nasdaq'), false);
    assert.equal(isVenue(''), false);
  });
});

/* -------------------------------------------------------------- alias reading */

describe('parseVenue', () => {
  it('reads every alias of every venue back to that venue', () => {
    for (const v of VENUES) {
      for (const alias of v.aliases) {
        assert.equal(parseVenue(alias), v.id, `alias "${alias}"`);
      }
    }
  });

  it('folds case and trims the whitespace a typed token carries', () => {
    for (const v of VENUES) {
      for (const alias of v.aliases) {
        assert.equal(parseVenue(alias.toUpperCase()), v.id, `alias "${alias}" upper-cased`);
        assert.equal(parseVenue(`  ${alias}\t`), v.id, `alias "${alias}" padded`);
      }
    }
  });

  it('returns null for a token that names no venue', () => {
    assert.equal(parseVenue('nasdaq'), null);
    assert.equal(parseVenue(''), null);
    assert.equal(parseVenue('   '), null);
    // The colon belongs to the reference syntax, not to the alias.
    assert.equal(parseVenue('pm:'), null);
  });

  it('declines the aliases that are ordinary English when read as a search word', () => {
    // `SRCH us election` is a search for two words. Claiming `us` there
    // silently searched Polymarket US, and `TV The Office US` charted the
    // wrong book.
    for (const word of ['us', 'k', 'intl', 'international']) {
      assert.equal(parseVenue(word, 'word'), null, `"${word}" as a word`);
      assert.ok(parseVenue(word, 'prefix'), `"${word}" as a prefix`);
    }
    assert.equal(parseVenue('US', 'word'), null);
    assert.equal(parseVenue('International', 'word'), null);
  });

  it('still claims the unambiguous aliases out of free text', () => {
    assert.equal(parseVenue('kalshi', 'word'), 'kalshi');
    assert.equal(parseVenue('kx', 'word'), 'kalshi');
    assert.equal(parseVenue('pm', 'word'), 'polymarket');
    assert.equal(parseVenue('poly', 'word'), 'polymarket');
    assert.equal(parseVenue('pmus', 'word'), 'polymarket-us');
    assert.equal(parseVenue('polymarket-us', 'word'), 'polymarket-us');
  });

  it('defaults to the prefix reading, which accepts everything', () => {
    assert.equal(parseVenue('us'), 'polymarket-us');
    assert.equal(parseVenue('k'), 'kalshi');
  });
});

/* ------------------------------------------------------ identifier casing */

describe('normaliseId', () => {
  it('shouts a Kalshi ticker and whispers a Polymarket slug', () => {
    assert.equal(
      normaliseId('kalshi', 'kxfeddecision-26sep-t3.75'),
      'KXFEDDECISION-26SEP-T3.75',
    );
    assert.equal(
      normaliseId('polymarket', 'Will-The-Fed-Decrease-Interest-Rates-In-September-2026'),
      'will-the-fed-decrease-interest-rates-in-september-2026',
    );
    assert.equal(normaliseId('polymarket-us', 'TEC-MLB-NLCHAMP-2026-09-27-LAD'), 'tec-mlb-nlchamp-2026-09-27-lad');
  });

  it('leaves an already correct identifier alone, and is idempotent', () => {
    for (const id of VENUE_IDS) {
      const once = normaliseId(id, REAL_ID[id]);
      assert.equal(once, REAL_ID[id]);
      assert.equal(normaliseId(id, once), once);
    }
  });

  it('follows the case each venue declares, not a hard-coded venue name', () => {
    for (const v of VENUES) {
      const folded = normaliseId(v.id, 'MiXeD-Case-42');
      assert.equal(folded, v.case === 'upper' ? 'MIXED-CASE-42' : 'mixed-case-42');
    }
  });

  it('rejects an unknown venue instead of guessing a case', () => {
    assert.throws(() => normaliseId('nasdaq' as Venue, 'AAPL'), /Unknown venue/);
  });
});

/* ----------------------------------------------------------- reading a ref */

describe('parseRef', () => {
  it('reads an unprefixed reference as the default venue, upper-cased', () => {
    assert.deepEqual(parseRef('kxfeddecision-26sep-t3.75'), {
      venue: 'kalshi',
      id: 'KXFEDDECISION-26SEP-T3.75',
    });
  });

  it('routes each canonical prefix and cases the identifier for it', () => {
    assert.deepEqual(parseRef('kx:kxfeddecision-26sep-t3.75'), {
      venue: 'kalshi',
      id: 'KXFEDDECISION-26SEP-T3.75',
    });
    assert.deepEqual(parseRef('pm:Will-The-Fed-Decrease-Interest-Rates-In-September-2026'), {
      venue: 'polymarket',
      id: 'will-the-fed-decrease-interest-rates-in-september-2026',
    });
    assert.deepEqual(parseRef('pmus:TEC-MLB-NLCHAMP-2026-09-27-LAD'), {
      venue: 'polymarket-us',
      id: 'tec-mlb-nlchamp-2026-09-27-lad',
    });
  });

  it('accepts any alias as a prefix, including the ordinary-English ones', () => {
    assert.equal(parseRef('KALSHI:kxfed-26sep').venue, 'kalshi');
    assert.equal(parseRef('poly:fed-decision').venue, 'polymarket');
    assert.equal(parseRef('us:usfed-fomc-2026-10-28').venue, 'polymarket-us');
    assert.equal(parseRef('intl:fed-decision').venue, 'polymarket');
  });

  it('tolerates the whitespace a typed command carries around the colon', () => {
    assert.deepEqual(parseRef('  PM : fed-decision-in-september  '), {
      venue: 'polymarket',
      id: 'fed-decision-in-september',
    });
    assert.deepEqual(parseRef('  kxfed-26sep  '), { venue: 'kalshi', id: 'KXFED-26SEP' });
  });

  it('splits on the first colon only, leaving the rest to the identifier', () => {
    assert.deepEqual(parseRef('pm:a:b'), { venue: 'polymarket', id: 'a:b' });
  });

  it('honours an explicit fallback venue for an unprefixed reference', () => {
    // The cross-venue panels read a list of ids already known to be one
    // venue's, and must not have them upper-cased into Kalshi tickers.
    assert.deepEqual(parseRef('Fed-Decision-2026', 'polymarket'), {
      venue: 'polymarket',
      id: 'fed-decision-2026',
    });
    assert.deepEqual(parseRef('usfed-fomc-2026-10-28', 'polymarket-us'), {
      venue: 'polymarket-us',
      id: 'usfed-fomc-2026-10-28',
    });
  });

  it('reports an unrecognised prefix instead of sending it to a venue', () => {
    // Neither a Kalshi ticker nor a Polymarket slug contains a colon, so
    // `foo:bar` is a typo — mangling it into a ticker asks the wrong exchange.
    assert.throws(() => parseRef('foo:bar'), /Unknown venue prefix "foo"/);
    assert.throws(() => parseRef('https://kalshi.com/markets/x'), /Unknown venue prefix "https"/);
  });

  it('lists the prefixes it does know when it rejects one', () => {
    assert.throws(() => parseRef('foo:bar'), (err: Error) => {
      for (const v of VENUES) assert.ok(err.message.includes(v.prefix), `missing ${v.prefix}`);
      return true;
    });
  });

  it('rejects a prefix with nothing after it, in that venue\'s own words', () => {
    assert.throws(() => parseRef('kx:'), /names a venue but no ticker/);
    assert.throws(() => parseRef('pm:   '), /names a venue but no slug/);
    assert.throws(() => parseRef('pmus:'), /names a venue but no slug/);
  });
});

/* ---------------------------------------------------------- printing a ref */

describe('formatRef', () => {
  it('prints the default venue bare and every other venue prefixed', () => {
    assert.equal(formatRef({ venue: 'kalshi', id: REAL_ID.kalshi }), REAL_ID.kalshi);
    assert.equal(
      formatRef({ venue: 'polymarket', id: REAL_ID.polymarket }),
      `pm:${REAL_ID.polymarket}`,
    );
    assert.equal(
      formatRef({ venue: 'polymarket-us', id: REAL_ID['polymarket-us'] }),
      `pmus:${REAL_ID['polymarket-us']}`,
    );
  });

  it('round-trips through parseRef for every venue in the table', () => {
    // Panels print a reference into a command string (`XV pm:…`) which the
    // command bus reads straight back, so this loop is the actual round trip.
    for (const id of VENUE_IDS) {
      const ref = { venue: id, id: REAL_ID[id] };
      assert.deepEqual(parseRef(formatRef(ref)), ref, `${id} does not round-trip`);
    }
  });

  it('round-trips an identifier that begins with another venue\'s alias', () => {
    // `us-election-2026` is a real Polymarket slug; printed bare it would read
    // as a Polymarket US reference, which is why only the default prints bare.
    const ref = { venue: 'polymarket' as const, id: 'us-election-2026' };
    assert.equal(formatRef(ref), 'pm:us-election-2026');
    assert.deepEqual(parseRef(formatRef(ref)), ref);
  });

  it('rejects an unknown venue rather than printing a prefix-less reference', () => {
    assert.throws(() => formatRef({ venue: 'nasdaq' as Venue, id: 'AAPL' }), /Unknown venue/);
  });
});

/* ------------------------------------------------------------ capabilities */

describe('supports', () => {
  it('answers for the venues that publish history and a tape', () => {
    assert.equal(supports('kalshi', 'candles'), true);
    assert.equal(supports('kalshi', 'trades'), true);
    assert.equal(supports('polymarket', 'candles'), true);
    assert.equal(supports('polymarket', 'trades'), true);
  });

  it('answers for the venue that publishes neither', () => {
    // DES renders GP and TAS disabled off these two, so a false here is the
    // difference between a tooltip and a click that can only reach a 501.
    assert.equal(supports('polymarket-us', 'candles'), false);
    assert.equal(supports('polymarket-us', 'trades'), false);
  });

  it('agrees with the table for every venue', () => {
    for (const v of VENUES) {
      assert.equal(supports(v.id, 'candles'), v.capabilities.candles);
      assert.equal(supports(v.id, 'trades'), v.capabilities.trades);
    }
  });

  it('rejects an unknown venue instead of reporting it as incapable', () => {
    // Returning false would render every button dead and say nothing.
    assert.throws(() => supports('nasdaq' as Venue, 'candles'), /Unknown venue/);
  });
});

describe('supportsSort', () => {
  it('lets TOP rank the venues that publish the figure', () => {
    assert.equal(supportsSort('kalshi', 'volume'), true);
    assert.equal(supportsSort('polymarket', 'volume'), true);
    assert.equal(supportsSort('polymarket-us', 'volume'), false);
  });

  it('keeps open interest to the one venue that publishes it per market', () => {
    assert.equal(supportsSort('kalshi', 'open_interest'), true);
    assert.equal(supportsSort('polymarket', 'open_interest'), false);
    assert.equal(supportsSort('polymarket-us', 'open_interest'), false);
  });

  it('treats gainers and losers as one figure — a venue publishing change serves both', () => {
    for (const v of VENUES) {
      assert.equal(
        v.capabilities.sorts.includes('gainers'),
        v.capabilities.sorts.includes('losers'),
        `${v.id} serves one direction of change but not the other`,
      );
    }
  });

  it('agrees with the declared sorts for every venue and every ranking', () => {
    for (const v of VENUES) {
      for (const sort of ALL_SORTS) {
        assert.equal(supportsSort(v.id, sort), v.capabilities.sorts.includes(sort), `${v.id}/${sort}`);
      }
    }
  });

  it('declines a ranking no venue knows, rather than asking every venue for it', () => {
    for (const id of VENUE_IDS) {
      assert.equal(supportsSort(id, 'sharpe' as MoverSort), false);
    }
  });

  it('rejects an unknown venue', () => {
    assert.throws(() => supportsSort('nasdaq' as Venue, 'volume'), /Unknown venue/);
  });
});

/* ------------------------------------------------- table ↔ source modules */

describe('sourceFor', () => {
  it('has a source module for every venue in the table', () => {
    for (const id of VENUE_IDS) {
      assert.ok(sourceFor(id), `${id} has a table row but no source module`);
    }
  });

  it('gives every venue the whole verb set the routes call', () => {
    // The routes are written once and dispatched by venue, so a module missing
    // a verb is a TypeError inside a request rather than a described error.
    for (const id of VENUE_IDS) {
      const source = sourceFor(id) as unknown as Record<string, unknown>;
      for (const method of VENUE_SOURCE_METHODS) {
        assert.equal(typeof source[method], 'function', `${id}.${method}`);
      }
    }
  });

  it('maps each venue to its own module, not a neighbour\'s', () => {
    const modules = VENUE_IDS.map((id) => sourceFor(id));
    assert.equal(new Set(modules).size, VENUE_IDS.length);
    assert.equal(sourceFor('kalshi'), sourceFor('kalshi'));
  });

  it('refuses candles and trades at a venue that declares neither, without a fetch', () => {
    // The declaration and the module have to agree: the button is disabled
    // from the table, and a hand-typed `GP pmus:…` still has to be answered
    // with a described `unsupported`, not an empty chart.
    for (const v of VENUES) {
      const source = sourceFor(v.id);
      if (!v.capabilities.candles) {
        assert.throws(
          () => void source.getCandles(REAL_ID[v.id], 60, 0, 1),
          (err: Error & { code?: string }) => {
            assert.equal(err.code, 'unsupported');
            return true;
          },
          `${v.id} declares no candles but does not say so`,
        );
      }
      if (!v.capabilities.trades) {
        assert.throws(
          () => void source.getTrades(REAL_ID[v.id], 50),
          (err: Error & { code?: string }) => {
            assert.equal(err.code, 'unsupported');
            return true;
          },
          `${v.id} declares no trades but does not say so`,
        );
      }
    }
  });

  it('takes a category argument exactly where the table says the filter is honoured', () => {
    // `seriesCategoryFilter` is what a caller reads before offering a category
    // box; a module that ignores the argument must not be declaring true.
    for (const v of VENUES) {
      assert.equal(
        sourceFor(v.id).listSeries.length > 0,
        v.capabilities.seriesCategoryFilter,
        `${v.id} listSeries arity disagrees with its declaration`,
      );
    }
  });
});
