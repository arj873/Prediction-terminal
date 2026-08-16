/**
 * Market news — from Alpaca's news API.
 *
 * The one feed here that needs a credential, for the simple reason that no
 * unauthenticated equity news feed exists that is legal to read and stable
 * enough to parse. Alpaca resells Benzinga's wire as JSON at
 * `data.alpaca.markets/v1beta1/news`, keyed to an Alpaca account; a free paper
 * account is enough, and the key never leaves this process — it is sent as a
 * request header to one host and is not echoed into any response.
 *
 * Two upstream defaults are deliberately overridden:
 *
 *  - **`start` defaults to the beginning of the current day.** For the whole
 *    tape that is fine; for `NEWS NVDA` it means a quiet morning, a weekend, or
 *    a thinly-covered ticker answers `200 OK` with an empty list, which reads
 *    as "no news exists" rather than "none today". The window is set
 *    explicitly, and the panel says how wide it is.
 *  - **`exclude_contentless` stays off.** A headline with no article body is
 *    still the information — halt notices and 8-K flags arrive that way — so
 *    dropping them would quietly hide the fastest-moving items on the wire.
 *
 * `include_content` stays off in the other direction: the article body is never
 * rendered and asking for it multiplies the payload for nothing.
 */

import * as cheerio from 'cheerio';
import type { NewsArticle, NewsFeed } from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';

/** Overridable so the fetch path can be pointed at a fixture server. */
const BASE = process.env.ALPACA_DATA_BASE ?? 'https://data.alpaca.markets/v1beta1';

/** Alpaca caps a page at 50 articles; asking for more is a 400, not a longer page. */
export const MAX_LIMIT = 50;
export const MAX_SYMBOLS = 20;
export const MAX_DAYS = 90;
export const DEFAULT_DAYS = 7;

/* ------------------------------------------------------------ credentials */

/**
 * Alpaca's own SDKs read `APCA_API_KEY_ID` / `APCA_API_SECRET_KEY`, so anyone
 * who already uses Alpaca has those exported. The `ALPACA_`-prefixed names are
 * what this terminal documents; both are accepted, prefixed names first.
 */
function env(primary: string, fallback: string): string {
  return (process.env[primary] ?? process.env[fallback] ?? '').trim();
}

export function hasCredentials(): boolean {
  return (
    env('ALPACA_API_KEY_ID', 'APCA_API_KEY_ID') !== '' &&
    env('ALPACA_API_SECRET_KEY', 'APCA_API_SECRET_KEY') !== ''
  );
}

const SETUP_HINT =
  'Set ALPACA_API_KEY_ID and ALPACA_API_SECRET_KEY and restart the server. Keys are ' +
  'free from alpaca.markets — a paper-trading account issues a pair, and this feed ' +
  'only ever reads news with them.';

function credentials(): { keyId: string; secret: string } {
  const keyId = env('ALPACA_API_KEY_ID', 'APCA_API_KEY_ID');
  const secret = env('ALPACA_API_SECRET_KEY', 'APCA_API_SECRET_KEY');

  if (!keyId || !secret) {
    throw new UpstreamError('The news feed needs Alpaca API credentials', {
      code: 'not_configured',
      hint: SETUP_HINT,
    });
  }
  return { keyId, secret };
}

/* -------------------------------------------------------------- arguments */

/**
 * Symbols Alpaca will filter news by.
 *
 * Equities can carry a dot (`BRK.B`) and crypto arrives as a pair (`BTCUSD`,
 * `BTC/USD`), so this is looser than a Kalshi ticker — but it is still a
 * whitelist, because these tokens are interpolated into an outbound query
 * string.
 */
const SYMBOL = /^[A-Z][A-Z0-9.\-/]{0,14}$/;

export function assertSymbols(raw: string): string[] {
  const out: string[] = [];

  for (const token of raw.split(/[,\s]+/)) {
    const symbol = token.trim().toUpperCase();
    if (symbol === '') continue;

    if (!SYMBOL.test(symbol)) {
      throw new UpstreamError(`"${token}" is not a symbol the news feed can filter by`, {
        code: 'bad_request',
        hint: 'Symbols look like NVDA, BRK.B or BTCUSD. `NEWS` with no symbol shows the whole wire.',
      });
    }
    if (out.includes(symbol)) continue;
    if (out.length === MAX_SYMBOLS) {
      throw new UpstreamError(`Too many symbols — the news feed takes at most ${MAX_SYMBOLS}`, {
        code: 'bad_request',
      });
    }
    out.push(symbol);
  }

  return out;
}

function clamp(value: number, min: number, max: number, fallback: number): number {
  if (!Number.isFinite(value)) return fallback;
  return Math.min(Math.max(Math.trunc(value), min), max);
}

/* ------------------------------------------------------------ normalising */

interface RawArticle {
  id?: number | string;
  headline?: string;
  summary?: string;
  author?: string;
  source?: string;
  url?: string | null;
  created_at?: string;
  updated_at?: string;
  symbols?: unknown;
}

interface RawNewsResponse {
  news?: RawArticle[];
  next_page_token?: string | null;
}

/**
 * Upstream text → plain text.
 *
 * Wire copy arrives HTML-escaped (`AT&amp;T`, `Q3 &#39;26`) and a summary is cut
 * from the article body, so it can bring markup with it. Every panel renders
 * through `textContent`, which would print `AT&amp;T` literally — the escape has
 * to be undone somewhere, and doing it here means it is done once, on the
 * server, for every consumer of the feed.
 */
export function plainText(raw: unknown): string {
  if (typeof raw !== 'string' || raw === '') return '';
  if (!/[<&]/.test(raw)) return raw.replace(/\s+/g, ' ').trim();

  const $ = cheerio.load(raw);
  $('script, style').remove();
  return $('body').text().replace(/\s+/g, ' ').trim();
}

