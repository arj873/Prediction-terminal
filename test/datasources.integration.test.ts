/**
 * End-to-end exercise of the new publishers against a local fixture server.
 *
 * The live hosts cannot be relied on in CI: the OECD throttles a shared
 * datacentre address with HTTP 500, the BLS caps an unregistered IP at 25
 * queries a day, and the EIA and Polygon answer nothing at all without a key.
 * Pointing each module's `*_API_BASE` at a fixture covers everything except the
 * socket — URL construction, the year-window fan-out, the multi-request Federal
 * Reserve index, the response assembly the client consumes, and the registry
 * that fans a search across all of them.
 */

import assert from 'node:assert/strict';
import { createServer, type Server } from 'node:http';
import { after, before, describe, it } from 'node:test';
import type { AddressInfo } from 'node:net';

/* ------------------------------------------------------------- fixtures */

const BLS_BODY = {
  status: 'REQUEST_SUCCEEDED',
  message: [],
  Results: {
    series: [
      {
        seriesID: 'LNS14000000',
        data: [
          { year: '2024', period: 'M13', periodName: 'Annual', value: '4.0' },
          { year: '2024', period: 'M02', periodName: 'February', value: '3.9' },
          { year: '2024', period: 'M01', periodName: 'January', value: '3.7' },
        ],
      },
    ],
  },
};

const ECB_CSV = `KEY,FREQ,TIME_PERIOD,OBS_VALUE,TITLE,UNIT
EXR.D.USD.EUR.SP00.A,D,2026-08-12,1.1545,US dollar/Euro,USD
EXR.D.USD.EUR.SP00.A,D,2026-08-13,1.1602,US dollar/Euro,USD
`;

const FED_CHOOSER = `<html><body>
<div><input type="radio" value="rel=H15&amp;series=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&amp;type=package" /> Treasury Constant Maturities [csv]</div>
<div><input type="radio" value="rel=H15&amp;series=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb&amp;type=package" /> Monthly Averages [csv]</div>
</body></html>`;

const FED_DAILY = `"Series Description","Market yield, 10-year"
"Unit:","Percent:_Per_Year"
"Multiplier:","1"
"Currency:","NA"
"Unique Identifier: ","H15/H15/RIFLGFCY10_N.B"
"Time Period","RIFLGFCY10_N.B"
2026-08-12,4.70
2026-08-13,4.72
`;

const FED_MONTHLY = `"Series Description","Federal funds effective rate"
"Unit:","Percent:_Per_Year"
"Multiplier:","1"
"Currency:","NA"
"Unique Identifier: ","H15/H15/RIFSPFF_N.M"
"Time Period","RIFSPFF_N.M"
2026-06,4.33
2026-07,4.33
`;

const EIA_BODY = {
  response: {
    frequency: 'weekly',
    data: [
      { period: '2026-08-03', value: 3.09, units: '$/GAL', 'series-description': 'US regular gasoline' },
      { period: '2026-08-10', value: 3.11, units: '$/GAL', 'series-description': 'US regular gasoline' },
    ],
  },
};

/* --------------------------------------------------------------- server */

let server: Server;
let requests: string[] = [];
let datasources: typeof import('../src/server/sources/datasources.js');
let bls: typeof import('../src/server/sources/bls.js');
let fed: typeof import('../src/server/sources/feddata.js');
let eia: typeof import('../src/server/sources/eia.js');

