/**
 * Parser tests for the awards, trends, release and podcast feeds.
 *
 * Same rule as the other entertainment tests: pin the cases where a
 * plausible-looking parser returns a number that is wrong without ever failing.
 * Every one of these was a real bug caught against the live upstream —
 *
 *   · an artist search that answers with a covers band's newest compilation,
 *     because it releases weekly and sorts to the top of "newest first"
 *   · a release dedupe aggressive enough to fold a re-recording into the
 *     original, hiding a record that a separate market trades
 *   · a trends feed whose namespaced fields collapse onto each other under an
 *     HTML parser, so the traffic figure reads as the picture URL
 *   · an award ceremony that has not happened, which must read as "not yet"
 *     rather than as an empty result or an error
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { expandAlias } from '../src/server/sources/awards.js';
import { parseFeed } from '../src/server/sources/podcasts.js';
import { parseReleases } from '../src/server/sources/releases.js';
import { assertKind } from '../src/server/sources/releases.js';
import { assertGeo, parseTraffic, parseTrendsRss } from '../src/server/sources/trends.js';
import { assertView } from '../src/server/sources/podcasts.js';

/* ------------------------------------------------------------------ awards */

describe('awards expandAlias', () => {
  it('expands a short name to the label Wikidata files it under', () => {
    assert.equal(expandAlias('best picture'), 'Academy Award for Best Picture');
    assert.equal(expandAlias('AOTY'), 'Grammy Award for Album of the Year');
    assert.equal(expandAlias('drama series'), 'Primetime Emmy Award for Outstanding Drama Series');
  });

  it('is insensitive to case and inner spacing', () => {
    assert.equal(expandAlias('  Best   Picture '), 'Academy Award for Best Picture');
  });

  /**
   * Wikidata labels this "The Game Awards − Game of the Year" with a U+2212
   * minus sign, and searching for the label verbatim finds nothing at all. The
   * alias has to be the form that resolves, not the form that reads correctly.
   */
  it('avoids the Game Awards label that does not resolve', () => {
    const expanded = expandAlias('game of the year');
    assert.equal(expanded, 'Game Awards Game of the Year');
    assert.ok(!expanded.startsWith('The '), 'leading article breaks the entity search');
    assert.ok(!/[−–—]/.test(expanded), 'the dash in the real label is not a hyphen');
  });

  it('passes an unrecognised name through for the search to resolve', () => {
    assert.equal(expandAlias('Palme d’Or'), 'Palme d’Or');
  });
});

/* ------------------------------------------------------------------ trends */

describe('trends parseTraffic', () => {
  it('scales Google’s suffixes', () => {
    assert.equal(parseTraffic('500+'), 500);
    assert.equal(parseTraffic('20K+'), 20_000);
    assert.equal(parseTraffic('2M+'), 2_000_000);
    assert.equal(parseTraffic('1.5M+'), 1_500_000);
  });

  /**
   * "Google did not state a figure" and "nobody searched it" are different
   * facts, and only the first one is ever true here — the same rule as a film
   * with no Tomatometer reporting `null` rather than 0.
   */
  it('returns null for an absent or unparseable figure, never 0', () => {
    assert.equal(parseTraffic(''), null);
    assert.equal(parseTraffic('   '), null);
    assert.equal(parseTraffic('lots'), null);
  });
});

describe('trends assertGeo', () => {
  it('defaults to US and uppercases', () => {
    assert.equal(assertGeo(''), 'US');
    assert.equal(assertGeo('gb'), 'GB');
  });

  it('rejects anything that is not a two-letter code', () => {
    assert.throws(() => assertGeo('USA'), /not a country code/);
    assert.throws(() => assertGeo('1'), /not a country code/);
  });
});

