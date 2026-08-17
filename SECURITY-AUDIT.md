# Security audit — Prediction Terminal

**Date:** 2026-08-17
**Scope:** the whole tree at `claude/security-audit-ry08rx` (`main` is empty, so the
branch *is* the codebase) — 25.6k lines across the Express API server, the three
venue clients, the seven scraped feeds, and the browser client.
**Method:** manual review of every server module and the client's rendering path,
plus live exercise of the running app. Every finding rated Medium or above was
reproduced against real code rather than inferred from reading; the measurements
below are from this machine.

**Baseline hygiene, verified:**

| Check | Result |
| --- | --- |
| `npm audit` | 0 vulnerabilities, 221 dependencies |
| Secrets in working tree | none |
| Secrets in git history | none — only `.env.example` was ever committed, and it holds no live values |
| `npm run typecheck` | clean |
| `npm test` | 433/433 pass |

---

## Summary

| # | Severity | Finding | Location |
| --- | --- | --- | --- |
| 1 | **High** | Unauthenticated event-loop DoS via search query terms | `sources/corpus.ts:99` |
| 2 | **High** | Rate limiter bypassable by a client header; its own bookkeeping degrades the server | `index.ts:40`, `index.ts:56` |
| 3 | Medium | Shared cache evictable by unvalidated attacker-controlled key material | `lib/cache.ts` + call sites |
| 4 | Medium | No security response headers anywhere | `index.ts` |
| 5 | Medium | Artwork proxy buffers unbounded bodies and admits `image/svg+xml` | `routes/billboard.ts:84` |
| 6 | Low | `/api/health` unauthenticated, reports which credentials are configured | `index.ts:94` |
| 7 | Low | Generic error handler returns internal `err.message` verbatim | `index.ts:149` |
| 8 | Low | 16 MiB (Netflix: 96 MiB) responses buffered into a single JS string | `lib/http.ts:52`, `sources/netflix.ts:31` |
| 9 | Low | `..` survives ticker encoding and is normalised into a different upstream path | `sources/kalshi.ts:262` |
| 10 | Low | `/api/xv/series` runs the full cross-venue pairing per request, uncached | `sources/crossvenue.ts:389` |

Findings 1 and 2 compound: 2 removes the only brake on 1.

---

## 1. Unauthenticated event-loop DoS via search query terms — High

**Where:** `src/server/sources/corpus.ts:99-108`

`searchCorpus` walks every event in the venue snapshot and, for each query term,
compiles a fresh regex and tests it against that event's haystack:

```ts
for (const term of terms) {
  if (!haystack.includes(term)) { matchedAll = false; break; }
  score += 10;
  if (ticker.includes(term)) score += 12;
  if (title.startsWith(term)) score += 8;
  if (new RegExp(`\\b${escapeRegExp(term)}`).test(haystack)) score += 6;   // ← per event, per term
}
```

Nothing caps the term count or the query length. The `break` on a non-matching
term is the only guard, and it is defeated by repeating a term that matches
everything: `q=a a a a …`. The corpus is up to 12,000 events
(`CORPUS_MAX_PAGES = 60` × 200) and is warmed at boot, so the expensive path is
the *default* state, not a cold-start edge case.

Node runs one thread. While this loop runs, the whole terminal is frozen for
every connected user.

**Reachable from:** `/api/kalshi/search?q=`, and
`/api/venue/{kalshi,polymarket,polymarket-us}/search?q=` — all unauthenticated GETs.

**Measured**, against a synthetic 10,000-event corpus of realistic shape:

| Query terms | Query bytes | Event loop blocked |
| --- | --- | --- |
| 1 | 1 | 44 ms |
| 100 | 199 | 468 ms |
| 1,000 | 1,999 | 4,269 ms |
| 4,000 | 7,999 | **17,756 ms** |

Cost is linear in term count. Node's default 16 KB header limit allows roughly
8,000 terms in one URL, so a single GET request stalls the server for about
35 seconds. Repeat it and the terminal never serves anyone again.

**Fix:** cap query length and term count at the route boundary (a real search is
under a dozen words); hoist the per-term regex out of the per-event loop by
compiling each term once up front, or replace it with an index-based
word-boundary check on the `includes` hit that already ran.

**Reproduce:** call `searchCorpus(corpus, 'a '.repeat(4000), 25)` against a
10,000-event corpus and time it.

---

## 2. Rate limiter bypassable by a client header, and self-degrading — High

**Where:** `src/server/index.ts:40` and `src/server/index.ts:56-81`

```ts
app.set('trust proxy', true);        // line 40
...
const key = req.ip ?? 'unknown';     // line 61 — now attacker-chosen
```

`trust proxy: true` tells Express to believe `X-Forwarded-For` from *any* peer,
so `req.ip` becomes whatever the client says it is. Every request can claim a
fresh address, and the per-IP bucket never fills.

**Measured**, with `RATE_LIMIT=5`:

```
same client, no header:   4 allowed,  8 rate-limited
same client, rotating XFF: 12 allowed, 0 rate-limited
```

