/**
 * The outbound HTTP wrapper and the provider chain, exercised over real sockets.
 *
 * Everything this server scrapes goes through `fetchText`/`fetchJson`, so the
 * behaviours that matter are the ones the upstreams actually inflict: FRED
 * answering an HTML "page not found" where CSV was asked for, Yahoo returning
 * 429 to a shared datacentre IP, an egress proxy dressing up an origin reset as
 * a 503, a host that accepts the connection and then says nothing, and a body
 * with no content-length that keeps arriving. Those are all decided by status
 * codes and stream behaviour, not by a parser, so the fixture host is a real
 * `node:http` server rather than a stubbed `fetch` — a stub cannot get the
 * chunking, the redirect, or the socket timeout wrong in the same way.
 *
 * `firstAnswer` is the shared shape behind FRED (scrape, then the official API)
 * and equities (Yahoo, then Nasdaq). The cases here are the ones those two
 * call sites depend on: a `not_found` from any provider is the answer and must
 * not be re-asked of the next one; a provider with no API key is skipped rather
 * than failed; and the answering provider's id is reported rather than guessed.
 */

import assert from 'node:assert/strict';
import { createServer, type Server } from 'node:http';
import { after, before, describe, it } from 'node:test';
import type { AddressInfo } from 'node:net';
import { UpstreamError, fetchJson, fetchText, hostOf } from '../src/server/lib/http.js';
import { firstAnswer, type Provider } from '../src/server/lib/providers.js';

/* --------------------------------------------------------------- fixtures */

/** Trimmed `api.stlouisfed.org/fred/series/observations` body. */
const FRED_OBSERVATIONS = {
  realtime_start: '2026-08-17',
  observation_start: '2024-01-01',
  units: 'lin',
  count: 3,
  observations: [
    { realtime_start: '2026-08-17', date: '2024-01-01', value: '3.7' },
    { realtime_start: '2026-08-17', date: '2024-02-01', value: '3.9' },
    { realtime_start: '2026-08-17', date: '2024-03-01', value: '.' },
  ],
};

/** What FRED serves for an unknown id: a page, with a 200, where CSV was asked for. */
const NOT_FOUND_PAGE =
  '<!doctype html><html><head><title>Page not found | FRED</title></head>' +
  '<body><h1>Page not found</h1></body></html>';

/** Envoy's phrasing when the *origin* hung up rather than answering. */
const ENVOY_BODY = 'upstream connect error or disconnect/reset before headers. reset reason: connection termination';

let server: Server;
let base: string;
/** Requests received, keyed by the exact request line, so tests can count attempts. */
const hits = new Map<string, number>();

before(async () => {
  server = createServer((req, res) => {
    hits.set(req.url ?? '', (hits.get(req.url ?? '') ?? 0) + 1);
    const url = new URL(req.url ?? '/', 'http://fixture');
    const path = url.pathname;

    if (path === '/ok') {
      res.writeHead(200, { 'content-type': 'text/plain' }).end('ok');
      return;
    }
    if (path === '/echo-headers') {
      res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(req.headers));
      return;
    }
    if (path === '/envoy') {
      res.writeHead(503, { 'content-type': 'text/plain' }).end(ENVOY_BODY);
      return;
    }
    if (path.startsWith('/status/')) {
      res
        .writeHead(Number(path.slice('/status/'.length)), { 'content-type': 'text/plain' })
        .end('the origin said no');
      return;
    }
    if (path === '/flaky') {
      const fail = Number(url.searchParams.get('fail') ?? '0');
      const n = hits.get(req.url ?? '') ?? 1;
      if (n <= fail) {
        res.writeHead(503, { 'content-type': 'text/plain' }).end('origin unavailable');
        return;
      }
      res.writeHead(200, { 'content-type': 'text/plain' }).end('recovered');
      return;
    }
    if (path === '/sized') {
      const bytes = Number(url.searchParams.get('bytes') ?? '0');
      res
        .writeHead(200, { 'content-length': String(bytes), 'content-type': 'text/plain' })
        .end('x'.repeat(bytes));
      return;
    }
    if (path === '/chunked') {
      // No content-length: node frames this as chunked, which is how the real
      // scraped pages arrive, so only the streaming ceiling can stop it.
      res.writeHead(200, { 'content-type': 'text/html' });
      for (let i = 0; i < Number(url.searchParams.get('chunks') ?? '1'); i++) res.write('y'.repeat(512));
      res.end();
      return;
    }
    if (path === '/never') return; // accept the connection, answer nothing
    if (path === '/redirect') {
      res.writeHead(302, { location: '/ok' }).end();
      return;
    }
    if (path === '/no-content') {
      res.writeHead(204).end();
      return;
    }
    if (path === '/not-modified') {
      res.writeHead(304).end();
      return;
    }
    if (path === '/json/observations') {
      res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(FRED_OBSERVATIONS));
      return;
    }
    if (path === '/json/page') {
      res.writeHead(200, { 'content-type': 'text/html' }).end(NOT_FOUND_PAGE);
      return;
    }
    if (path === '/json/empty') {
      res.writeHead(200, { 'content-type': 'application/json' }).end('');
      return;
    }
    res.writeHead(404, { 'content-type': 'text/plain' }).end('no such fixture route');
  });

  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
});