const TRENDS_RSS = `<?xml version="1.0" encoding="UTF-8"?>
<rss xmlns:ht="https://trends.google.com/trending/rss" version="2.0">
 <channel>
  <title>Daily Search Trends</title>
  <item>
   <title>madison keys</title>
   <ht:approx_traffic>1000+</ht:approx_traffic>
   <pubDate>Sun, 16 Aug 2026 09:50:00 -0700</pubDate>
   <ht:picture>https://example.test/pic.jpg</ht:picture>
   <ht:picture_source>ESPN</ht:picture_source>
   <ht:news_item>
    <ht:news_item_title>Keys advances in Cincinnati</ht:news_item_title>
    <ht:news_item_url>https://example.test/a</ht:news_item_url>
    <ht:news_item_source>ESPN</ht:news_item_source>
   </ht:news_item>
   <ht:news_item>
    <ht:news_item_title>Second story</ht:news_item_title>
    <ht:news_item_url>https://example.test/b</ht:news_item_url>
    <ht:news_item_source>Reuters</ht:news_item_source>
   </ht:news_item>
  </item>
  <item>
   <title>newcastle</title>
   <pubDate>Sun, 16 Aug 2026 09:40:00 -0700</pubDate>
  </item>
 </channel>
</rss>`;

describe('trends parseTrendsRss', () => {
  const list = parseTrendsRss(TRENDS_RSS, 'US', 'https://trends.test/rss');

  it('ranks by document order, since the feed carries no numbers', () => {
    assert.deepEqual(list.entries.map((e) => e.rank), [1, 2]);
    assert.equal(list.entries[0]!.query, 'madison keys');
    assert.equal(list.entries[1]!.query, 'newcastle');
  });

  /**
   * `ht:picture` and `ht:picture_source` differ only after the prefix, and an
   * HTML parse mangles the namespace so both resolve to the same lookup — which
   * silently puts an image URL where the traffic figure belongs. Parsing as XML
   * is what keeps them apart, so this asserts the values that would collide.
   */
  it('keeps namespaced siblings distinct', () => {
    const first = list.entries[0]!;
    assert.equal(first.trafficFloor, 1000);
    assert.equal(first.headlineSource, 'ESPN');
    assert.equal(first.headline, 'Keys advances in Cincinnati');
    assert.ok(!first.headline.includes('http'), 'a URL leaked into the headline');
  });

  it('takes the first news item but counts them all', () => {
    assert.equal(list.entries[0]!.articles, 2);
    assert.equal(list.entries[0]!.headlineUrl, 'https://example.test/a');
  });

  it('keeps an item with no traffic figure, reporting null', () => {
    const second = list.entries[1]!;
    assert.equal(second.trafficFloor, null);
    assert.equal(second.articles, 0);
    assert.equal(second.headline, '');
  });

  it('throws a diagnosable error on a feed with no items', () => {
    assert.throws(
      () => parseTrendsRss('<rss><channel></channel></rss>', 'US', 'https://trends.test/rss'),
      /No trending items/,
    );
  });
});

/* ---------------------------------------------------------------- releases */

const APPLE_ALBUMS = {
  resultCount: 6,
  results: [
    // A covers act. It releases weekly, so it is the newest row in the payload
    // and would head a "latest release" panel if it were not filtered out.
    {
      collectionName: '8Waves Of Popular Covers, Vol. 56',
      artistName: '8waves',
      releaseDate: '2026-08-14T07:00:00Z',
      trackCount: 10,
      primaryGenreName: 'Pop',
    },
    {
      collectionName: 'The Life of a Showgirl',
      artistName: 'Taylor Swift',
      releaseDate: '2025-10-03T07:00:00Z',
      trackCount: 12,
      primaryGenreName: 'Pop',
    },
    // A re-recording. Distinct release, distinct market — must survive dedupe.
    {
      collectionName: '1989 (Taylor’s Version)',
      artistName: 'Taylor Swift',
      releaseDate: '2023-10-27T07:00:00Z',
      trackCount: 21,
      primaryGenreName: 'Pop',
    },
    {
      collectionName: '1989',
      artistName: 'Taylor Swift',
      releaseDate: '2014-10-27T07:00:00Z',
      trackCount: 13,
      primaryGenreName: 'Pop',
    },
    // Same record, same day, listed twice across storefronts.
    {
      collectionName: '1989',
      artistName: 'Taylor Swift',
      releaseDate: '2014-10-27T07:00:00Z',
      trackCount: 13,
      primaryGenreName: 'Pop',
    },
    // A collaboration credited to two artists still counts as hers.
    {
      collectionName: 'Bring Your Love - Single',
      artistName: 'Madonna & Taylor Swift',
      releaseDate: '2026-04-30T07:00:00Z',
      trackCount: 1,
      primaryGenreName: 'Pop',
    },
  ],
};

