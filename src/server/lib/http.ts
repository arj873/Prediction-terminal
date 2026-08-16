/**
 * Outbound HTTP with the things every scraper needs and none of the things it
 * doesn't: a browser-shaped header set, a hard timeout, bounded retries with
 * jittered backoff, and a response-size ceiling so a pathological upstream
 * can't exhaust memory.
 */

export class UpstreamError extends Error {
  readonly code: string;
  readonly status: number | undefined;
  readonly hint: string | undefined;

  constructor(message: string, opts: { code: string; status?: number; hint?: string }) {
    super(message);
    this.name = 'UpstreamError';
    this.code = opts.code;
    this.status = opts.status;
    this.hint = opts.hint;
  }
}

const DESKTOP_UA =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36';

/** Headers a real Chrome sends. Several of these hosts 403 without them. */
const BROWSER_HEADERS: Record<string, string> = {
  'User-Agent': DESKTOP_UA,
  Accept: 'text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8',
  'Accept-Language': 'en-US,en;q=0.9',
  'Cache-Control': 'no-cache',
  Pragma: 'no-cache',
  'Sec-Fetch-Dest': 'document',
  'Sec-Fetch-Mode': 'navigate',
  'Sec-Fetch-Site': 'none',
  'Sec-Fetch-User': '?1',
  'Upgrade-Insecure-Requests': '1',
};

export interface FetchOptions {
  headers?: Record<string, string>;
  /** Per-attempt timeout. Default 20s. */
  timeoutMs?: number;
  /** Additional attempts after the first. Default 2. */
  retries?: number;
  /** Send browser-shaped headers. Default true. */
  browserHeaders?: boolean;
  /** Reject bodies larger than this. Default 16 MiB. */
  maxBytes?: number;
  signal?: AbortSignal;
}

const DEFAULT_MAX_BYTES = 16 * 1024 * 1024;

/** Statuses where trying again might plausibly help. */
function isRetryableStatus(status: number): boolean {
  return status === 408 || status === 425 || status === 429 || (status >= 500 && status <= 599);
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function readCapped(res: Response, maxBytes: number, url: string): Promise<string> {
  const declared = Number(res.headers.get('content-length') ?? '0');
  if (declared > maxBytes) {
    throw new UpstreamError(`Response from ${hostOf(url)} exceeds ${maxBytes} bytes`, {
      code: 'response_too_large',
      status: res.status,
    });
  }
  if (!res.body) return '';

  const decoder = new TextDecoder('utf-8');
  const reader = res.body.getReader();
  let total = 0;
  let out = '';
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > maxBytes) {
      await reader.cancel().catch(() => {});
      throw new UpstreamError(`Response from ${hostOf(url)} exceeds ${maxBytes} bytes`, {
        code: 'response_too_large',
        status: res.status,
      });
    }
    out += decoder.decode(value, { stream: true });
  }
  out += decoder.decode();
  return out;
}

export function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/**
 * Fetch `url` as text, retrying transient failures.
 *
 * Throws {@link UpstreamError} on a non-retryable status, on exhausting
 * retries, or on a transport failure. A connection reset — how bot-protected
 * hosts usually say no to a datacentre IP — surfaces as `upstream_blocked`
 * with a hint, because that is the failure operators actually hit.
 */
