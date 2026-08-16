/**
 * End-to-end exercise of the news fetch path against a local fixture server.
 *
 * The live host needs a credential nobody has in CI, so the real network path
 * cannot be exercised there. Pointing `ALPACA_DATA_BASE` at a fixture server
 * covers everything except the socket: that the key pair is sent as headers
 * rather than in the query string, that the request overrides the two upstream
 * defaults that would otherwise cost us headlines, and that the two failures an
 * operator will actually hit — no key, and a wrong key — arrive as distinct,
 * actionable errors rather than as a generic bad gateway.
 */

import assert from 'node:assert/strict';
import { createServer, type IncomingHttpHeaders, type Server } from 'node:http';
import { after, before, beforeEach, describe, it } from 'node:test';
import type { AddressInfo } from 'node:net';

const PAYLOAD = {
  news: [
    {
      id: 47374123,
      headline: 'Nvidia Q3 Beat: Data Center Revenue Tops Street&#39;s View',
      author: 'Benzinga Newsdesk',
      created_at: '2026-08-14T20:31:07Z',
      updated_at: '2026-08-14T20:33:41Z',
      summary: '<p>Shares rose in after-hours trade.</p>',
      url: 'https://www.benzinga.com/news/26/08/47374123/nvidia-q3',
      symbols: ['NVDA'],
      source: 'benzinga',
    },
  ],
  next_page_token: null,
};

interface Seen {
  path: string;
  query: URLSearchParams;
  headers: IncomingHttpHeaders;
}

let server: Server;
let requests: Seen[] = [];
let alpaca: typeof import('../src/server/sources/alpaca.js');

const GOOD_KEY = 'PKTESTKEYID';
const GOOD_SECRET = 'testsecret';

function useCredentials(keyId: string | undefined, secret: string | undefined): void {
  if (keyId === undefined) delete process.env['ALPACA_API_KEY_ID'];
  else process.env['ALPACA_API_KEY_ID'] = keyId;
  if (secret === undefined) delete process.env['ALPACA_API_SECRET_KEY'];
  else process.env['ALPACA_API_SECRET_KEY'] = secret;
}

before(async () => {
  server = createServer((req, res) => {
    const url = new URL(req.url ?? '/', 'http://localhost');
    requests.push({ path: url.pathname, query: url.searchParams, headers: req.headers });

    if (url.pathname !== '/news') {
      res.writeHead(404).end('nope');
      return;
    }

    // Alpaca answers a wrong key with 401 and a JSON message, not a page.
    if (req.headers['apca-api-key-id'] !== GOOD_KEY) {
      res
        .writeHead(401, { 'content-type': 'application/json' })
        .end(JSON.stringify({ message: 'access key verification failed' }));
      return;
    }

    res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(PAYLOAD));
  });

  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address() as AddressInfo;
  process.env['ALPACA_DATA_BASE'] = `http://127.0.0.1:${port}`;

  // Imported after the env var is set: the module reads the base at load time.
  alpaca = await import('../src/server/sources/alpaca.js');
});

after(() => {
  server.close();
  useCredentials(undefined, undefined);
});

beforeEach(() => {
  requests = [];
  useCredentials(GOOD_KEY, GOOD_SECRET);
});