describe('releases parseReleases', () => {
  const releases = parseReleases(APPLE_ALBUMS, 'album', 'taylor swift');

  /**
   * `attribute=artistTerm` narrows the upstream search but does not close it —
   * five of fifty live results for "taylor swift" were tribute acts. They are
   * the most recent rows, so without this filter the panel answers "her latest
   * album" with a covers compilation.
   */
  it('holds results to the artist that was asked for', () => {
    assert.ok(
      !releases.some((r) => r.artist === '8waves'),
      'a covers act survived the artist filter',
    );
    assert.ok(releases.every((r) => /taylor swift/i.test(r.artist)));
  });

  it('keeps a collaboration credited to more than one artist', () => {
    assert.ok(releases.some((r) => r.artist === 'Madonna & Taylor Swift'));
  });

  /**
   * Apple returns relevance order however you sort it — `sort=recent` is
   * accepted and ignored — so the ordering has to happen here. A panel showing
   * the most *popular* release under a "latest" heading is wrong in a way
   * nobody would notice.
   */
  it('sorts newest first rather than trusting the upstream order', () => {
    const dates = releases.map((r) => r.date);
    assert.deepEqual(dates, [...dates].sort().reverse());
    assert.equal(releases[0]!.date, '2026-04-30');
  });

  it('drops an exact duplicate listed across storefronts', () => {
    assert.equal(releases.filter((r) => r.title === '1989').length, 1);
  });

  /**
   * The tempting dedupe — strip the bracketed suffix — folds "1989" together
   * with "1989 (Taylor's Version)". They are two releases, traded by two
   * markets, and hiding one answers "has it come out" incorrectly.
   */
  it('does not fold a re-recording into the original', () => {
    assert.ok(releases.some((r) => r.title === '1989'));
    assert.ok(releases.some((r) => r.title === '1989 (Taylor’s Version)'));
  });

  it('trims the timestamp off the release date', () => {
    assert.ok(releases.every((r) => /^\d{4}-\d{2}-\d{2}$/.test(r.date)));
  });

  it('skips a result with no usable date rather than dating it today', () => {
    const parsed = parseReleases(
      { results: [{ collectionName: 'Untitled', artistName: 'Adele', releaseDate: '' }] },
      'album',
      'adele',
    );
    assert.deepEqual(parsed, []);
  });

  it('reads the track name for a song search and the album name otherwise', () => {
    const body = {
      results: [
        {
          trackName: 'Manchild',
          collectionName: 'Manchild - Single',
          artistName: 'Sabrina Carpenter',
          releaseDate: '2025-06-05T07:00:00Z',
        },
      ],
    };
    assert.equal(parseReleases(body, 'song', 'sabrina carpenter')[0]!.title, 'Manchild');
    assert.equal(
      parseReleases(body, 'album', 'sabrina carpenter')[0]!.title,
      'Manchild - Single',
    );
  });
});

describe('releases assertKind', () => {
  it('accepts singular and plural', () => {
    assert.equal(assertKind('album'), 'album');
    assert.equal(assertKind('songs'), 'song');
    assert.equal(assertKind(''), 'album');
  });

  /**
   * Apple's Search API still answers 200 for a film query and returns zero
   * results for every title. Offering the argument would make the film
   * release-date markets look covered when nothing can answer them.
   */
  it('rejects film, which the upstream no longer serves', () => {
    assert.throws(() => assertKind('movie'), /not a release kind/);
    assert.throws(() => assertKind('film'), /not a release kind/);
  });
});

/* ---------------------------------------------------------------- podcasts */

