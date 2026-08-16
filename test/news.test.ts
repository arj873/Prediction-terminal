/**
 * News feed normalisation tests.
 *
 * The fixture is a trimmed copy of a real `data.alpaca.markets/v1beta1/news`
 * response, so the quirks under test are the wire's actual quirks: copy that
 * arrives HTML-escaped, a summary cut out of the article body with its markup
 * still attached, an item with no link at all, and — the one that silently
 * misorders a news panel — a story published in the morning and *edited* in the
 * evening, which the upstream's own sort ranks above everything published since.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  MAX_SYMBOLS,
  assertSymbols,
  normaliseArticle,
  normaliseFeed,
  plainText,
  safeUrl,
  toUnix,
} from '../src/server/sources/alpaca.js';
import { chartCommand } from '../src/client/panels/news.js';

const FEED = {
  news: [
    {
      id: 47374123,
      headline: 'Nvidia Q3 Beat: Data Center Revenue Tops Street&#39;s $30B View',
      author: 'Benzinga Newsdesk',
      created_at: '2026-08-14T20:31:07Z',
      updated_at: '2026-08-14T20:33:41Z',
      summary: '<p>Shares of <b>NVIDIA</b> rose  in after-hours trade.</p>',
      url: 'https://www.benzinga.com/news/26/08/47374123/nvidia-q3',
      images: [{ size: 'large', url: 'https://cdn.benzinga.com/files/nvda.jpeg' }],
      symbols: ['NVDA', 'amd'],
      source: 'benzinga',
    },
    {
      // Filed in the morning, corrected at 21:10. Alpaca sorts on the edit, so
      // this arrives *first* despite being seven hours older than the story above.
      id: '47370001',
      headline: 'AT&amp;T Names New CFO',
      author: '',
      created_at: '2026-08-14T13:02:00Z',
      updated_at: '2026-08-14T21:10:00Z',
      summary: '',
      url: null,
      symbols: ['T'],
      source: 'benzinga',
    },
    {
      id: 47369000,
      headline: '',
      created_at: '2026-08-14T12:00:00Z',
      updated_at: '2026-08-14T12:00:00Z',
      symbols: [],
      source: 'benzinga',
    },
  ],
  next_page_token: null,
};

/* -------------------------------------------------------------- plain text */

describe('plainText', () => {
  it('decodes the escapes the wire ships headlines in', () => {
    // Rendered with textContent, an undecoded `&amp;` prints as five characters.
    assert.equal(plainText('AT&amp;T Names New CFO'), 'AT&T Names New CFO');
    assert.equal(plainText('Tops Street&#39;s View'), "Tops Street's View");
  });

  it('drops markup a summary brings from the article body', () => {
    assert.equal(
      plainText('<p>Shares of <b>NVIDIA</b> rose  in after-hours trade.</p>'),
      'Shares of NVIDIA rose in after-hours trade.',
    );
  });

  it('leaves a bare ampersand alone', () => {
    // `&G` is not an entity; a decoder that guesses would eat the character.
    assert.equal(plainText('P&G posts Q4 beat'), 'P&G posts Q4 beat');
  });

  it('does not leak script or style text into the headline', () => {
    assert.equal(plainText('<script>alert(1)</script>Halted'), 'Halted');
  });

  it('answers for the empty and the absent', () => {
    assert.equal(plainText(''), '');
    assert.equal(plainText(undefined), '');
    assert.equal(plainText(null), '');
    assert.equal(plainText(42), '');
  });
});

/* -------------------------------------------------------------------- urls */

describe('safeUrl', () => {
  it('keeps http and https links', () => {
    assert.equal(safeUrl('https://www.benzinga.com/a'), 'https://www.benzinga.com/a');
    assert.equal(safeUrl('http://example.test/a'), 'http://example.test/a');
  });

  it('drops anything a click could execute', () => {
    // The headline renders as an anchor, so this href is one click from running.
    assert.equal(safeUrl('javascript:alert(1)'), '');
    assert.equal(safeUrl('data:text/html,<script>alert(1)</script>'), '');
    assert.equal(safeUrl('file:///etc/passwd'), '');
  });

  it('drops what is not a url at all', () => {
    assert.equal(safeUrl(null), '');
    assert.equal(safeUrl(''), '');
    assert.equal(safeUrl('   '), '');
    assert.equal(safeUrl('benzinga.com/a'), '');
  });
});

/* -------------------------------------------------------------------- time */

describe('toUnix', () => {
  it('reads RFC-3339 into unix seconds', () => {
    assert.equal(toUnix('2026-08-14T20:31:07Z'), 1786739467);
    assert.equal(toUnix('2026-08-14T20:31:07.482Z'), 1786739467);
  });

  it('returns 0 rather than NaN for junk', () => {
    assert.equal(toUnix('not a date'), 0);
    assert.equal(toUnix(undefined), 0);
    assert.equal(toUnix(1786825867), 0);
  });
});

/* ----------------------------------------------------------------- symbols */

