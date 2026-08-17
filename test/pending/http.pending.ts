/**
 * Suspected defects in the HTTP wrapper and the provider chain.
 *
 * These are NOT in the suite glob: each one asserts the behaviour the module
 * says it has, and each one currently fails against the behaviour it actually
 * has. Every case here was observed by running it against the same local
 * `node:http` fixture host the passing tests use.
 *
 *   1. `fetchText` retries a 400 and a 403 for the full retry budget, even
 *      though the catch block's own comment names 400 as "a definitive upstream
 *      answer … not worth retrying". Only 404 escapes, because 404 is the one
 *      4xx that gets a code other than `upstream_status`.
 *   2. A caller header spelled in a different case than the browser default —
 *      `user-agent` rather than `User-Agent`, both legal, HTTP header names
 *      being case-insensitive — is *appended* to the default rather than
 *      replacing it, and the host receives two user agents in one header.
 *   3. `firstAnswer` rebuilds the error when a skipped provider supplies a
 *      hint, and the rebuild copies only `message`, `code` and `hint` — the
 *      upstream `status` is dropped, and the API route reports it to clients.
 *   4. The browser header set does not reach the wire as a browser's: fetch
 *      owns `Sec-Fetch-Mode`, so the request goes out as `Sec-Fetch-Dest:
 *      document` with `Sec-Fetch-Mode: cors`, a combination no browser sends
 *      and a cheap thing for the bot protection to key on.
 */

import assert from 'node:assert/strict';
import { createServer, type Server } from 'node:http';
import { after, before, describe, it } from 'node:test';
import type { AddressInfo } from 'node:net';
import { UpstreamError, fetchText } from '../../src/server/lib/http.js';
import { firstAnswer } from '../../src/server/lib/providers.js';

let server: Server;
let base: string;
const hits = new Map<string, number>();

before(async () => {
  server = createServer((req, res) => {
    hits.set(req.url ?? '', (hits.get(req.url ?? '') ?? 0) + 1);
    const path = new URL(req.url ?? '/', 'http://fixture').pathname;
    if (path === '/echo-headers') {
      res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(req.headers));
      return;
    }
    if (path.startsWith('/status/')) {
      res
        .writeHead(Number(path.slice('/status/'.length)), { 'content-type': 'text/plain' })
        .end('the origin said no');
      return;
    }
    res.writeHead(404).end('no such fixture route');
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
});

after(() => {
  server.closeAllConnections();
  server.close();
});

async function caught(fn: () => Promise<unknown>): Promise<UpstreamError> {
  try {
    await fn();
  } catch (err) {
    return err as UpstreamError;
  }
  throw new assert.AssertionError({ message: 'expected the call to reject, but it resolved' });
}

describe('fetchText retry scope', () => {
  it('asks a 400 once — the request will not become well formed by being repeated', async () => {
    // Observed: 3 requests for `retries: 2`. The status branch throws an
    // UpstreamError coded `upstream_status`, and the catch block only rethrows
    // immediately when the code is *not* `upstream_status`, so every
    // non-retryable 4xx falls through to the backoff loop.
    hits.clear();
    await caught(() => fetchText(`${base}/status/400?case=pending`, { retries: 2 }));
    assert.equal(hits.get('/status/400?case=pending'), 1);
  });

  it('asks a 403 once, rather than hammering a host that is already blocking this IP', async () => {
    // Observed: 3 requests for `retries: 2`, spread over ~1.2s of backoff.
    // isRetryableStatus(403) is false, so the intent is plainly one attempt.
    hits.clear();
    await caught(() => fetchText(`${base}/status/403?case=pending`, { retries: 2 }));
    assert.equal(hits.get('/status/403?case=pending'), 1);
  });

  it('asks a 410 once', async () => {
    hits.clear();
    await caught(() => fetchText(`${base}/status/410?case=pending`, { retries: 2 }));
    assert.equal(hits.get('/status/410?case=pending'), 1);
  });
});

describe('fetchText header merging', () => {
  it('lets a caller replace a default header whatever case they spell it in', async () => {
    // Observed value on the wire:
    //   'Mozilla/5.0 (Macintosh; …) Chrome/127.0.0.0 Safari/537.36, prediction-terminal/1.0'
    // `{ ...BROWSER_HEADERS, ...headers }` only overrides on an exact key match,
    // so 'user-agent' and 'User-Agent' survive as two entries and Headers joins
    // them with ', '. A caller trying to identify itself instead ships both.
    const sent = JSON.parse(
      await fetchText(`${base}/echo-headers?case=lower`, {
        retries: 0,
        headers: { 'user-agent': 'prediction-terminal/1.0' },
      }),
    ) as Record<string, string>;
    assert.equal(sent['user-agent'], 'prediction-terminal/1.0');
  });

  it('lets a caller replace Accept in lower case', async () => {
    // Observed: 'text/html,application/xhtml+xml,…,*/*;q=0.8, text/csv'.
    // FRED's CSV endpoint is served by content negotiation, so an Accept with
    // text/html still in front of it is not a harmless extra.
    const sent = JSON.parse(
      await fetchText(`${base}/echo-headers?case=accept`, {
        retries: 0,
        headers: { accept: 'text/csv' },
      }),
    ) as Record<string, string>;
    assert.equal(sent['accept'], 'text/csv');
  });

  it('sends a Sec-Fetch triple a browser would actually send', async () => {
    // Observed: sec-fetch-mode arrives as 'cors'. fetch treats Sec-Fetch-Mode
    // as its own header and overwrites whatever BROWSER_HEADERS set, leaving
    // `Sec-Fetch-Dest: document` + `Sec-Fetch-User: ?1` + `Sec-Fetch-Mode: cors`
    // — a combination Chrome never emits, on the very requests these headers
    // exist to make look ordinary.
    const sent = JSON.parse(await fetchText(`${base}/echo-headers?case=secfetch`, { retries: 0 })) as Record<
      string,
      string
    >;
    assert.equal(sent['sec-fetch-mode'], 'navigate');
  });
});

describe('firstAnswer skipped-provider hint', () => {
  it('keeps the upstream status when it annotates the failure with a skipped hint', async () => {
    // Observed: status === undefined. The rebuilt UpstreamError copies message,
    // code and hint only. `src/server/index.ts` puts `err.status` in the JSON
    // error body, so the client is told a FRED scrape failed but not that the
    // host answered 503 — and the same annotation path is the *only* one FRED's
    // most common failure (blocked scrape, no API key) takes.
    const err = await caught(() =>
      firstAnswer(
        [
          {
            id: 'scrape',
            label: 'fred.stlouisfed.org',
            run: async () => {
              throw new UpstreamError('fred.stlouisfed.org refused the connection', {
                code: 'upstream_blocked',
                status: 503,
                hint: 'It resets datacentre IPs.',
              });
            },
          },
          {
            id: 'api',
            label: 'the FRED API',
            available: () => false,
            skippedHint: (cause) =>
              cause?.code === 'upstream_blocked' ? `${cause.hint} Set FRED_API_KEY.` : undefined,
            run: async () => ({}) as never,
          },
        ],
        { what: 'series UNRATE' },
      ),
    );
    assert.equal(err.code, 'upstream_blocked');
    assert.equal(err.hint, 'It resets datacentre IPs. Set FRED_API_KEY.');
    assert.equal(err.status, 503);
  });
});