The comment on the limiter says it is "not a security boundary — just a stop on a
runaway client loop turning into an outbound flood". That framing is right about
intent, but the limiter does not achieve even that: the flood it is meant to stop
is exactly the case that sets the header.

There is a second-order problem in the same block. The sweep only deletes
*expired* entries, and only runs once `hits.size > 5000`:

```ts
if (hits.size > 5000) {
  for (const [k, v] of hits) if (v.resetAt <= now) hits.delete(k);
}
```

Under rotating addresses every entry is fresh, so nothing is ever collected — the
map grows without bound *and* the O(n) scan then runs on every subsequent
request.

**Measured**, 25,000 requests with distinct spoofed addresses:

| Distinct IPs seen | Time for that 5,000-request batch | Heap growth |
| --- | --- | --- |
| 5,000 | 8,862 ms | +20.7 MB |
| 10,000 | 9,381 ms | +28.0 MB |
| 15,000 | 9,837 ms | +49.2 MB |
| 20,000 | 10,544 ms | +39.2 MB |
| 25,000 | 11,287 ms | +43.2 MB |

Latency degrades monotonically and memory never comes back. The mechanism meant
to bound work is itself unbounded work.

**Fix:** make the proxy trust explicit rather than blanket — `trust proxy` should
name the hop count or the proxy's address, and default to off, so `req.ip` is the
socket address unless an operator states otherwise. Separately, give the sweep an
unconditional bound (evict oldest when over a hard cap, not only when expired),
so the map is bounded regardless of what the key space looks like.

---

## 3. Shared cache evictable by attacker-controlled key material — Medium

**Where:** `src/server/lib/cache.ts:22-56`, and the call sites below.

One global `TtlCache` with `maxEntries = 1000` and LRU eviction backs every
source. Keys embed raw user input:

| Key | Attacker-controlled component |
| --- | --- |
| `kalshi:${path}` | `cursor`, `status`, `tickers`, `event_ticker`, `series_ticker` |
| `fred:search:${q}:${limit}` | free text |
| `rt:search:${q}` | free text |
| `steam:search:${q}` | free text |
| `billboard:${chart}:${date}` | any `[a-z0-9-]{1,61}` slug × any valid date |
| `polymarket:token:${slug}`, `polymarket-us:${path}` | slug |

None of these components is length-bounded, and each distinct value is a new
entry. The values being evicted are not equal in cost: `kalshi:corpus` is a
~15-second, 60-page crawl.

**Measured:** seed the cache with a warm corpus, then write 1,200 entries
differing only in a `cursor` value:

```
warm corpus present before flood: true
warm corpus present after 1200 distinct cursors: false
cache stats: { hits: 0, misses: 1, entries: 1000, evictions: 201 }
```

1,200 cheap requests discard the expensive shared object, so the next `SRCH` from
any user pays the full cold-start crawl. Repeating the flood keeps everyone cold.

**Fix:** cap the length of user-derived key components, and keep the crawl
snapshots out of the same eviction pool as per-query entries — either a separate
cache instance for catalogue keys, or a pin/reserve for them.

---

## 4. No security response headers — Medium

**Where:** `src/server/index.ts`

Confirmed absent on every response, including the built client:
`Content-Security-Policy`, `X-Content-Type-Options`, `X-Frame-Options` /
`frame-ancestors`, `Referrer-Policy`. Observed headers on `/api/health` are only
`connection`, `content-length`, `content-type`, `date`, `etag`, `keep-alive`.

There is **no XSS sink in the client today** — see *Verified sound* below; the
rendering layer is genuinely disciplined about this. This is a defence-in-depth
gap, not a live hole. It matters because the page is framable as it stands, and
because the one thing standing between upstream text and the DOM is a convention
that a future panel has to keep choosing.

**Fix:** set a CSP (`default-src 'self'`, `frame-ancestors 'none'`), `nosniff`,
and a `Referrer-Policy` on all responses.

---

## 5. Artwork proxy: unbounded buffering, and `image/svg+xml` admitted — Medium

**Where:** `src/server/routes/billboard.ts:79-91`

Two issues in the response-handling half of an otherwise well-built route.

**The size cap is advisory.** The whole body is read into memory *before* the
limit is checked:

```ts
const buffer = Buffer.from(await upstream.arrayBuffer());   // unbounded read
if (buffer.byteLength > MAX_ART_BYTES) { throw ... }        // checked too late
```

`MAX_ART_BYTES` is 3 MiB but nothing stops the process buffering far more first.
`lib/http.ts`'s `readCapped` already does this correctly — check `content-length`
up front, then cap while streaming — and that pattern should be reused here.

**`image/svg+xml` passes the content-type check.** `type.startsWith('image/')`
admits SVG, which carries script, and the bytes are served back from the
terminal's own origin with the upstream's `Content-Type` and no `nosniff`. The
host allowlist makes this hard to reach — it would take attacker-controlled
content hosted under `billboard.com` — but the allowlist is doing all the work,
and the two-line fix does not depend on it.

**Fix:** check `content-length` and stream-cap before buffering; narrow the
allowed types to raster image types; send `X-Content-Type-Options: nosniff`.

