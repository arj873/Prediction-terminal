/**
 * Rotten Tomatoes parser tests.
 *
 * The fixtures mirror the real page structure: the score payload sits in a
 * `<script id="media-scorecard-json">` whose attributes are split across lines,
 * and search hits are `<search-page-media-row>` custom elements carrying their
 * data in attributes. Both are trimmed from live captures.
 *
 * The case that matters most is a film with no Tomatometer yet — which is
 * exactly when a Kalshi KXRT ladder has something to price, and where returning
 * `0` instead of `null` would read as "universally panned".
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  parseSearchPage,
  parseTitlePage,
  slugify,
} from '../src/server/sources/rottentomatoes.js';

function page(scorecard: unknown, opts: { title?: string; date?: string; type?: string } = {}): string {
  return `<!doctype html><html><head>
<meta property="og:title" content="${opts.title ?? 'Dune: Part Two'} | Rotten Tomatoes">
<meta property="og:type" content="${opts.type ?? 'video.movie'}">
<script type="application/ld+json">${JSON.stringify({
    '@type': opts.type === 'video.tv_show' ? 'TVSeries' : 'Movie',
    name: opts.title ?? 'Dune: Part Two',
    dateCreated: opts.date ?? '2024-03-01',
  })}</script>
</head><body>
  <script
      id="media-scorecard-json"
      data-json="mediaScorecard"
      type="application/json"
  >
  ${JSON.stringify(scorecard)}
  </script>
</body></html>`;
}

const SCORED = {
  criticsScore: {
    score: '92',
    averageRating: '8.40',
    reviewCount: 466,
    sentiment: 'POSITIVE',
    certified: true,
  },
  audienceScore: {
    score: '95',
    averageRating: '4.7',
    reviewCount: 2246,
    sentiment: 'POSITIVE',
    certifiedFresh: 'certified',
  },
  description: 'Paul Atreides unites with the Fremen.',
};

describe('parseTitlePage', () => {
  it('reads both scores as numbers', () => {
    const title = parseTitlePage(page(SCORED), 'dune_part_two', 'u');
    assert.equal(title.critics.score, 92);
    assert.equal(title.audience.score, 95);
  });

  it('reads the title, year and type from the ld+json block', () => {
    const title = parseTitlePage(page(SCORED), 'dune_part_two', 'u');
    assert.equal(title.title, 'Dune: Part Two');
    assert.equal(title.year, '2024');
    assert.equal(title.mediaType, 'Movie');
  });

  it('carries review counts and average ratings through', () => {
    const title = parseTitlePage(page(SCORED), 'dune_part_two', 'u');
    assert.equal(title.critics.reviewCount, 466);
    assert.equal(title.critics.averageRating, '8.40');
    assert.equal(title.audience.averageRating, '4.7');
  });

  it('recognises certification from either spelling', () => {
    // Critics say `certified: true`; the audience says `certifiedFresh:
    // "certified"`. Both mean the same thing and must fold to one flag.
    const title = parseTitlePage(page(SCORED), 'dune_part_two', 'u');
    assert.equal(title.critics.certified, true);
    assert.equal(title.critics.state, 'certified fresh');
    assert.equal(title.audience.certified, true);
  });

  it('reports an unreleased film as null, not zero', () => {
    const unscored = parseTitlePage(
      page({ criticsScore: { score: '' }, audienceScore: {} }, { title: 'Dune: Part Three', date: '2026-12-18' }),
      'dune_part_three',
      'u',
    );
    assert.equal(unscored.critics.score, null);
    assert.equal(unscored.audience.score, null);
    assert.equal(unscored.critics.state, 'not yet scored');
  });

  it('detects a TV series', () => {
    const show = parseTitlePage(
      page(SCORED, { title: 'The Last of Us', type: 'video.tv_show' }),
      'tv/the_last_of_us',
      'u',
    );
    assert.equal(show.mediaType, 'TV');
  });

  it('derives a rotten sentiment when the payload omits one', () => {
    const rotten = parseTitlePage(page({ criticsScore: { score: '31' } }), 'x', 'u');
    assert.equal(rotten.critics.score, 31);
    assert.equal(rotten.critics.state, 'negative');
  });

  it('throws a diagnosable error when the score payload is missing', () => {
    assert.throws(
      () => parseTitlePage('<html><body><p>nothing</p></body></html>', 'x', 'https://rt/'),
      /No score data found/,
    );
  });

  it('throws when the score payload is not valid JSON', () => {
    const broken = '<html><body><script id="media-scorecard-json" type="application/json">{not json</script></body></html>';
    assert.throws(() => parseTitlePage(broken, 'x', 'https://rt/'), /not valid JSON/);
  });
});

/* ------------------------------------------------------------------ search */

const SEARCH = `<html><body>
<search-page-media-row release-year="2024" tomatometer-score="92" tomatometer-is-certified="true">
  <a href="https://www.rottentomatoes.com/m/dune_part_two" data-qa="thumbnail-link"><img alt="Dune: Part Two"></a>
  <span data-qa="info-name">Dune: Part Two</span>
</search-page-media-row>
<search-page-media-row release-year="2026" tomatometer-score="">
  <a href="https://www.rottentomatoes.com/m/dune_part_three" data-qa="thumbnail-link"><img alt="Dune: Part Three"></a>
  <span data-qa="info-name">Dune: Part Three</span>
</search-page-media-row>
<search-page-media-row start-year="2023" tomatometer-score="96">
  <a href="https://www.rottentomatoes.com/tv/the_last_of_us" data-qa="thumbnail-link"><img alt="The Last of Us"></a>
  <span data-qa="info-name">The Last of Us</span>
</search-page-media-row>
</body></html>`;

describe('parseSearchPage', () => {
  it('extracts every hit with its slug and score', () => {
    const { results } = parseSearchPage(SEARCH, 'dune');
    assert.equal(results.length, 3);
    assert.equal(results[0]!.slug, 'dune_part_two');
    assert.equal(results[0]!.criticsScore, 92);
    assert.equal(results[0]!.year, '2024');
  });

  it('reports an unscored title as null rather than 0', () => {
    const { results } = parseSearchPage(SEARCH, 'dune');
    assert.equal(results[1]!.criticsScore, null);
  });

  it('keeps the tv/ prefix so the slug addresses the right page', () => {
    const { results } = parseSearchPage(SEARCH, 'dune');
    assert.equal(results[2]!.slug, 'tv/the_last_of_us');
    assert.equal(results[2]!.mediaType, 'TV');
    // TV rows carry `start-year` instead of `release-year`.
    assert.equal(results[2]!.year, '2023');
  });

  it('returns no results rather than throwing on an empty page', () => {
    assert.deepEqual(parseSearchPage('<html><body></body></html>', 'x').results, []);
  });
});

describe('slugify', () => {
  it('turns a typed title into RT slug form', () => {
    assert.equal(slugify('Dune: Part Two'), 'dune_part_two');
    assert.equal(slugify('Spider-Man: Brand New Day'), 'spider_man_brand_new_day');
  });

  it('drops apostrophes rather than turning them into separators', () => {
    assert.equal(slugify("Don't Look Up"), 'dont_look_up');
  });

  it('spells out an ampersand', () => {
    assert.equal(slugify('Fire & Ice'), 'fire_and_ice');
  });

  it('never leaves leading or trailing separators', () => {
    assert.equal(slugify('  ...Wicked!  '), 'wicked');
  });
});
