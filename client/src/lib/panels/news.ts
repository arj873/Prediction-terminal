/**
 * The two readings the news wire needs before it can print a row: when a story
 * was filed, and which chart the symbol it is tagged with belongs to.
 *
 * Both are pure and neither is about the wire's shape, so they live beside the
 * panel rather than inside it — and the second one has tests.
 */

import { EM_DASH } from '../format';

const HHMM = new Intl.DateTimeFormat('en-GB', {
  hour: '2-digit',
  minute: '2-digit',
  hour12: false,
  timeZone: 'UTC',
});

const DAY_HHMM = new Intl.DateTimeFormat('en-GB', {
  day: '2-digit',
  month: 'short',
  hour: '2-digit',
  minute: '2-digit',
  hour12: false,
  timeZone: 'UTC',
});

/**
 * `14:25Z` for today, `14 Aug 09:02` for anything older.
 *
 * A wire panel is mostly today, where the clock is what matters and a date on
 * every row is noise — but the window reaches back a week, and a bare time on a
 * three-day-old story would read as three hours old.
 *
 * The wire type is a `bigint` because the server sends unix seconds out of a
 * `u64`; it is an instant, not an amount, so it is narrowed once here rather
 * than at every call site.
 */
export function wireTime(unixSeconds: bigint | number): string {
  const seconds = Number(unixSeconds);
  if (!Number.isFinite(seconds) || seconds <= 0) return EM_DASH;
  const date = new Date(seconds * 1000);
  const today = new Date();
  const sameDay = date.toISOString().slice(0, 10) === today.toISOString().slice(0, 10);
  return sameDay ? `${HHMM.format(date)}Z` : DAY_HHMM.format(date).replace(',', '');
}

/**
 * The chart command for a tagged symbol.
 *
 * Crypto stories are tagged with a pair (`BTCUSD`), and `STK BTCUSD` prices
 * nothing — those go to `CRY` with the quote currency stripped. Matching the
 * pair shape rather than a list of known coins keeps an equity ticker that
 * happens to share a name with a token (`LINK`) on the equity feed.
 */
const CRYPTO_PAIR = /^([A-Z]{2,10})[-/]?(?:USD|USDT|USDC)$/;

export function chartCommand(symbol: string): string {
  const pair = CRYPTO_PAIR.exec(symbol);
  return pair ? `CRY ${pair[1]}` : `STK ${symbol}`;
}