**The SSRF control itself is sound and should be left alone.** I tried to break
it four ways and all four were correctly refused:

```
http://169.254.169.254/latest/meta-data/          → 400 Refusing to fetch artwork from 169.254.169.254
https://169.254.169.254/                          → 400 Refusing to fetch artwork from 169.254.169.254
https://charts-static.billboard.com.evil.test/…   → 400 Refusing to fetch artwork from charts-static.billboard.com.evil.test
https://billboard.com@evil.test/x.png             → 400 Refusing to fetch artwork from evil.test
```

Exact-match host set, `https:` only, and `redirect: 'error'` to stop an
allowlisted host bouncing the request inward. The comments explaining *why* each
of those is there are accurate.

---

## 6–10. Low severity

**6. `/api/health` is unauthenticated** (`index.ts:94-103`). It reports uptime,
cache statistics, and — usefully to an attacker — `fredApiKey` and `alpacaKeys`
booleans saying which credentials this deployment holds. Minor recon value; worth
gating if the server is ever exposed beyond localhost.

**7. Internal error messages are returned verbatim** (`index.ts:149-152`). For
anything that is not an `UpstreamError`, the handler sends
`err instanceof Error ? err.message : …` to the client. `UpstreamError` messages
are carefully host-only, but an unanticipated throw is not held to that. Log the
detail; return a fixed string.

**8. Large responses are buffered as single JS strings** (`lib/http.ts:52`,
`sources/netflix.ts:31`). The default ceiling is 16 MiB; Netflix's country feed
raises it to 96 MiB, which at UTF-16 is roughly 192 MB of heap for one request.
Fine for a trusted feed on a warm cache, but it is the largest single allocation
the process can be made to perform.

**9. `..` survives ticker encoding** (`sources/kalshi.ts:262`).
`encodeURIComponent` does not encode `.`, so a ticker of `..` produces
`…/trade-api/v2/markets/..`, which WHATWG URL normalisation collapses to a
different path. It stays on the same allowlisted host — `/` is encoded, so no
traversal beyond one segment — but path segments should be rejected by a ticker
regex rather than normalised away.

**10. `/api/xv/series` does its full pairing per request** (`crossvenue.ts:389`).
The venue indexes are cached, but the cross-product pairing, grouping and merging
run on every call, with the query applied only as a final `matchesQuery` filter.
The route takes no required argument. Cost is fixed rather than
attacker-amplifiable, so this is a scaling concern rather than a DoS — but it is
uncached work on the shared event loop.

---

## Verified sound

Stated explicitly so these are not re-examined on the next pass.

**The client has no XSS sink.** No `innerHTML`, `outerHTML`,
`insertAdjacentHTML`, `document.write`, `eval`, or `new Function` anywhere in
`src/client`. `lib/dom.ts` builds every node with `textContent` and
`createTextNode`, and actively throws on an `html:` attribute
(`el(): raw html is not supported by design`). This is the single best thing
about the codebase's security posture.

**Untrusted upstream URLs are scheme-filtered before they reach an anchor.**
`panels/news.ts:171` sets `href` from an Alpaca-supplied URL, but `safeUrl`
(`sources/alpaca.ts:163-171`) parses it and returns `''` for anything that is not
`http:`/`https:`, so a `javascript:` href cannot reach the DOM. The anchors also
carry `rel="noopener noreferrer"`.

**Outbound URL construction is consistently safe.** Every user-controlled value
that reaches an upstream URL is either validated against a strict allowlist regex
— FRED series ids, Billboard slugs and dates, box-office dates, TVmaze
date/country, Netflix category/scope, Steam app ids, Alpaca symbols, RT slugs,
stream-chart country codes — or passed through `encodeURIComponent` /
`URLSearchParams`. I found no path where a caller can choose the destination
host.

**No prototype-pollution reach.** `isVenue` and `sourceFor` go through a `Map`,
not object indexing, so `__proto__` and `constructor` cannot select a source
module. Persisted keybindings are regex-gated on read (`state.ts:46-58`), and
`readStorage` validates every field's type and bounds.

**Credentials are handled correctly.** Alpaca's key pair is sent only as request
headers, only to `data.alpaca.markets`, and is never echoed into a response body
or a log line; a 401/403 is explicitly re-explained as a credential problem
rather than an IP block. `FRED_API_KEY` does go into a query string, but every
error message derived from a failed fetch carries `hostOf(url)` only, never the
URL, so the key does not leak through error text or the provider-chain warning
log.

**Outbound HTTP is well-behaved.** `lib/http.ts` enforces a per-attempt timeout,
bounded jittered retries, a streaming size cap, and a 1 KiB bounded `peek` on
error bodies rather than draining them.

---

## Recommended order of work

1. Cap search query length and term count, and hoist the per-term regex out of
   the per-event loop (finding 1).
2. Make `trust proxy` explicit, and bound the rate-limiter map unconditionally
   (finding 2).
3. Add the security headers (finding 4) — smallest change on the list.
4. Partition the cache and bound user-derived key components (finding 3).
5. Stream-cap the artwork proxy and narrow its content types (finding 5).
6. The Low findings as convenient.
