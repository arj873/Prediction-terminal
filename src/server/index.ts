/**
 * Prediction Terminal API server.
 *
 * Exists for three reasons the browser cannot handle on its own:
 *   1. CORS — none of the three upstreams allow cross-origin reads.
 *   2. Scraping — FRED and Billboard serve HTML that has to be parsed somewhere.
 *   3. Caching and request coalescing, so N polling panels make 1 upstream call.
 *
 * In production it also serves the built client, so the whole terminal is one
 * process on one port.
 */

import { existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import express, { type NextFunction, type Request, type Response } from 'express';
import type { ApiError } from '../shared/types.js';
import { cache } from './lib/cache.js';
import { UpstreamError } from './lib/http.js';
import { billboardRouter } from './routes/billboard.js';
import { crossVenueRouter } from './routes/crossvenue.js';
import { entertainmentRouter } from './routes/entertainment.js';
import { fredRouter } from './routes/fred.js';
import { impliedRouter } from './routes/implied.js';
import { kalshiRouter } from './routes/kalshi.js';
import { newsRouter } from './routes/news.js';
import { spotRouter } from './routes/spot.js';
import { venueRouter } from './routes/venue.js';
import { hasCredentials as hasAlpacaCredentials } from './sources/alpaca.js';
import { warmIndexes } from './sources/crossvenue.js';
import { warmAll } from './sources/venues.js';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PORT = Number(process.env.PORT ?? 8787);
const HOST = process.env.HOST ?? '127.0.0.1';

/**
 * How much of `X-Forwarded-For` to believe.
 *
 * Off by default, because `trust proxy: true` believes the header from *any*
 * peer — which makes `req.ip` whatever the client says it is, and the per-IP
 * rate limit below a formality: rotating the header let a single client take
 * twelve of twelve requests against a limit of five. An operator who really is
 * behind a proxy states so, and states how many hops.
 *
 * Accepts what Express accepts: a hop count (`1`), a preset (`loopback`), or a
 * comma-separated list of trusted addresses/subnets.
 */
function trustProxySetting(): boolean | number | string {
  const raw = process.env.TRUST_PROXY?.trim();
  if (!raw || raw === 'false' || raw === '0') return false;
  const hops = Number(raw);
  return Number.isInteger(hops) && hops > 0 ? hops : raw;
}

/**
 * Security headers, on everything.
 *
 * The client builds every node with `textContent` and refuses raw HTML by
 * design, so there is no injection point today — this is the layer that holds
 * if a future panel forgets. `frame-ancestors` is the one that fixes a present
 * fact rather than a hypothetical: without it the terminal is framable by
 * anyone.
 *
 * `style-src` needs `'unsafe-inline'` because the book-depth bars set their
 * width through a `style` attribute; nothing else here relaxes the default.
 */
const SECURITY_HEADERS: Record<string, string> = {
  'Content-Security-Policy': [
    "default-src 'self'",
    "base-uri 'none'",
    "object-src 'none'",
    "frame-ancestors 'none'",
    "form-action 'none'",
    "script-src 'self'",
    "style-src 'self' 'unsafe-inline'",
    "img-src 'self' data:",
    "connect-src 'self'",
  ].join('; '),
  'X-Content-Type-Options': 'nosniff',
  'X-Frame-Options': 'DENY',
  'Referrer-Policy': 'no-referrer',
};

export function createApp(): express.Express {
  const app = express();
  app.disable('x-powered-by');
  app.set('trust proxy', trustProxySetting());

  app.use((_req, res, next) => {
    for (const [header, value] of Object.entries(SECURITY_HEADERS)) res.setHeader(header, value);
    next();
  });

  // ---- lightweight request log ------------------------------------------
  app.use((req, res, next) => {
    const started = performance.now();
    res.on('finish', () => {
      if (!req.path.startsWith('/api')) return;
      const ms = (performance.now() - started).toFixed(0);
      console.log(`${res.statusCode} ${req.method} ${req.originalUrl} ${ms}ms`);
    });
    next();
  });

  // ---- crude per-IP rate limit ------------------------------------------
  // Not a security boundary — just a stop on a runaway client loop turning into
  // an outbound flood at Kalshi and Billboard.
  const hits = new Map<string, { count: number; resetAt: number }>();
  const WINDOW_MS = 60_000;
  const MAX_PER_WINDOW = Number(process.env.RATE_LIMIT ?? 600);

  /**
   * Ceiling on tracked clients, and the mark the sweep clears down to.
   *
   * Sweeping only *expired* buckets was not a bound at all: a client varying
   * its address leaves every bucket fresh, so nothing was ever collected, the
   * map grew for as long as the traffic lasted, and the O(n) scan then ran on
   * every subsequent request — 25,000 addresses cost 27% in latency and climbing.
   *
   * Clearing down to a low-water mark rather than to the ceiling is what makes
   * the scan amortise: it cannot run again until another tenth of the table has
   * refilled.
   */
  const MAX_CLIENTS = 20_000;
  const LOW_WATER = Math.floor(MAX_CLIENTS * 0.9);

  app.use('/api', (req, res, next) => {
    const key = req.ip ?? 'unknown';
    const now = Date.now();
    const bucket = hits.get(key);

    if (!bucket || bucket.resetAt <= now) {
      hits.set(key, { count: 1, resetAt: now + WINDOW_MS });
    } else if (++bucket.count > MAX_PER_WINDOW) {
      res.status(429).json({
        error: 'Too many requests',
        code: 'rate_limited',
        hint: `This client exceeded ${MAX_PER_WINDOW} API calls per minute.`,
      } satisfies ApiError);
      return;
    }

    if (hits.size > MAX_CLIENTS) {
      for (const [k, v] of hits) if (v.resetAt <= now) hits.delete(k);
      // Expiry alone cannot get us under the mark when every bucket is fresh.
      // Insertion order is first-seen order, so this drops the oldest windows —
      // the ones closest to expiring anyway.
      for (const k of hits.keys()) {
        if (hits.size <= LOW_WATER) break;
        hits.delete(k);
      }
    }
    next();
  });

  // ---- routes ------------------------------------------------------------
  app.use('/api/kalshi', kalshiRouter);
  app.use('/api/venue/:venue', venueRouter);
  app.use('/api/xv', crossVenueRouter);
  app.use('/api/spot', spotRouter);
  app.use('/api/implied', impliedRouter);
  app.use('/api/fred', fredRouter);
  app.use('/api/billboard', billboardRouter);
  app.use('/api/ent', entertainmentRouter);
  app.use('/api/news', newsRouter);

  /**
   * Liveness, and which feeds this deployment can serve.
   *
   * The capability flags stay public: they are how the client explains a dead
   * `NEWS` panel, and "this deployment holds a key" is a fact about the feature
   * set rather than a secret. Uptime and cache counters do not — hit, miss and
   * eviction totals are a read-out on the cache an attacker is trying to churn,
   * which is exactly the feedback loop not to hand out. `HEALTH_DETAIL=1`
   * restores them for an operator watching their own box.
   */
  const healthDetail = process.env.HEALTH_DETAIL?.trim() === '1';

  app.get('/api/health', (_req, res) => {
    res.json({
      ok: true,
      fredApiKey: Boolean(process.env.FRED_API_KEY?.trim()),
      alpacaKeys: hasAlpacaCredentials(),
      time: new Date().toISOString(),
      ...(healthDetail
        ? { uptimeSeconds: Math.round(process.uptime()), cache: cache.stats() }
        : {}),
    });
  });

  app.use('/api', (_req, res) => {
    res.status(404).json({ error: 'No such API route', code: 'not_found' } satisfies ApiError);
  });

  // ---- built client, when present ----------------------------------------
  // dist/server/server/index.js → ../../client
  const clientDir = path.resolve(HERE, '../../client');
  if (existsSync(clientDir)) {
    app.use(express.static(clientDir, { index: false, maxAge: '1h' }));
    app.get(/.*/, (_req, res) => {
      res.sendFile(path.join(clientDir, 'index.html'));
    });
  }

  // ---- error handler -----------------------------------------------------
  app.use((err: unknown, _req: Request, res: Response, _next: NextFunction) => {
    if (res.headersSent) return;

    if (err instanceof UpstreamError) {
      // `bad_request` is the caller's fault, `not_configured` is the operator's
      // — an unset key is this deployment not offering the feed, not a bad
      // gateway — and everything else is the upstream's.
      const status =
        err.code === 'bad_request'
          ? 400
          : err.code === 'not_found'
            ? 404
            : // The venue is reachable and the request is well formed; it simply
              // does not publish this. A 501 says that and nothing else.
              err.code === 'unsupported'
              ? 501
              : err.code === 'not_configured'
                ? 503
                : err.code === 'upstream_timeout'
                  ? 504
                  : 502;
      const body: ApiError = { error: err.message, code: err.code };
      if (err.hint) body.hint = err.hint;
      if (err.status) body.status = err.status;
      res.status(status).json(body);
      return;
    }

    // An `UpstreamError` message is written to be read by whoever typed the
    // command, and says only which host declined. Anything reaching here is by
    // definition unanticipated, so its message was never held to that — it can
    // carry a path, a stack frame, or an internal identifier. Log it in full;
    // answer with a fixed string.
    console.error('[server] unhandled error', err);
    res.status(500).json({
      error: 'Internal server error',
      code: 'internal_error',
    } satisfies ApiError);
  });

  return app;
}

// Only listen when run directly, so tests can import `createApp` freely.
const invokedDirectly =
  process.argv[1] !== undefined &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);