after(() => {
  server.closeAllConnections();
  server.close();
});

/** Run `fn`, assert it rejected, and hand back the error for inspection. */
async function caught(fn: () => Promise<unknown>): Promise<UpstreamError> {
  try {
    await fn();
  } catch (err) {
    return err as UpstreamError;
  }
  throw new assert.AssertionError({ message: 'expected the call to reject, but it resolved' });
}

/* ----------------------------------------------------------------- hostOf */

describe('hostOf', () => {
  it('names the host, port and all, for the error text', () => {
    assert.equal(hostOf('https://api.nasdaq.com/api/quote/AAPL/info?assetclass=stocks'), 'api.nasdaq.com');
    assert.equal(hostOf('http://127.0.0.1:8080/graph/fredgraph.csv'), '127.0.0.1:8080');
  });

  it('hands back what it was given when that is not a url', () => {
    // The error message is the only consumer; it must not throw on the way out.
    assert.equal(hostOf('fredgraph.csv'), 'fredgraph.csv');
    assert.equal(hostOf(''), '');
  });
});

/* ---------------------------------------------------------------- headers */

describe('fetchText header merging', () => {
  it('sends the browser-shaped set the bot-protected hosts insist on', async () => {
    const sent = JSON.parse(await fetchText(`${base}/echo-headers`, { retries: 0 })) as Record<string, string>;
    assert.match(sent['user-agent']!, /^Mozilla\/5\.0 \(Macintosh;/);
    assert.equal(sent['sec-fetch-dest'], 'document');
    assert.equal(sent['sec-fetch-site'], 'none');
    assert.equal(sent['upgrade-insecure-requests'], '1');
    assert.equal(sent['pragma'], 'no-cache');
    assert.equal(sent['accept-language'], 'en-US,en;q=0.9');
    assert.match(sent['accept']!, /^text\/html,/);
  });

  it('lets a caller replace a default it spells the same way', async () => {
    const sent = JSON.parse(
      await fetchText(`${base}/echo-headers?case=exact`, {
        retries: 0,
        headers: { 'User-Agent': 'prediction-terminal/1.0' },
      }),
    ) as Record<string, string>;
    assert.equal(sent['user-agent'], 'prediction-terminal/1.0');
  });

  it('carries a caller header the defaults say nothing about', async () => {
    const sent = JSON.parse(
      await fetchText(`${base}/echo-headers?api-key=1`, {
        retries: 0,
        headers: { 'X-Api-Key': 'abc123' },
      }),
    ) as Record<string, string>;
    assert.equal(sent['x-api-key'], 'abc123');
    // …without losing the defaults it did not mention.
    assert.equal(sent['sec-fetch-site'], 'none');
  });

  it('does not mutate the caller-owned headers object', async () => {
    // Several call sites build one options object and reuse it per retry sweep.
    const headers = { 'X-Api-Key': 'abc123' };
    await fetchText(`${base}/echo-headers?nomutate=1`, { retries: 0, headers });
    assert.deepEqual(headers, { 'X-Api-Key': 'abc123' });
  });

  it('drops the page-shaped headers when asked for an API request', async () => {
    // Sec-Fetch-Dest: document on a JSON endpoint is a tell, and some APIs 400 on it.
    const sent = JSON.parse(
      await fetchText(`${base}/echo-headers?browser=off`, { retries: 0, browserHeaders: false }),
    ) as Record<string, string>;
    assert.equal(sent['sec-fetch-dest'], undefined);
    assert.equal(sent['upgrade-insecure-requests'], undefined);
    assert.equal(sent['cache-control'], undefined);
    // The user agent stays: it is the header the hosts actually check.
    assert.match(sent['user-agent']!, /^Mozilla\/5\.0/);
  });
});

/* ----------------------------------------------------------- status codes */

describe('fetchText status handling', () => {
  it('returns the body on a 200', async () => {
    assert.equal(await fetchText(`${base}/ok`, { retries: 0 }), 'ok');
  });

  it('reads a 404 as not_found, with the hint that points at the identifier', async () => {
    const err = await caught(() => fetchText(`${base}/status/404?case=code`, { retries: 2 }));
    assert.ok(err instanceof UpstreamError);
    assert.equal(err.code, 'not_found');
    assert.equal(err.status, 404);
    assert.match(err.hint!, /no such resource/);
  });

  it('asks a 404 exactly once, because a missing series stays missing', async () => {
    hits.clear();
    await caught(() => fetchText(`${base}/status/404?case=once`, { retries: 2 }));
    assert.equal(hits.get('/status/404?case=once'), 1);
  });

  it('names a 403 as a possible IP block rather than a bad identifier', async () => {
    const err = await caught(() => fetchText(`${base}/status/403`, { retries: 0 }));
    assert.equal(err.code, 'upstream_status');
    assert.equal(err.status, 403);
    assert.match(err.hint!, /blocking this IP/);
  });

  it('invents no hint for a status it has nothing to say about', async () => {
    const err = await caught(() => fetchText(`${base}/status/400`, { retries: 0 }));
    assert.equal(err.code, 'upstream_status');
    assert.equal(err.status, 400);
    assert.equal(err.hint, undefined);
  });

  it('reports a 429 as rate limiting, carrying the status for the caller to branch on', async () => {
    // stocks.ts reads `status === 429` to tell "Yahoo is throttling this IP"
    // apart from "your symbol is wrong", so the status must survive.
    const err = await caught(() => fetchText(`${base}/status/429`, { retries: 0 }));
    assert.equal(err.code, 'upstream_status');
    assert.equal(err.status, 429);
    assert.match(err.hint!, /rate-limiting/);
  });

  it('reads a 204 as an empty body rather than a failure', async () => {
    assert.equal(await fetchText(`${base}/no-content`, { retries: 0 }), '');
  });

  it('treats a 304 as a failed request, since nothing here sends conditional headers', async () => {
    // Documented so a caller adding If-None-Match knows it gets an error, not ''.
    const err = await caught(() => fetchText(`${base}/not-modified`, { retries: 0 }));
    assert.equal(err.code, 'upstream_status');
    assert.equal(err.status, 304);
  });

  it('follows a redirect to the body at the end of it', async () => {
    assert.equal(await fetchText(`${base}/redirect`, { retries: 0 }), 'ok');
  });
});

/* ---------------------------------------------------------------- retries */

describe('fetchText retries', () => {
  it('makes exactly one request when retries is zero', async () => {
    hits.clear();
    await caught(() => fetchText(`${base}/status/503?case=none`, { retries: 0 }));
    assert.equal(hits.get('/status/503?case=none'), 1);
  });

  it('retries a 503 and returns the body once the origin comes back', async () => {
    hits.clear();
    const body = await fetchText(`${base}/flaky?fail=2&k=recovers`, { retries: 3 });
    assert.equal(body, 'recovered');
    assert.equal(hits.get('/flaky?fail=2&k=recovers'), 3);
  });

  it('makes one attempt plus the configured retries, no more', async () => {
    hits.clear();
    await caught(() => fetchText(`${base}/status/429?case=count`, { retries: 1 }));
    assert.equal(hits.get('/status/429?case=count'), 2);
  });

  it('surfaces the last status once the retries are spent', async () => {
    const err = await caught(() => fetchText(`${base}/status/503?case=spent`, { retries: 1 }));
    assert.ok(err instanceof UpstreamError);
    assert.equal(err.code, 'upstream_status');
    assert.equal(err.status, 503);
    assert.match(err.hint!, /upstream outage/);
  });

  it('does not retry a 5xx whose body is a proxy reporting an origin reset', async () => {
    // Envoy's 503 means the origin hung up on the proxy. Retrying it just
    // repeats the block, and the remediation is a different network, not a wait.
    hits.clear();
    const err = await caught(() => fetchText(`${base}/envoy`, { retries: 2 }));
    assert.equal(hits.get('/envoy'), 1);
    assert.equal(err.code, 'upstream_blocked');
    assert.equal(err.status, 503);
    assert.match(err.hint!, /datacentre and cloud IPs/);
  });
});

/* -------------------------------------------------------------- size cap */

describe('fetchText response ceiling', () => {
  it('refuses a body whose declared length is over the ceiling', async () => {
    const err = await caught(() => fetchText(`${base}/sized?bytes=5000`, { retries: 0, maxBytes: 1024 }));
    assert.equal(err.code, 'response_too_large');
    assert.match(err.message, /exceeds 1024 bytes/);
  });

  it('accepts a body exactly at the ceiling', async () => {
    const body = await fetchText(`${base}/sized?bytes=1024`, { retries: 0, maxBytes: 1024 });
    assert.equal(body.length, 1024);
  });

  it('stops a chunked body that runs past the ceiling mid-stream', async () => {
    // No content-length to check, so only the running byte count can catch this.
    const err = await caught(() => fetchText(`${base}/chunked?chunks=10`, { retries: 0, maxBytes: 1024 }));
    assert.equal(err.code, 'response_too_large');
  });

  it('reads a chunked body that fits, whole', async () => {
    const body = await fetchText(`${base}/chunked?chunks=4`, { retries: 0, maxBytes: 4096 });
    assert.equal(body.length, 4 * 512);
  });

  it('does not retry an over-large response', async () => {
    hits.clear();
    await caught(() => fetchText(`${base}/sized?bytes=5000&k=noretry`, { retries: 2, maxBytes: 16 }));
    assert.equal(hits.get('/sized?bytes=5000&k=noretry'), 1);
  });

  it('decodes multi-byte characters across the streamed chunks', async () => {
    // The reader decodes incrementally; a naive per-chunk decode splits a
    // multi-byte codepoint and prints a replacement character in a headline.
    const body = await fetchText(`${base}/json/observations`, { retries: 0 });
    assert.equal(JSON.parse(body).count, 3);
  });
});

/* ------------------------------------------------------------- transport */

describe('fetchText transport failures', () => {
  it('calls a host that accepts and then says nothing a timeout', async () => {
    const err = await caught(() => fetchText(`${base}/never?case=timeout`, { retries: 0, timeoutMs: 150 }));
    assert.ok(err instanceof UpstreamError);
    assert.equal(err.code, 'upstream_timeout');
    assert.match(err.message, /did not respond in time/);
    assert.match(err.hint!, /never replied/);
  });

  it('applies the timeout per attempt, so a retry gets its own budget', async () => {
    hits.clear();
    await caught(() => fetchText(`${base}/never?case=perattempt`, { retries: 1, timeoutMs: 150 }));
    assert.equal(hits.get('/never?case=perattempt'), 2);
  });

  it('calls a refused connection a block, with the residential-IP hint', async () => {
    // Port 1 is closed: ECONNREFUSED reaches the wrapper as undici's "fetch failed".
    const err = await caught(() => fetchText('http://127.0.0.1:1/graph/fredgraph.csv', { retries: 0 }));
    assert.equal(err.code, 'upstream_blocked');
    assert.match(err.hint!, /bot protection/);
  });

  it('reports an unusable url as a failed request rather than crashing', async () => {
    const err = await caught(() => fetchText('fredgraph.csv', { retries: 0 }));
    assert.equal(err.code, 'upstream_error');
    assert.match(err.message, /failed:/);
  });

  it("lets a caller's own abort through as an abort, not as an upstream fault", async () => {
    // A cancelled request is the caller's decision; dressing it as an upstream
    // outage would put a 502 in the log for a client that just navigated away.
    const controller = new AbortController();
    controller.abort();
    const err = await caught(() => fetchText(`${base}/ok?case=abort`, { retries: 2, signal: controller.signal }));
    assert.ok(!(err instanceof UpstreamError));
    assert.equal((err as Error).name, 'AbortError');
  });
});

/* -------------------------------------------------------------- fetchJson */

describe('fetchJson', () => {
  it('parses a JSON body into the value the caller asked for', async () => {
    const body = await fetchJson<typeof FRED_OBSERVATIONS>(`${base}/json/observations`, { retries: 0 });
    assert.equal(body.count, 3);
    assert.deepEqual(body.observations[2], {
      realtime_start: '2026-08-17',
      date: '2024-03-01',
      value: '.',
    });
  });

  it('names a 200-with-HTML for what it is instead of throwing a SyntaxError', async () => {
    // This is FRED's actual behaviour for an unknown id, and the error a
    // caller sees must say "not JSON", not "Unexpected token < in JSON".
    const err = await caught(() => fetchJson(`${base}/json/page`, { retries: 0 }));
    assert.ok(err instanceof UpstreamError);
    assert.equal(err.code, 'bad_upstream_body');
    assert.match(err.message, /not valid JSON/);
  });

  it('reads an empty body as a bad body, not as an empty object', async () => {
    const err = await caught(() => fetchJson(`${base}/json/empty`, { retries: 0 }));
    assert.equal(err.code, 'bad_upstream_body');
  });

  it('sends an API Accept and leaves the page headers off', async () => {
    const sent = await fetchJson<Record<string, string>>(`${base}/echo-headers?json=1`, { retries: 0 });
    assert.equal(sent['accept'], 'application/json');
    assert.equal(sent['sec-fetch-dest'], undefined);
  });

  it('lets a caller override the Accept for an API that wants something else', async () => {
    const sent = await fetchJson<Record<string, string>>(`${base}/echo-headers?json=2`, {
      retries: 0,
      headers: { Accept: 'application/vnd.api+json' },
    });
    assert.equal(sent['accept'], 'application/vnd.api+json');
  });

  it('passes a status failure straight through, still coded', async () => {
    const err = await caught(() => fetchJson(`${base}/status/404?case=json`, { retries: 0 }));
    assert.equal(err.code, 'not_found');
    assert.equal(err.status, 404);
  });
});

/* -------------------------------------------------------- provider chains */

/** A provider that records when it ran, so ordering can be asserted. */
function recording<T>(
  id: string,
  order: string[],
  outcome: T | Error,
  extra: Partial<Provider<T>> = {},
): Provider<T> {
  return {
    id,
    label: id.toUpperCase(),
    run: async () => {
      order.push(id);
      if (outcome instanceof Error) throw outcome;
      return outcome;
    },
    ...extra,
  };
}

describe('firstAnswer', () => {
  it('returns the first provider answer, attributed to it', async () => {
    const order: string[] = [];
    const result = await firstAnswer(
      [recording('yahoo', order, 'quote'), recording('nasdaq', order, 'other')],
      { what: 'a price for AAPL' },
    );
    assert.deepEqual(result, { value: 'quote', source: 'yahoo' });
    assert.deepEqual(order, ['yahoo'], 'the fallback must not be asked once the first answered');
  });

  it('falls through to the next provider, and says which one answered', async () => {
    const order: string[] = [];
    const result = await firstAnswer(
      [
        recording('yahoo', order, new UpstreamError('Yahoo returned HTTP 429', { code: 'upstream_status', status: 429 })),
        recording('nasdaq', order, 'quote'),
      ],
      { what: 'a price for AAPL' },
    );
    assert.deepEqual(result, { value: 'quote', source: 'nasdaq' });
    assert.deepEqual(order, ['yahoo', 'nasdaq']);
  });

  it('runs providers in the order given, one after the other', async () => {
    const order: string[] = [];
    await firstAnswer(
      [
        recording('a', order, new UpstreamError('a', { code: 'upstream_error' })),
        recording('b', order, new UpstreamError('b', { code: 'upstream_error' })),
        recording('c', order, 'ok'),
      ],
      { what: 'x' },
    );
    assert.deepEqual(order, ['a', 'b', 'c']);
  });

  it('skips a provider this deployment cannot use, without running it', async () => {
    // The FRED API arm only exists when FRED_API_KEY is set.
    const order: string[] = [];
    const result = await firstAnswer(
      [
        recording('api', order, 'from api', { available: () => false }),
        recording('scrape', order, 'from scrape'),
      ],
      { what: 'series UNRATE' },
    );
    assert.deepEqual(result, { value: 'from scrape', source: 'scrape' });
    assert.deepEqual(order, ['scrape']);
  });

  it('stops on a not_found instead of asking the next source the same question', async () => {
    // No fallback can turn "no such series" into a series, and asking doubles
    // the latency of every typo a user makes.
    const order: string[] = [];
    const notFound = new UpstreamError('FRED has no series NOSUCH', { code: 'not_found', status: 404 });
    const err = await caught(() =>
      firstAnswer([recording('scrape', order, notFound), recording('api', order, 'from api')], {
        what: 'series NOSUCH',
      }),
    );
    assert.equal(err, notFound, 'the definitive error propagates untouched');
    assert.deepEqual(order, ['scrape']);
  });

  it('honours a caller-supplied terminal code list in place of the default', async () => {
    const order: string[] = [];
    const badRequest = new UpstreamError('start date is after end date', { code: 'bad_request' });
    const err = await caught(() =>
      firstAnswer([recording('a', order, badRequest), recording('b', order, 'ok')], {
        what: 'x',
        terminalCodes: ['bad_request'],
      }),
    );
    assert.equal(err, badRequest);
    assert.deepEqual(order, ['a']);
    // …and with that list in force, not_found is no longer terminal.
    const order2: string[] = [];
    const result = await firstAnswer(
      [
        recording('a', order2, new UpstreamError('missing', { code: 'not_found' })),
        recording('b', order2, 'ok'),
      ],
      { what: 'x', terminalCodes: ['bad_request'] },
    );
    assert.equal(result.source, 'b');
  });

  it('surfaces an unsupported capability as itself, not as a generic outage', async () => {
    // 501 "the venue does not publish this" is a fact about the provider; the
    // route maps it to a different HTTP status than a failed fetch.
    const order: string[] = [];
    const unsupported = new UpstreamError('Polymarket does not publish candles here', { code: 'unsupported' });
    const err = await caught(() =>
      firstAnswer(
        [
          recording('a', order, new UpstreamError('a fell over', { code: 'upstream_error' })),
          recording('b', order, unsupported),
        ],
        { what: 'x' },
      ),
    );
    assert.equal(err, unsupported);
  });

  it('raises not_configured, listing the providers, when every one is unavailable', async () => {
    const order: string[] = [];
    const err = await caught(() =>
      firstAnswer(
        [
          recording('api', order, 'x', { available: () => false }),
          recording('scrape', order, 'y', { available: () => false }),
        ],
        { what: 'series UNRATE' },
      ),
    );
    assert.equal(err.code, 'not_configured');
    assert.match(err.message, /No source is configured to serve series UNRATE/);
    assert.match(err.hint!, /API, SCRAPE/);
    assert.deepEqual(order, []);
  });

  it('raises the first provider failure when nothing answered', async () => {
    // The first provider is the source of record; its error says why it declined.
    const order: string[] = [];
    const first = new UpstreamError('fred.stlouisfed.org refused the connection', {
      code: 'upstream_blocked',
      status: 503,
    });
    const err = await caught(() =>
      firstAnswer(
        [
          recording('scrape', order, first),
          recording('api', order, new UpstreamError('the FRED API returned HTTP 500', { code: 'upstream_status' })),
        ],
        { what: 'series UNRATE' },
      ),
    );
    assert.equal(err, first);
  });

  it('prefers a caller-built exhausted error over the first failure', async () => {
    const order: string[] = [];
    const err = await caught(() =>
      firstAnswer(
        [
          recording('yahoo', order, new UpstreamError('429', { code: 'upstream_status', status: 429 })),
          recording('nasdaq', order, new UpstreamError('404', { code: 'upstream_status', status: 404 })),
        ],
        {
          what: 'a price for ZZZZ',
          onExhausted: (failures) => {
            assert.deepEqual(failures.map((f) => f.id), ['yahoo', 'nasdaq']);
            assert.deepEqual(failures.map((f) => f.label), ['YAHOO', 'NASDAQ']);
            return new UpstreamError('No price source could quote ZZZZ', { code: 'upstream_error' });
          },
        },
      ),
    );
    assert.equal(err.message, 'No price source could quote ZZZZ');
  });

  it('annotates the failure with the hint from a provider that was skipped', async () => {
    // FRED's case exactly: the scrape was blocked and the API arm could have
    // covered it, so the operator is told the key would have helped.
    const order: string[] = [];
    const err = await caught(() =>
      firstAnswer(
        [
          recording(
            'scrape',
            order,
            new UpstreamError('fred.stlouisfed.org refused the connection', {
              code: 'upstream_blocked',
              hint: 'It resets datacentre IPs.',
            }),
          ),
          recording('api', order, 'unused', {
            available: () => false,
            skippedHint: (cause) =>
              cause?.code === 'upstream_blocked' ? `${cause.hint} Set FRED_API_KEY to use the official API.` : undefined,
          }),
        ],
        { what: 'series UNRATE' },
      ),
    );
    assert.equal(err.code, 'upstream_blocked');
    assert.equal(err.message, 'fred.stlouisfed.org refused the connection');
    assert.equal(err.hint, 'It resets datacentre IPs. Set FRED_API_KEY to use the official API.');
  });

  it('leaves the failure alone when the skipped provider declines to comment', async () => {
    const order: string[] = [];
    const first = new UpstreamError('Yahoo returned HTTP 500', { code: 'upstream_status', status: 500 });
    const err = await caught(() =>
      firstAnswer(
        [
          recording('scrape', order, first),
          recording('api', order, 'unused', {
            available: () => false,
            // Only a *block* is worth blaming on the missing key.
            skippedHint: (cause) => (cause?.code === 'upstream_blocked' ? 'set the key' : undefined),
          }),
        ],
        { what: 'series UNRATE' },
      ),
    );
    assert.equal(err, first);
    assert.equal(err.status, 500);
  });

  it('passes a failure that is not an UpstreamError through unchanged', async () => {
    // A parser blowing up inside a provider must not be relabelled as an outage.
    const order: string[] = [];
    const boom = new TypeError("Cannot read properties of undefined (reading 'chart')");
    const err = await caught(() =>
      firstAnswer([recording('yahoo', order, boom), recording('nasdaq', order, new TypeError('also broken'))], {
        what: 'a price for AAPL',
      }),
    );
    assert.equal(err, boom);
  });

  it('logs once, under the given prefix, when more than one provider failed', async () => {
    const order: string[] = [];
    const lines: unknown[][] = [];
    const original = console.warn;
    console.warn = (...args: unknown[]) => void lines.push(args);
    try {
      await caught(() =>
        firstAnswer(
          [
            recording('yahoo', order, new UpstreamError('a', { code: 'upstream_error' })),
            recording('nasdaq', order, new UpstreamError('b', { code: 'upstream_error' })),
          ],
          { what: 'a price for AAPL', logPrefix: 'stocks' },
        ),
      );
    } finally {
      console.warn = original;
    }
    assert.equal(lines.length, 1);
    assert.match(String(lines[0]![0]), /^\[stocks\] every provider failed for a price for AAPL$/);
    assert.deepEqual(Object.keys(lines[0]![1] as object), ['yahoo', 'nasdaq']);
  });

  it('stays quiet when only one provider failed', async () => {
    const order: string[] = [];
    const lines: unknown[][] = [];
    const original = console.warn;
    console.warn = (...args: unknown[]) => void lines.push(args);
    try {
      await caught(() =>
        firstAnswer([recording('yahoo', order, new UpstreamError('a', { code: 'upstream_error' }))], {
          what: 'a price for AAPL',
          logPrefix: 'stocks',
        }),
      );
    } finally {
      console.warn = original;
    }
    assert.deepEqual(lines, []);
  });
});