before(async () => {
  server = createServer((req, res) => {
    const url = new URL(req.url ?? '/', 'http://localhost');
    requests.push(`${url.pathname}${url.search}`);

    const send = (status: number, type: string, body: string): void => {
      res.writeHead(status, { 'content-type': type });
      res.end(body);
    };

    // ---- BLS: a POST whose body carries the year window ------------------
    if (url.pathname.startsWith('/v1/timeseries/data')) {
      let body = '';
      req.on('data', (chunk) => (body += chunk));
      req.on('end', () => {
        requests.push(`POST-BODY ${body}`);
        send(200, 'application/json', JSON.stringify(BLS_BODY));
      });
      return;
    }

    // ---- ECB / SDMX -------------------------------------------------------
    if (url.pathname.startsWith('/service/data/')) return send(200, 'text/csv', ECB_CSV);

    // ---- Federal Reserve DDP ---------------------------------------------
    if (url.pathname === '/Choose.aspx') return send(200, 'text/html', FED_CHOOSER);
    if (url.pathname === '/Output.aspx') {
      const hash = url.searchParams.get('series');
      return send(200, 'text/csv', hash === 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' ? FED_DAILY : FED_MONTHLY);
    }

    // ---- EIA --------------------------------------------------------------
    if (url.pathname.startsWith('/seriesid/')) {
      return send(200, 'application/json', JSON.stringify(EIA_BODY));
    }

    send(404, 'text/plain', 'no fixture');
  });

  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address() as AddressInfo;
  const base = `http://127.0.0.1:${port}`;

  process.env['BLS_API_BASE'] = base;
  process.env['ECB_API_BASE'] = `${base}/service`;
  process.env['FED_DDP_BASE'] = base;
  process.env['EIA_API_BASE'] = base;
  process.env['EIA_API_KEY'] = 'fixture-key';

  // Imported after the environment is set, so each module reads the fixture base.
  bls = await import('../src/server/sources/bls.js');
  fed = await import('../src/server/sources/feddata.js');
  eia = await import('../src/server/sources/eia.js');
  datasources = await import('../src/server/sources/datasources.js');

  const { cache } = await import('../src/server/lib/cache.js');
  cache.clear();
});

after(() => {
  server.close();
  delete process.env['BLS_API_BASE'];
  delete process.env['ECB_API_BASE'];
  delete process.env['FED_DDP_BASE'];
  delete process.env['EIA_API_BASE'];
  delete process.env['EIA_API_KEY'];
});

/* ----------------------------------------------------------------- tests */

describe('BLS against a fixture host', () => {
  it('assembles a series, dropping the annual-average rows', async () => {
    const { series, observations } = await bls.getSeries('LNS14000000', '2024', '2024');
    assert.equal(series.provider, 'bls');
    assert.equal(series.id, 'LNS14000000');
    assert.equal(series.title, 'Unemployment rate, seasonally adjusted');
    assert.equal(series.sourceUrl, 'https://data.bls.gov/timeseries/LNS14000000');
    // M13 is the annual average and must not become a thirteenth month.
    assert.deepEqual(observations, [
      { date: '2024-01-01', value: 3.7 },
      { date: '2024-02-01', value: 3.9 },
    ]);
    assert.equal(series.observationStart, '2024-01-01');
    assert.equal(series.observationEnd, '2024-02-01');
  });

  it('splits a long range into windows the API would accept', async () => {
    requests = [];
    await bls.getSeries('CUUR0000SA0', '1990', '2024');
    const bodies = requests.filter((r) => r.startsWith('POST-BODY'));
    // 35 years, unregistered, is four ten-year windows.
    assert.equal(bodies.length, 4);
    assert.match(bodies[0]!, /"startyear":"1990"/);
    assert.match(bodies[0]!, /"endyear":"1999"/);
    assert.match(bodies[3]!, /"endyear":"2024"/);
  });

  it('rejects a malformed series id before making a request', async () => {
    requests = [];
    await assert.rejects(bls.getSeries('not a series'), /not a valid BLS series id/);
    assert.equal(requests.length, 0);
  });
});

describe('the Federal Reserve DDP against a fixture host', () => {
  it('builds a release index and resolves a series through it', async () => {
    requests = [];
    const { series, observations } = await fed.getSeries('H15/RIFLGFCY10_N.B', '2026-08-01');

    assert.equal(series.provider, 'fed');
    assert.equal(series.units, 'Percent Per Year');
    assert.equal(series.frequency, 'Business daily');
    assert.deepEqual(observations, [
      { date: '2026-08-12', value: 4.7 },
      { date: '2026-08-13', value: 4.72 },
    ]);

    // The chooser page, one probe per package, then the ranged download.
    assert.ok(requests.some((r) => r.startsWith('/Choose.aspx?rel=H15')));
    assert.equal(requests.filter((r) => r.includes('lastobs=1')).length, 2);
  });

  it('sends both bounds, because an open-ended range returns nothing', async () => {
    const ranged = requests.find((r) => r.includes('Output.aspx') && !r.includes('lastobs=1'));
    assert.ok(ranged);
    assert.match(ranged, /from=08%2F01%2F2026/);
    assert.match(ranged, /to=\d{2}%2F\d{2}%2F\d{4}/);
    assert.match(ranged, /lastobs=&/);
  });

  it('finds a series in the second package, and dates its monthly rows', async () => {
    const { observations, series } = await fed.getSeries('H15/RIFSPFF_N.M', '2026-01-01');
    assert.equal(series.frequency, 'Monthly');
    assert.deepEqual(observations, [
      { date: '2026-06-01', value: 4.33 },
      { date: '2026-07-01', value: 4.33 },
    ]);
  });

  it('names the release’s own series when asked for the release alone', async () => {
    await assert.rejects(fed.getSeries('H15'), /holds 2 series — name one/);
  });

  it('reports an unknown series rather than charting a neighbouring column', async () => {
    await assert.rejects(fed.getSeries('H15/NOSUCH'), /has no series called NOSUCH/);
  });
});

describe('the EIA against a fixture host', () => {
  it('reads the compatibility route for a v1-style id', async () => {
    requests = [];
    const { series, observations } = await eia.getSeries('PET.EMM_EPMR_PTE_NUS_DPG.W');
    assert.equal(series.provider, 'eia');
    assert.equal(series.units, '$/GAL');
    assert.equal(series.frequency, 'weekly');
    assert.equal(series.source, 'seriesid');
    assert.equal(observations.length, 2);
    assert.ok(requests.some((r) => r.startsWith('/seriesid/PET.EMM_EPMR_PTE_NUS_DPG.W')));
  });

  it('asks for values ascending, so a truncation loses the oldest', async () => {
    const call = requests.find((r) => r.startsWith('/seriesid/'));
    assert.ok(call);
    assert.match(call, /sort%5B0%5D%5Bdirection%5D=asc/);
    assert.match(call, /data%5B0%5D=value/);
  });
});

describe('the registry', () => {
  it('routes an unprefixed reference to FRED and a prefixed one to its publisher', async () => {
    const { series } = await datasources.getSeries('bls:LNS14000000', '2024', '2024');
    assert.equal(series.provider, 'bls');
  });

  it('refuses a source that publishes no chartable series', async () => {
    await assert.rejects(
      datasources.getSeries('sec:AAPL'),
      /does not publish chartable series/,
    );
  });

  it('reports a bad prefix as a usage error, not as a FRED 404', async () => {
    await assert.rejects(datasources.getSeries('bloomberg:SPX'), /Unknown source prefix/);
  });

  it('interleaves results so one large catalogue cannot fill the board', async () => {
    // FRED matches almost everything and would otherwise bury the primary
    // publisher a reader ran `ECOS` in order to discover.
    const { results } = await datasources.searchSeries('unemployment', ['bls', 'ecb', 'oecd'], 6);
    const providers = results.map((r) => r.provider);
    assert.ok(providers.length > 0);
    assert.ok(new Set(providers).size > 1, `only one publisher answered: ${providers.join(', ')}`);
    assert.notDeepEqual(providers, [...providers].sort());
  });

  it('names a publisher it could not ask instead of silently shrinking the board', async () => {
    delete process.env['EIA_API_KEY'];
    const { skipped } = await datasources.searchSeries('oil', ['eia'], 10);
    assert.equal(skipped.length, 1);
    assert.equal(skipped[0]!.provider, 'eia');
    assert.match(skipped[0]!.hint, /EIA_API_KEY/);
    process.env['EIA_API_KEY'] = 'fixture-key';
  });

  it('lists every source with its availability', () => {
    const statuses = datasources.sourceStatuses();
    assert.equal(statuses.length, 12);
    const fredRow = statuses.find((s) => s.id === 'fred');
    assert.equal(fredRow?.idExample, 'UNRATE');
    const blsRow = statuses.find((s) => s.id === 'bls');
    // A non-default source's example carries its prefix, so it can be typed
    // straight into `ECO` from the board.
    assert.equal(blsRow?.idExample, 'bls:LNS14000000');
  });

  it('rejects an unknown name in a source filter', () => {
    assert.throws(() => datasources.parseSourceList('bls,nasdaq'), /"nasdaq" is not a data source/);
    assert.deepEqual(datasources.parseSourceList('bls, ecb ,bls'), ['bls', 'ecb']);
  });
});
