/**
 * FRED parser tests.
 *
 * These matter more than the usual unit test: fred.stlouisfed.org refuses
 * connections from datacentre IPs, so CI cannot exercise the live scrape. The
 * fixtures below reproduce the shapes FRED actually serves — current and legacy
 * CSV headers, the series page's labelled-attribute markup, and the search
 * results page — so a regression in the parsing logic is caught even where the
 * network path is not available.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { assertSeriesId, parseFredCsv, parseSearchPage, parseSeriesPage } from '../src/server/sources/fred.js';

describe('parseFredCsv', () => {
  it('parses the current observation_date header', () => {
    const rows = parseFredCsv('observation_date,UNRATE\n1948-01-01,3.4\n1948-02-01,3.8\n');
    assert.deepEqual(rows, [
      { date: '1948-01-01', value: 3.4 },
      { date: '1948-02-01', value: 3.8 },
    ]);
  });

  it('parses the legacy DATE header identically', () => {
    const rows = parseFredCsv('DATE,GDP\n1947-01-01,243.164\n');
    assert.deepEqual(rows, [{ date: '1947-01-01', value: 243.164 }]);
  });

  it('maps FRED\'s "." missing marker to null rather than dropping the row', () => {
    const rows = parseFredCsv('observation_date,DGS10\n2020-01-01,.\n2020-01-02,1.88\n');
    assert.deepEqual(rows, [
      { date: '2020-01-01', value: null },
      { date: '2020-01-02', value: 1.88 },
    ]);
  });

  it('handles CRLF line endings and a trailing blank line', () => {
    const rows = parseFredCsv('observation_date,UNRATE\r\n2024-01-01,3.7\r\n\r\n');
    assert.deepEqual(rows, [{ date: '2024-01-01', value: 3.7 }]);
  });

  it('handles quoted fields and thousands separators', () => {
    const rows = parseFredCsv('"observation_date","GDP"\n"2024-01-01","28,624.069"\n');
    assert.deepEqual(rows, [{ date: '2024-01-01', value: 28624.069 }]);
  });

  it('ignores rows whose first column is not a date', () => {
    const rows = parseFredCsv('observation_date,X\nnot-a-date,1\n2024-01-01,2\n');
    assert.deepEqual(rows, [{ date: '2024-01-01', value: 2 }]);
  });

  it('returns an empty array for an empty body', () => {
    assert.deepEqual(parseFredCsv(''), []);
  });

  it('preserves negative and exponential values', () => {
    const rows = parseFredCsv('observation_date,X\n2024-01-01,-2.5\n2024-02-01,1.2e3\n');
    assert.deepEqual(rows, [
      { date: '2024-01-01', value: -2.5 },
      { date: '2024-02-01', value: 1200 },
    ]);
  });
});

const SERIES_PAGE = `
<!doctype html><html><head>
<title>Unemployment Rate (UNRATE) | FRED | St. Louis Fed</title>
<meta property="og:title" content="Unemployment Rate (UNRATE) | FRED | St. Louis Fed">
</head><body>
  <h1 id="page-title">Unemployment Rate <span class="series-id">(UNRATE)</span></h1>
  <div class="series-meta">
    <p><span class="series-meta-label">Units:</span> <span class="series-meta-value">Percent, Seasonally Adjusted</span></p>
    <p><span class="series-meta-label">Frequency:</span> <span class="series-meta-value">Monthly</span></p>
    <p><span class="series-meta-label">Seasonal Adjustment:</span> <span class="series-meta-value">Seasonally Adjusted</span></p>
    <p><span class="series-meta-label">Last Updated:</span> <span class="series-meta-value">Aug 1, 2026 7:46 AM CDT</span></p>
  </div>
  <div class="series-obs-range">1948-01-01 to 2026-07-01</div>
  <div id="notes-container">The unemployment rate represents the number of unemployed as a percentage of the labor force.</div>
</body></html>`;

describe('parseSeriesPage', () => {
  it('extracts the title without the FRED suffix or the id', () => {
    const meta = parseSeriesPage(SERIES_PAGE, 'UNRATE');
    assert.equal(meta.title, 'Unemployment Rate');
    assert.equal(meta.id, 'UNRATE');
  });

  it('reads labelled attributes and strips the adjustment suffix off units', () => {
    const meta = parseSeriesPage(SERIES_PAGE, 'UNRATE');
    assert.equal(meta.units, 'Percent');
    assert.equal(meta.frequency, 'Monthly');
    assert.equal(meta.seasonalAdjustment, 'Seasonally Adjusted');
    assert.match(meta.lastUpdated, /Aug 1, 2026/);
  });

  it('extracts the observation range', () => {
    const meta = parseSeriesPage(SERIES_PAGE, 'UNRATE');
    assert.equal(meta.observationStart, '1948-01-01');
    assert.equal(meta.observationEnd, '2026-07-01');
  });

  it('extracts notes', () => {
    const meta = parseSeriesPage(SERIES_PAGE, 'UNRATE');
    assert.match(meta.notes, /percentage of the labor force/);
  });

  it('falls back to og:title when the page heading is missing', () => {
    const html = `<html><head><meta property="og:title" content="Real Gross Domestic Product (GDPC1) | FRED"></head><body></body></html>`;
    const meta = parseSeriesPage(html, 'GDPC1');
    assert.equal(meta.title, 'Real Gross Domestic Product');
  });

  it('degrades to the series id rather than throwing on unrecognised markup', () => {
    // The whole point: losing the "Units:" line must never cost you the data.
    const meta = parseSeriesPage('<html><body><div>totally different</div></body></html>', 'DGS10');
    assert.equal(meta.title, 'DGS10');
    assert.equal(meta.units, '');
    assert.equal(meta.frequency, '');
  });
});

const SEARCH_PAGE = `
<!doctype html><html><body>
  <div class="search-results">
    <div class="series-pager-item">
      <a href="/series/UNRATE">Unemployment Rate</a>
      <div class="series-meta">Percent, Monthly, Seasonally Adjusted</div>
    </div>
    <div class="series-pager-item">
      <a href="https://fred.stlouisfed.org/series/U6RATE">Total Unemployed, Plus All Persons Marginally Attached</a>
      <div class="series-meta">Percent, Monthly, Seasonally Adjusted</div>
    </div>
    <div class="series-pager-item">
      <a href="/series/UNRATE">Unemployment Rate</a>
    </div>
    <a href="/series/">Browse series</a>
    <a href="/categories/32991">A category</a>
  </div>
</body></html>`;

describe('parseSearchPage', () => {
  it('collects series links and de-duplicates repeats', () => {
    const results = parseSearchPage(SEARCH_PAGE);
    assert.equal(results.length, 2);
    assert.deepEqual(
      results.map((r) => r.id),
      ['UNRATE', 'U6RATE'],
    );
  });

  it('handles both relative and absolute series hrefs', () => {
    const results = parseSearchPage(SEARCH_PAGE);
    assert.equal(results[1]!.id, 'U6RATE');
  });

  it('splits the metadata line into units / frequency / adjustment', () => {
    const [first] = parseSearchPage(SEARCH_PAGE);
    assert.equal(first!.units, 'Percent');
    assert.equal(first!.frequency, 'Monthly');
    assert.equal(first!.seasonalAdjustment, 'Seasonally Adjusted');
  });

  it('ignores navigation links that are not individual series', () => {
    const results = parseSearchPage(SEARCH_PAGE);
    assert.ok(!results.some((r) => r.title === 'Browse series'));
    assert.ok(!results.some((r) => r.id.startsWith('32991')));
  });

  it('returns an empty array when nothing matched', () => {
    assert.deepEqual(parseSearchPage('<html><body>No results.</body></html>'), []);
  });
});

describe('assertSeriesId', () => {
  it('upper-cases and accepts valid ids', () => {
    assert.equal(assertSeriesId('unrate'), 'UNRATE');
    assert.equal(assertSeriesId('GDPC1'), 'GDPC1');
    assert.equal(assertSeriesId('CPIAUCSL'), 'CPIAUCSL');
  });

  it('rejects path traversal and query injection', () => {
    for (const bad of ['../../etc/passwd', 'UNRATE&id=X', 'UN RATE', '', 'a'.repeat(65)]) {
      assert.throws(() => assertSeriesId(bad), /not a valid FRED series id/);
    }
  });
});
