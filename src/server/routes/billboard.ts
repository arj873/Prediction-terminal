import { Router } from 'express';
import { UpstreamError, readCappedBytes } from '../lib/http.js';
import * as billboard from '../sources/billboard.js';
import { asyncRoute, pathParam } from './helpers.js';

export const billboardRouter: Router = Router();

/**
 * Hosts the artwork proxy will fetch from.
 *
 * Exact matches only. A suffix check would accept
 * `charts-static.billboard.com.evil.test`, turning this route into an
 * open SSRF relay — the whole point of the allowlist is that a caller cannot
 * choose the destination.
 */
const ART_HOSTS = new Set([
  'charts-static.billboard.com',
  'www.billboard.com',
  'billboard.com',
]);

const MAX_ART_BYTES = 3 * 1024 * 1024;

/**
 * Image types this route will hand back.
 *
 * An allowlist rather than an `image/` prefix test, because `image/svg+xml`
 * passes that test and an SVG carries script — served from this origin, under
 * this origin's cookies and storage. Chart artwork is photographic; none of
 * these five formats can execute.
 */
const ART_TYPES = new Set([
  'image/jpeg',
  'image/png',
  'image/webp',
  'image/gif',
  'image/avif',
]);

/**
 * Validate an artwork URL before the server will fetch it.
 *
 * Exported so the allowlist is covered by tests: this function is the only
 * thing standing between a query parameter and an outbound request, and the
 * failure mode (SSRF against link-local metadata, or localhost) is severe
 * enough to be worth asserting rather than assuming.
 */
export function assertArtUrl(raw: string): URL {
  let target: URL;
  try {
    target = new URL(raw);
  } catch {
    throw new UpstreamError('Artwork URL is not a valid URL', { code: 'bad_request' });
  }

  if (target.protocol !== 'https:' || !ART_HOSTS.has(target.hostname)) {
    throw new UpstreamError(`Refusing to fetch artwork from ${target.hostname}`, {
      code: 'bad_request',
      hint: 'Only Billboard chart artwork can be proxied.',
    });
  }
  return target;
}

/**
 * Proxy chart artwork.
 *
 * Billboard serves art from a CDN that rejects some referrers and is not
 * reachable from every network the terminal runs on. Routing it through the
 * server keeps the page on one origin and makes the column work everywhere the
 * API itself works.
 */
billboardRouter.get(
  '/art',
  asyncRoute(async (req, res) => {
    const target = assertArtUrl(typeof req.query['u'] === 'string' ? req.query['u'] : '');

    const upstream = await fetch(target, {
      headers: { Accept: 'image/*', Referer: 'https://www.billboard.com/' },
      signal: AbortSignal.timeout(15_000),
      // Not `follow`: a redirect would be resolved *after* the allowlist check,
      // so an allowlisted host could bounce this request to an internal
      // address. Billboard's CDN serves artwork directly, so refusing to follow
      // costs nothing and closes the hole.
      redirect: 'error',
    });

    if (!upstream.ok || !upstream.body) {
      throw new UpstreamError(`Artwork fetch returned HTTP ${upstream.status}`, {
        code: 'upstream_status',
        status: upstream.status,
      });
    }

    // `image/png; charset=binary` is still a PNG — match on the media type and
    // drop whatever parameters follow it.
    const type = (upstream.headers.get('content-type') ?? '').split(';')[0]!.trim().toLowerCase();
    if (!ART_TYPES.has(type)) {
      throw new UpstreamError('Artwork URL did not return an image', {
        code: 'bad_upstream_body',
        hint: `Chart artwork is served as ${[...ART_TYPES].join(', ')}.`,
      });
    }

    // Capped while streaming rather than after buffering: reading the whole
    // body and *then* measuring it has already performed the allocation the
    // limit is here to prevent.
    const buffer = await readCappedBytes(upstream, MAX_ART_BYTES, target.toString());

    res.setHeader('Content-Type', type);
    res.setHeader('Cache-Control', 'public, max-age=86400, immutable');
    res.send(buffer);
  }),
);

billboardRouter.get('/charts', (_req, res) => {
  res.json({ charts: billboard.KNOWN_CHARTS });
});

billboardRouter.get(
  '/chart/:slug',
  asyncRoute(async (req) =>
    billboard.getChart(
      pathParam(req.params, 'slug'),
      typeof req.query['date'] === 'string' && req.query['date'] !== ''
        ? req.query['date']
        : undefined,
    ),
  ),
);