export async function fetchText(url: string, options: FetchOptions = {}): Promise<string> {
  const {
    timeoutMs = 20_000,
    retries = 2,
    browserHeaders = true,
    maxBytes = DEFAULT_MAX_BYTES,
    headers = {},
    signal,
  } = options;

  const merged: Record<string, string> = browserHeaders
    ? { ...BROWSER_HEADERS, ...headers }
    : { 'User-Agent': DESKTOP_UA, ...headers };

  let lastError: unknown;

  for (let attempt = 0; attempt <= retries; attempt++) {
    if (attempt > 0) {
      // 400ms, 800ms, 1600ms … plus jitter, so parallel panels don't sync up.
      await sleep(400 * 2 ** (attempt - 1) + Math.floor(Math.random() * 250));
    }

    const timeout = AbortSignal.timeout(timeoutMs);
    const composite = signal ? AbortSignal.any([signal, timeout]) : timeout;

    try {
      const res = await fetch(url, { headers: merged, redirect: 'follow', signal: composite });

      if (!res.ok) {
        // A 502/503 can be the origin being down *or* an egress proxy reporting
        // that the origin hung up on it. The body distinguishes them, and the
        // remediation is completely different, so read it before deciding.
        const snippet = res.status >= 500 ? await peek(res) : '';
        const blocked = res.status >= 500 && CONNECT_FAILURE.test(snippet);

        if (isRetryableStatus(res.status) && !blocked && attempt < retries) {
          lastError = new UpstreamError(`${hostOf(url)} returned ${res.status}`, {
            code: 'upstream_status',
            status: res.status,
          });
          continue;
        }

        if (blocked) {
          throw new UpstreamError(`${hostOf(url)} refused the connection`, {
            code: 'upstream_blocked',
            status: res.status,
            hint: blockedHint(url),
          });
        }

        throw new UpstreamError(`${hostOf(url)} returned HTTP ${res.status}`, {
          code: res.status === 404 ? 'not_found' : 'upstream_status',
          status: res.status,
          hint: describeStatus(res.status, url),
        });
      }

      return await readCapped(res, maxBytes, url);
    } catch (err) {
      lastError = err;
      // A definitive upstream answer (404, 400) is not worth retrying.
      if (err instanceof UpstreamError && err.code !== 'upstream_status') throw err;
      if (signal?.aborted) throw err;
      if (attempt === retries) break;
    }
  }

  throw normaliseTransportError(lastError, url);
}

/** JSON convenience wrapper. Sends an API-shaped Accept instead of a page one. */
export async function fetchJson<T>(url: string, options: FetchOptions = {}): Promise<T> {
  const text = await fetchText(url, {
    ...options,
    browserHeaders: false,
    headers: { Accept: 'application/json', ...(options.headers ?? {}) },
  });
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new UpstreamError(`${hostOf(url)} returned a body that is not valid JSON`, {
      code: 'bad_upstream_body',
    });
  }
}

/**
 * Body text an egress proxy emits when the *origin* dropped the connection,
 * rather than answering. Envoy and friends phrase it as "upstream connect
 * error or disconnect/reset before headers".
 */
const CONNECT_FAILURE =
  /upstream connect error|disconnect\/reset before headers|connection (?:reset|refused)|no healthy upstream|remote reset/i;

/** Read at most 1 KiB of an error body without draining a huge response. */
async function peek(res: Response): Promise<string> {
  try {
    const reader = res.body?.getReader();
    if (!reader) return '';
    const { value } = await reader.read();
    await reader.cancel().catch(() => {});
    return value ? new TextDecoder().decode(value.slice(0, 1024)) : '';
  } catch {
    return '';
  }
}

function blockedHint(url: string): string {
  const host = hostOf(url);
  return (
    `${host} reset the connection before sending a response. Hosts behind bot ` +
    `protection commonly do this to datacentre and cloud IPs — the same request ` +
    `usually succeeds from a residential connection.`
  );
}

function describeStatus(status: number, url: string): string | undefined {
  const host = hostOf(url);
  if (status === 403) return `${host} refused the request (403) — it may be blocking this IP.`;
  if (status === 429) return `${host} is rate-limiting this IP. Wait a moment and retry.`;
  if (status === 404) return `${host} has no such resource. Check the identifier.`;
  if (status >= 500) return `${host} is returning ${status}. This is an upstream outage.`;
  return undefined;
}

function normaliseTransportError(err: unknown, url: string): UpstreamError {
  if (err instanceof UpstreamError) return err;
  const host = hostOf(url);
  const message = err instanceof Error ? err.message : String(err);

  if (/timed?\s*out|timeout/i.test(message)) {
    return new UpstreamError(`${host} did not respond in time`, {
      code: 'upstream_timeout',
      hint: `${host} accepted the connection but never replied. It may be throttling this IP.`,
    });
  }

  // ECONNRESET / EPIPE / "socket hang up" from a host that dislikes datacentre
  // egress. Worth calling out precisely: it is not a bug in the parser.
  if (/reset|hang up|EPIPE|ECONNREFUSED|fetch failed|socket/i.test(message)) {
    return new UpstreamError(`${host} refused the connection`, {
      code: 'upstream_blocked',
      hint: blockedHint(url),
    });
  }

  return new UpstreamError(`Request to ${host} failed: ${message}`, { code: 'upstream_error' });
}
