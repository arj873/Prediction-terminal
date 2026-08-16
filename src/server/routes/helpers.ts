import type { NextFunction, Request, RequestHandler, Response } from 'express';

/**
 * Wrap an async handler that *returns* its payload.
 *
 * Express 5 forwards rejected promises to the error middleware on its own, but
 * handlers still have to remember to `res.json(...)`. Returning the value
 * instead keeps every route to a single expression and makes it impossible to
 * forget the response.
 */
export function asyncRoute<T>(handler: (req: Request, res: Response) => Promise<T>): RequestHandler {
  return (req: Request, res: Response, next: NextFunction) => {
    handler(req, res)
      .then((payload) => {
        if (res.headersSent) return;
        res.json(payload);
      })
      .catch(next);
  };
}

/**
 * Read a path parameter as a single string.
 *
 * Express 5 types `req.params[x]` as `string | string[]` because wildcard
 * segments can repeat. None of our routes use wildcards, so collapsing to the
 * first value is always right — and beats sprinkling casts at every call site.
 */
export function pathParam(params: Record<string, string | string[]>, name: string): string {
  const value = params[name];
  return Array.isArray(value) ? (value[0] ?? '') : (value ?? '');
}

/** Coerce a query parameter to an integer, clamped and with a default. */
export function intParam(
  raw: unknown,
  fallback: number,
  min: number,
  max: number,
): number {
  if (raw === undefined || raw === null || raw === '') return fallback;
  const value = Number(Array.isArray(raw) ? raw[0] : raw);
  if (!Number.isFinite(value)) return fallback;
  return Math.min(Math.max(Math.trunc(value), min), max);
}