const APPLE_PODCASTS = {
  feed: {
    title: 'Top Shows',
    updated: 'Sun, 16 Aug 2026 17:12:14 +0000',
    results: [
      {
        name: 'The Daily',
        artistName: 'The New York Times',
        genres: [{ name: 'News' }, { name: 'Daily News' }],
        url: 'https://podcasts.apple.test/1',
        releaseDate: '2026-08-16T09:00:00Z',
      },
      {
        name: 'The Joe Rogan Experience',
        artistName: 'Joe Rogan',
        genres: [{ name: 'Comedy' }],
        // Apple's own spelling on the live episode chart. Not a typo in this
        // fixture — see the test below.
        contentAdvisoryRating: 'Explict',
        url: 'https://podcasts.apple.test/2',
      },
      // No name: Apple occasionally ships a partial row. Skipped, not rendered
      // as a blank rank.
      { artistName: 'Nobody', genres: [{ name: 'News' }] },
    ],
  },
};

describe('podcasts parseFeed', () => {
  const chart = parseFeed(APPLE_PODCASTS, 'top', 'us', 'https://apple.test/feed');

  it('ranks by document order, which is the only order Apple gives', () => {
    assert.deepEqual(chart.entries.map((e) => e.rank), [1, 2]);
    assert.equal(chart.entries[0]!.name, 'The Daily');
  });

  it('skips a row with no name instead of emitting a blank rank', () => {
    assert.equal(chart.entries.length, 2);
    assert.ok(chart.entries.every((e) => e.name !== ''));
  });

  it('takes the first genre, which Apple orders by relevance', () => {
    assert.equal(chart.entries[0]!.genre, 'News');
  });

  /**
   * Apple spells its own advisory rating `"Explict"` on the live episode chart —
   * one `i` short. Testing for `=== 'explicit'` matches nothing and reports a
   * chart of exclusively clean content, which is wrong without ever looking
   * broken. Both spellings must read as explicit.
   */
  it('reads Apple’s misspelled "Explict" as explicit', () => {
    assert.equal(chart.entries[0]!.explicit, false);
    assert.equal(chart.entries[1]!.explicit, true);

    const spellings = parseFeed(
      {
        feed: {
          results: [
            { name: 'a', contentAdvisoryRating: 'Explict' },
            { name: 'b', contentAdvisoryRating: 'Explicit' },
            { name: 'c', contentAdvisoryRating: 'Clean' },
            { name: 'd' },
          ],
        },
      },
      'top',
      'us',
      'https://apple.test/feed',
    );
    assert.deepEqual(spellings.entries.map((e) => e.explicit), [true, true, false, false]);
  });

  /**
   * The show chart carries a date and the episode chart does not, which is the
   * opposite way round from what the columns suggest. An absent date stays
   * empty so the panel can decide whether the column is worth drawing at all,
   * rather than rendering a column of dashes.
   */
  it('trims the release timestamp to a date, and leaves it empty when absent', () => {
    assert.equal(chart.entries[0]!.released, '2026-08-16');
    assert.equal(chart.entries[1]!.released, '');
  });

  it('uppercases the country and keeps Apple’s own stamp', () => {
    assert.equal(chart.country, 'US');
    assert.equal(chart.updated, 'Sun, 16 Aug 2026 17:12:14 +0000');
  });

  /**
   * An empty chart is a real upstream state — not every country publishes every
   * chart — so it gets a `not_found` with a usable hint rather than a parse
   * failure that reads like a broken selector.
   */
  it('reports an empty chart as not_found, not as a parse failure', () => {
    assert.throws(
      () => parseFeed({ feed: { results: [] } }, 'top', 'zz', 'https://apple.test/feed'),
      /empty top chart for ZZ/,
    );
  });
});

describe('podcasts assertView', () => {
  it('normalises the spellings a person would type', () => {
    assert.equal(assertView(''), 'top');
    assert.equal(assertView('shows'), 'top');
    assert.equal(assertView('episode'), 'episodes');
  });

  it('rejects an unknown view', () => {
    assert.throws(() => assertView('charts'), /not a podcast chart/);
  });
});