if (invokedDirectly) {
  createApp().listen(PORT, HOST, () => {
    console.log(`PREDICTION TERMINAL api  http://${HOST}:${PORT}`);
    console.log(`  kalshi     /api/kalshi/{markets,events,search,top,series}`);
    console.log(`  venues     /api/venue/{kalshi,polymarket,polymarket-us}/{markets,events,search,top}`);
    console.log(`  xvenue     /api/xv/{series,compare}`);
    console.log(`  spot       /api/spot/{stock,crypto}/:symbol[/candles]`);
    console.log(`  implied    /api/implied/{underlyings,candidates,series}`);
    console.log(`  fred       /api/fred/{series/:id,search}`);
    console.log(`  billboard  /api/billboard/{charts,chart/:slug}`);
    console.log(`  ent        /api/ent/{markets,rt,netflix,spotify,youtube,boxoffice,steam,tv}`);
    console.log(`  news       /api/news?symbols=&limit=&days=`);
    if (!process.env.FRED_API_KEY?.trim()) {
      console.log(`  note: FRED_API_KEY unset — FRED uses scraping only (no fallback).`);
    }
    if (!hasAlpacaCredentials()) {
      console.log(`  note: ALPACA_API_KEY_ID/SECRET unset — NEWS is unavailable until they are.`);
    }
    // Crawl all three catalogues in the background, then pair their series up,
    // so the first `SRCH` or `XV` does not pay the cold-start cost.
    warmAll();
    warmIndexes();
  });
}
