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
import { fredRouter } from './routes/fred.js';
import { impliedRouter } from './routes/implied.js';
import { kalshiRouter } from './routes/kalshi.js';
import { spotRouter } from './routes/spot.js';
import { warmCorpus } from './sources/kalshi.js';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PORT = Number(process.env.PORT ?? 8787);
const HOST = process.env.HOST ?? '127.0.0.1';

export function createApp(): express.Express {
  const app = express();
  app.disable('x-powered-by');
  app.set('trust proxy', true);

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

    // Opportunistic sweep so the map cannot grow without bound.
    if (hits.size > 5000) {
      for (const [k, v] of hits) if (v.resetAt <= now) hits.delete(k);
    }
    next();
  });

  // ---- routes ------------------------------------------------------------
  app.use('/api/kalshi', kalshiRouter);
  app.use('/api/spot', spotRouter);
  app.use('/api/implied', impliedRouter);
  app.use('/api/fred', fredRouter);
  app.use('/api/billboard', billboardRouter);

  app.get('/api/health', (_req, res) => {
    res.json({
      ok: true,
      uptimeSeconds: Math.round(process.uptime()),
      cache: cache.stats(),
      fredApiKey: Boolean(process.env.FRED_API_KEY?.trim()),
      time: new Date().toISOString(),
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
      // `bad_request` is the caller's fault; everything else is the upstream's.
      const status =
        err.code === 'bad_request'
          ? 400
          : err.code === 'not_found'
            ? 404
            : err.code === 'upstream_timeout'
              ? 504
              : 502;
      const body: ApiError = { error: err.message, code: err.code };
      if (err.hint) body.hint = err.hint;
      if (err.status) body.status = err.status;
      res.status(status).json(body);
      return;
    }

    console.error('[server] unhandled error', err);
    res.status(500).json({
      error: err instanceof Error ? err.message : 'Internal server error',
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
    console.log(`  spot       /api/spot/{stock,crypto}/:symbol[/candles]`);
    console.log(`  implied    /api/implied/{underlyings,candidates,series}`);
    console.log(`  fred       /api/fred/{series/:id,search}`);
    console.log(`  billboard  /api/billboard/{charts,chart/:slug}`);
    if (!process.env.FRED_API_KEY?.trim()) {
      console.log(`  note: FRED_API_KEY unset — FRED uses scraping only (no fallback).`);
    }
    // Crawl the Kalshi event universe in the background so the first `SRCH`
    // does not pay the ~15s cold-start cost.
    warmCorpus();
  });
}