describe('assertSymbols', () => {
  it('splits on commas and whitespace, and upper-cases', () => {
    assert.deepEqual(assertSymbols('nvda,amd msft'), ['NVDA', 'AMD', 'MSFT']);
  });

  it('accepts the shapes the wire actually tags', () => {
    assert.deepEqual(assertSymbols('BRK.B BTCUSD BTC/USD'), ['BRK.B', 'BTCUSD', 'BTC/USD']);
  });

  it('de-duplicates', () => {
    assert.deepEqual(assertSymbols('AAPL aapl AAPL'), ['AAPL']);
  });

  it('reads no symbols as the whole wire, not as an error', () => {
    assert.deepEqual(assertSymbols(''), []);
    assert.deepEqual(assertSymbols('  ,  '), []);
  });

  it('refuses anything that is not a symbol', () => {
    // These tokens end up in an outbound query string, so the whitelist is the
    // boundary — not a formatting preference.
    assert.throws(() => assertSymbols('AAPL&limit=9999'), /not a symbol/);
    assert.throws(() => assertSymbols('../../etc'), /not a symbol/);
    assert.throws(() => assertSymbols('9NVDA'), /not a symbol/);
  });

  it('caps how many symbols one request may filter on', () => {
    const many = Array.from({ length: MAX_SYMBOLS + 1 }, (_, i) => `SYM${i}`).join(',');
    assert.throws(() => assertSymbols(many), /at most/);
  });
});

/* -------------------------------------------------------------- normalising */

describe('normaliseArticle', () => {
  it('maps one story onto the wire contract', () => {
    const article = normaliseArticle(FEED.news[0]!);
    assert.deepEqual(article, {
      id: '47374123',
      headline: "Nvidia Q3 Beat: Data Center Revenue Tops Street's $30B View",
      summary: 'Shares of NVIDIA rose in after-hours trade.',
      author: 'Benzinga Newsdesk',
      publisher: 'benzinga',
      url: 'https://www.benzinga.com/news/26/08/47374123/nvidia-q3',
      time: 1786739467,
      updated: 1786739621,
      symbols: ['NVDA', 'AMD'],
    });
  });

  it('keeps a numeric id as text', () => {
    // 47374123 is small, but the ids are wide enough to be worth never doing
    // arithmetic on, and the client compares them as strings.
    assert.equal(normaliseArticle({ id: 47374123 }).id, '47374123');
    assert.equal(normaliseArticle({ id: '47370001' }).id, '47370001');
    assert.equal(normaliseArticle({}).id, '');
  });

  it('falls back to the publication time when there is no edit time', () => {
    const article = normaliseArticle({ headline: 'x', created_at: '2026-08-14T13:02:00Z' });
    assert.equal(article.updated, article.time);
    assert.equal(article.time, 1786712520);
  });

  it('leaves a link-less item with an empty url rather than a broken anchor', () => {
    assert.equal(normaliseArticle(FEED.news[1]!).url, '');
  });
});

describe('normaliseFeed', () => {
  const feed = normaliseFeed(FEED, ['NVDA'], 7, 'https://data.alpaca.markets/v1beta1/news');

  it('orders on publication, not on the last edit', () => {
    // Alpaca sorts by `updated_at`, which would put a seven-hour-old story
    // corrected at 21:10 above one published at 20:31.
    assert.deepEqual(
      feed.articles.map((a) => a.headline),
      ["Nvidia Q3 Beat: Data Center Revenue Tops Street's $30B View", 'AT&T Names New CFO'],
    );
  });

  it('drops a row with no headline instead of printing a blank line', () => {
    assert.equal(feed.articles.length, 2);
    assert.ok(feed.articles.every((a) => a.headline !== ''));
  });

  it('carries the query back so the panel can state its own scope', () => {
    assert.deepEqual(feed.symbols, ['NVDA']);
    assert.equal(feed.days, 7);
    assert.equal(feed.source, 'Alpaca / Benzinga');
  });

  it('reads a payload with no stories as an empty wire, not a failure', () => {
    const empty = normaliseFeed({ news: [] }, [], 7, 'u');
    assert.deepEqual(empty.articles, []);
  });
});

/* ------------------------------------------------------------ drill-through */

describe('chartCommand', () => {
  it('sends an equity tag to the equity chart', () => {
    assert.equal(chartCommand('NVDA'), 'STK NVDA');
    assert.equal(chartCommand('BRK.B'), 'STK BRK.B');
  });

  it('sends a crypto pair to the crypto chart, without the quote currency', () => {
    // `STK BTCUSD` prices nothing — the wire tags crypto stories as pairs.
    assert.equal(chartCommand('BTCUSD'), 'CRY BTC');
    assert.equal(chartCommand('BTC/USD'), 'CRY BTC');
    assert.equal(chartCommand('ETH-USD'), 'CRY ETH');
    assert.equal(chartCommand('SOLUSDT'), 'CRY SOL');
  });
});
