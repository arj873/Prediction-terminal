/**
 * End-to-end exercise of the FRED scrape path against a local fixture server.
 *
 * The live host (fred.stlouisfed.org) resets connections from datacentre IPs,
 * so the real network path cannot be exercised in CI. Pointing `FRED_WEB_BASE`
 * at a fixture server covers everything except the socket itself: URL
 * construction, the CSV/metadata fan-out, the "HTML instead of CSV" not-found
 * case, and the assembly of the response the client consumes.
 */

import assert from 'node:assert/strict';
import { createServer, type Server } from 'node:http';
import { after, before, describe, it } from 'node:test';
import type { AddressInfo } from 'node:net';

const CSV = `observation_date,UNRATE
2024-01-01,3.7
2024-02-01,3.9
2024-03-01,.
2024-04-01,3.9
`;

const SERIES_HTML = `<!doctype html><html><head>
<title>Unemployment Rate (UNRATE) | FRED | St. Louis Fed</title></head><body>
<h1 id="page-title">Unemployment Rate <span>(UNRATE)</span></h1>
<p><span class="series-meta-label">Units:</span> <span class="series-meta-value">Percent, Seasonally Adjusted</span></p>
<p><span class="series-meta-label">Frequency:</span> <span class="series-meta-value">Monthly</span></p>
<div class="series-obs-range">1948-01-01 to 2024-04-01</div>
<div id="notes-container">Fixture notes.</div>
</body></html>`;

const SEARCH_HTML = `<html><body>
<div><a href="/series/UNRATE">Unemployment Rate</a><div class="series-meta">Percent, Monthly, Seasonally Adjusted</div></div>
<div><a href="/series/U6RATE">Broader unemployment</a></div>
</body></html>`;

const NOT_FOUND_HTML = `<!doctype html><html><body><h1>Page not found</h1></body></html>`;

let server: Server;
let requests: string[] = [];
let fred: typeof import('../src/server/sources/fred.js');

before(async () => {
  server = createServer((req, res) => {
    const url = new URL(req.url ?? '/', 'http://localhost');
    requests.push(`${url.pathname}${url.search}`);

    if (url.pathname === '/graph/fredgraph.csv') {
      // Mirror FRED's behaviour for a bad id: an HTML page, not a 404.
      if (url.searchParams.get('id') === 'NOSUCH') {
        res.writeHead(200, { 'content-type': 'text/html' }).end(NOT_FOUND_HTML);
        return;
      }
      res.writeHead(200, { 'content-type': 'text/csv' }).end(CSV);
      return;
    }
    if (url.pathname.startsWith('/series/')) {
      res.writeHead(200, { 'content-type': 'text/html' }).end(SERIES_HTML);
      return;
    }
    if (url.pathname === '/searchresults/') {
      res.writeHead(200, { 'content-type': 'text/html' }).end(SEARCH_HTML);
      return;
    }
    res.writeHead(404).end('nope');
  });

  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address() as AddressInfo;
  process.env['FRED_WEB_BASE'] = `http://127.0.0.1:${port}`;
  delete process.env['FRED_API_KEY'];

  // Imported after the env var is set: the module reads it at load time.
  fred = await import('../src/server/sources/fred.js');
});

after(() => {
  server.close();
});

describe('getSeries against a fixture host', () => {
  it('returns observations with the missing marker preserved as null', async () => {
    const result = await fred.getSeries('UNRATE');
    assert.deepEqual(result.observations, [
      { date: '2024-01-01', value: 3.7 },
      { date: '2024-02-01', value: 3.9 },
      { date: '2024-03-01', value: null },
      { date: '2024-04-01', value: 3.9 },
    ]);
  });

  it('merges metadata scraped from the series page', async () => {
    const { series } = await fred.getSeries('UNRATE');
    assert.equal(series.id, 'UNRATE');
    assert.equal(series.title, 'Unemployment Rate');
    assert.equal(series.units, 'Percent');
    assert.equal(series.frequency, 'Monthly');
    assert.equal(series.source, 'scrape');
    assert.match(series.notes, /Fixture notes/);
  });

  it('requests the CSV endpoint with the id and date window', async () => {
    requests = [];
    await fred.getSeries('GDPC1', '2020-01-01', '2021-01-01');
    const csvRequest = requests.find((r) => r.startsWith('/graph/fredgraph.csv'));
    assert.ok(csvRequest, 'expected a fredgraph.csv request');
    assert.match(csvRequest, /id=GDPC1/);
    assert.match(csvRequest, /cosd=2020-01-01/);
    assert.match(csvRequest, /coed=2021-01-01/);
  });

  it('upper-cases the series id before hitting the network', async () => {
    requests = [];
    await fred.getSeries('cpiaucsl');
    assert.ok(requests.some((r) => r.includes('id=CPIAUCSL')));
  });

  it('reports not_found when FRED answers with a page instead of CSV', async () => {
    await assert.rejects(fred.getSeries('NOSUCH'), (err: Error & { code?: string }) => {
      assert.equal(err.code, 'not_found');
      return true;
    });
  });

  it('rejects a malformed series id without making a request', async () => {
    requests = [];
    await assert.rejects(fred.getSeries('../etc/passwd'), /not a valid FRED series id/);
    assert.equal(requests.length, 0);
  });

  it('caches, so a repeated request does not re-fetch', async () => {
    await fred.getSeries('DGS10');
    requests = [];
    await fred.getSeries('DGS10');
    assert.equal(requests.length, 0);
  });
});

describe('searchSeries against a fixture host', () => {
  it('parses results out of the search page, and says which arm answered', async () => {
    // `searchSeries` returns `Attributed<…>` now that FRED is one of eight
    // publishers behind `ECOS`: the results, plus which of its own two arms
    // produced them.
    const { value: results, source } = await fred.searchSeries('unemployment');
    assert.equal(source, 'scrape');
    assert.deepEqual(
      results.map((r) => r.id),
      ['UNRATE', 'U6RATE'],
    );
    assert.equal(results[0]!.frequency, 'Monthly');
    // Every result names its publisher, because the multi-source board merges
    // them with seven other catalogues before anyone sees them.
    assert.ok(results.every((r) => r.provider === 'fred'));
  });

  it('sends the query as the st parameter', async () => {
    requests = [];
    await fred.searchSeries('real gdp');
    const search = requests.find((r) => r.startsWith('/searchresults/'));
    assert.ok(search);
    assert.match(search, /st=real\+gdp/);
  });

  it('rejects an empty query', async () => {
    await assert.rejects(fred.searchSeries('   '), /at least one word/);
  });
});