/**
 * Article links, filtered to schemes a browser may safely open.
 *
 * The headline is rendered as an anchor, and the href comes from an upstream
 * feed. A `javascript:` url in that position is one click from being a script,
 * so anything that is not http(s) is dropped and the headline renders as plain
 * text instead.
 */
export function safeUrl(raw: unknown): string {
  if (typeof raw !== 'string' || raw.trim() === '') return '';
  try {
    const url = new URL(raw.trim());
    return url.protocol === 'https:' || url.protocol === 'http:' ? url.toString() : '';
  } catch {
    return '';
  }
}

/** RFC-3339 → unix seconds. `0` for anything unparseable, never `NaN`. */
export function toUnix(raw: unknown): number {
  if (typeof raw !== 'string' || raw === '') return 0;
  const ms = Date.parse(raw);
  return Number.isFinite(ms) ? Math.floor(ms / 1000) : 0;
}

function symbolList(raw: unknown): string[] {
  if (!Array.isArray(raw)) return [];
  const out: string[] = [];
  for (const entry of raw) {
    if (typeof entry !== 'string') continue;
    const symbol = entry.trim().toUpperCase();
    if (symbol !== '' && !out.includes(symbol)) out.push(symbol);
  }
  return out;
}

export function normaliseArticle(raw: RawArticle): NewsArticle {
  const time = toUnix(raw.created_at);
  const updated = toUnix(raw.updated_at);

  return {
    // Alpaca's ids are integers wide enough to be worth keeping as text: they
    // are an identity, never an amount, and nothing here does arithmetic on one.
    id: raw.id === undefined || raw.id === null ? '' : String(raw.id),
    headline: plainText(raw.headline),
    summary: plainText(raw.summary),
    author: plainText(raw.author),
    publisher: plainText(raw.source),
    url: safeUrl(raw.url),
    time,
    // An unedited item still carries `updated_at`; falling back to `time` keeps
    // the column meaningful when the upstream omits it entirely.
    updated: updated || time,
    symbols: symbolList(raw.symbols),
  };
}

export function normaliseFeed(
  raw: RawNewsResponse,
  symbols: string[],
  days: number,
  sourceUrl: string,
): NewsFeed {
  const articles = (Array.isArray(raw.news) ? raw.news : [])
    .map(normaliseArticle)
    // A row with no headline is not a story — it is a parse that went wrong,
    // and an empty line in a news tape is worse than a missing one.
    .filter((article) => article.headline !== '');

  // Alpaca sorts by *edit* time, so a story revised an hour after publication
  // outranks one published since. Re-sort on publication so the top of the
  // panel is the newest news rather than the newest correction.
  articles.sort((a, b) => b.time - a.time || a.headline.localeCompare(b.headline));

  return { symbols, days, articles, source: 'Alpaca / Benzinga', sourceUrl };
}

/* ----------------------------------------------------------------- fetch */

function startOf(days: number): string {
  return new Date(Date.now() - days * 86_400_000).toISOString();
}

/**
 * A 401/403 here is a wrong key, not a blocked IP.
 *
 * `fetchText` hints "this host may be blocking your IP" for a 403, which is the
 * right guess for the scraped feeds and exactly the wrong one for a keyed API —
 * it would send someone hunting a network problem they do not have.
 */
function explainAuthFailure(err: unknown): never {
  if (err instanceof UpstreamError && (err.status === 401 || err.status === 403)) {
    throw new UpstreamError('Alpaca rejected the API credentials', {
      code: 'bad_credentials',
      status: err.status,
      hint:
        'The key pair was sent but not accepted. Check ALPACA_API_KEY_ID and ' +
        'ALPACA_API_SECRET_KEY are a matching pair, and that they are live keys ' +
        'rather than a paper key with the secret from another account.',
    });
  }
  throw err;
}

/**
 * The latest headlines, newest first.
 *
 * `symbols` empty means the whole wire. The cache key does not include the
 * window's start instant — that moves every call, and keying on it would mean
 * never hitting the cache at all — so within a TTL two callers a second apart
 * share one snapshot, which is what the TTL is for.
 */
export async function getNews(
  symbols: string[],
  rawLimit = 30,
  rawDays = DEFAULT_DAYS,
): Promise<NewsFeed> {
  const limit = clamp(Number(rawLimit), 1, MAX_LIMIT, 30);
  const days = clamp(Number(rawDays), 1, MAX_DAYS, DEFAULT_DAYS);
  const { keyId, secret } = credentials();

  const key = `alpaca:news:${symbols.join(',')}:${limit}:${days}`;

  return cache.cached(key, TTL.news, async () => {
    const url = new URL(`${BASE}/news`);
    if (symbols.length > 0) url.searchParams.set('symbols', symbols.join(','));
    url.searchParams.set('limit', String(limit));
    url.searchParams.set('sort', 'desc');
    url.searchParams.set('start', startOf(days));
    url.searchParams.set('include_content', 'false');
    url.searchParams.set('exclude_contentless', 'false');

    const raw = await fetchJson<RawNewsResponse>(url.toString(), {
      timeoutMs: 20_000,
      retries: 2,
      headers: { 'APCA-API-KEY-ID': keyId, 'APCA-API-SECRET-KEY': secret },
    }).catch(explainAuthFailure);

    if (!raw || !Array.isArray(raw.news)) {
      throw new UpstreamError('Alpaca returned an unexpected news payload', {
        code: 'bad_upstream_body',
      });
    }

    return normaliseFeed(raw, symbols, days, url.toString());
  });
}