describe('getNews against a fixture host', () => {
  it('sends the key pair as headers, never in the query string', async () => {
    await alpaca.getNews(['NVDA']);
    const [sent] = requests;
    assert.ok(sent, 'expected a request');
    assert.equal(sent.headers['apca-api-key-id'], GOOD_KEY);
    assert.equal(sent.headers['apca-api-secret-key'], GOOD_SECRET);
    // A secret in a query string ends up in every access log between here and
    // the origin.
    assert.equal(sent.query.get('key'), null);
    assert.ok(!sent.query.toString().includes(GOOD_SECRET));
  });

  it('overrides the two upstream defaults that would cost headlines', async () => {
    await alpaca.getNews(['AMD'], 40, 14);
    const [sent] = requests;
    assert.ok(sent);
    assert.equal(sent.query.get('symbols'), 'AMD');
    assert.equal(sent.query.get('limit'), '40');
    assert.equal(sent.query.get('sort'), 'desc');
    // Headline-only items are the fastest-moving ones on the wire.
    assert.equal(sent.query.get('exclude_contentless'), 'false');
    // The body is never rendered, so it is never requested.
    assert.equal(sent.query.get('include_content'), 'false');

    // `start` defaults to *today* upstream, which reads as "no news exists"
    // for any thinly-covered ticker before lunchtime.
    const start = Date.parse(sent.query.get('start') ?? '');
    const expected = Date.now() - 14 * 86_400_000;
    assert.ok(Math.abs(start - expected) < 60_000, `start was ${sent.query.get('start')}`);
  });

  it('asks for the whole wire when no symbol is given', async () => {
    await alpaca.getNews([]);
    assert.equal(requests[0]?.query.get('symbols'), null);
  });

  it('returns the normalised feed the panel renders', async () => {
    const feed = await alpaca.getNews(['MSFT']);
    assert.equal(feed.articles.length, 1);
    assert.deepEqual(feed.symbols, ['MSFT']);
    assert.equal(feed.days, 7);
    assert.equal(feed.articles[0]!.headline, "Nvidia Q3 Beat: Data Center Revenue Tops Street's View");
    assert.equal(feed.articles[0]!.summary, 'Shares rose in after-hours trade.');
    assert.equal(feed.articles[0]!.time, 1786739467);
  });

  it('clamps a limit the upstream would reject outright', async () => {
    await alpaca.getNews(['INTC'], 5000);
    assert.equal(requests[0]?.query.get('limit'), String(alpaca.MAX_LIMIT));
  });

  it('caches, so N polling panels make one upstream call', async () => {
    await alpaca.getNews(['COIN']);
    requests = [];
    await alpaca.getNews(['COIN']);
    assert.equal(requests.length, 0);
  });

  it('refuses a malformed symbol without making a request', () => {
    assert.throws(() => alpaca.assertSymbols('NVDA&limit=9999'), /not a symbol/);
    assert.equal(requests.length, 0);
  });
});

describe('getNews credential failures', () => {
  it('says the feed is unconfigured, and does not call out', async () => {
    useCredentials(undefined, undefined);
    await assert.rejects(
      alpaca.getNews(['GME']),
      (err: Error & { code?: string; hint?: string }) => {
        assert.equal(err.code, 'not_configured');
        // The hint is shown verbatim in the panel, so it has to name the fix.
        assert.match(err.hint ?? '', /ALPACA_API_KEY_ID/);
        return true;
      },
    );
    assert.equal(requests.length, 0);
  });

  it('treats half a key pair as no key pair', async () => {
    useCredentials(GOOD_KEY, undefined);
    await assert.rejects(alpaca.getNews(['RIVN']), (err: Error & { code?: string }) => {
      assert.equal(err.code, 'not_configured');
      return true;
    });
    assert.equal(requests.length, 0);
  });

  it('reads Alpaca’s SDK variable names when the prefixed ones are unset', async () => {
    useCredentials(undefined, undefined);
    process.env['APCA_API_KEY_ID'] = GOOD_KEY;
    process.env['APCA_API_SECRET_KEY'] = GOOD_SECRET;
    try {
      const feed = await alpaca.getNews(['PLTR']);
      assert.equal(feed.articles.length, 1);
    } finally {
      delete process.env['APCA_API_KEY_ID'];
      delete process.env['APCA_API_SECRET_KEY'];
    }
  });

  it('calls a rejected key a rejected key, not a blocked IP', async () => {
    useCredentials('WRONGKEY', GOOD_SECRET);
    await assert.rejects(
      alpaca.getNews(['TSLA']),
      (err: Error & { code?: string; hint?: string; status?: number }) => {
        // The generic 4xx hint sends someone hunting a network problem they do
        // not have; this one points at the key pair.
        assert.equal(err.code, 'bad_credentials');
        assert.equal(err.status, 401);
        assert.match(err.hint ?? '', /ALPACA_API_SECRET_KEY/);
        return true;
      },
    );
  });
});
