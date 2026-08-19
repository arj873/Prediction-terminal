/**
 * Formatters.
 *
 * A terminal is a column-aligned medium: everything here returns fixed-width,
 * monospace-friendly output, and every function has a defined answer for
 * `null`. Missing data prints as `--`, never as `NaN`, `0` or an empty cell —
 * a blank where a price should be is a bug you want to see.
 */

export const EM_DASH = '--';

/** Kalshi prices are dollars 0..1; traders read them as cents. */
export function cents(value: number | null | undefined, width = 0): string {
  if (value === null || value === undefined || !Number.isFinite(value)) {
    return EM_DASH.padStart(width);
  }
  const c = value * 100;
  // Sub-cent tick sizes exist on some markets; show a decimal only when needed.
  const text = Number.isInteger(Math.round(c * 100) / 100) ? `${Math.round(c)}` : c.toFixed(1);
  return `${text}¢`.padStart(width);
}

/** Probability reading of a contract price. */
export function percent(value: number | null | undefined, digits = 1, width = 0): string {
  if (value === null || value === undefined || !Number.isFinite(value)) {
    return EM_DASH.padStart(width);
  }
  return `${(value * 100).toFixed(digits)}%`.padStart(width);
}

/** Signed change in cents, with an explicit `+` so the sign is never ambiguous. */
export function signedCents(value: number | null | undefined, width = 0): string {
  if (value === null || value === undefined || !Number.isFinite(value)) {
    return EM_DASH.padStart(width);
  }
  const c = Math.round(value * 100);
  return `${c > 0 ? '+' : c < 0 ? '' : ' '}${c}¢`.padStart(width);
}

/** 12345.6 → `12.3K`; 1234567 → `1.23M`. Keeps columns to 6 chars. */
export function compact(value: number | null | undefined, width = 0): string {
  if (value === null || value === undefined || !Number.isFinite(value)) {
    return EM_DASH.padStart(width);
  }
  const abs = Math.abs(value);
  const sign = value < 0 ? '-' : '';
  let text: string;
  if (abs >= 1e9) text = `${(abs / 1e9).toFixed(2)}B`;
  else if (abs >= 1e6) text = `${(abs / 1e6).toFixed(2)}M`;
  else if (abs >= 1e4) text = `${(abs / 1e3).toFixed(1)}K`;
  else if (abs >= 1) text = abs.toFixed(0);
  else if (abs === 0) text = '0';
  else text = abs.toFixed(2);
  return `${sign}${text}`.padStart(width);
}

/** Thousands-separated, for figures that deserve their exact value. */
export function group(value: number | null | undefined, digits = 0): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return EM_DASH;
  return value.toLocaleString('en-US', {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  });
}

/**
 * An underlying's price, with decimals chosen from its magnitude.
 *
 * One formatter has to serve BTC at 63,058.21 and EUR/USD at 1.0842. Fixing the
 * decimals would either bury the FX pair's whole daily range or pad crypto with
 * meaningless digits, so the scale of the number picks.
 */
export function price(value: number | null | undefined, reference?: number): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return EM_DASH;
  const digits = priceDigits(reference ?? value);
  return value.toLocaleString('en-US', {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  });
}

/**
 * Decimals for a price of this magnitude.
 *
 * `reference` exists for *differences*. A $8.57 basis on a $63,000 instrument
 * is a large-instrument number that happens to be small, and formatting it from
 * its own magnitude prints `-8.5714` next to `63,049.41` — two different
 * precisions for two halves of the same subtraction. Passing the price as the
 * reference keeps them in step.
 */
function priceDigits(magnitude: number): number {
  const abs = Math.abs(magnitude);
  if (!Number.isFinite(abs)) return 2;
  return abs >= 10 ? 2 : abs >= 1 ? 4 : 6;
}

/** Signed price move, with an explicit `+`, matching {@link price}'s decimals. */
export function signedPrice(value: number | null | undefined, reference?: number): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return EM_DASH;
  return `${value > 0 ? '+' : ''}${price(value, reference ?? value)}`;
}

/** `+1.24%` / `-0.30%`. */
export function signedPercent(value: number | null | undefined, digits = 2): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return EM_DASH;
  return `${value > 0 ? '+' : ''}${value.toFixed(digits)}%`;
}

/**
 * A general-purpose numeric formatter for FRED, whose series range from
 * fractions of a percent to trillions of dollars.
 */
export function metric(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return EM_DASH;
  const abs = Math.abs(value);
  if (abs >= 1e4 || (abs < 1e-3 && abs > 0)) {
    return value.toLocaleString('en-US', { maximumFractionDigits: 3 });
  }
  return value.toLocaleString('en-US', { maximumFractionDigits: abs < 10 ? 3 : 2 });
}

/* ------------------------------------------------------------------- time */

const UTC_TIME = new Intl.DateTimeFormat('en-GB', {
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hour12: false,
  timeZone: 'UTC',
});

const ET_TIME = new Intl.DateTimeFormat('en-US', {
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hour12: false,
  timeZone: 'America/New_York',
});

const UTC_STAMP = new Intl.DateTimeFormat('en-GB', {
  day: '2-digit',
  month: 'short',
  year: 'numeric',
  hour: '2-digit',
  minute: '2-digit',
  hour12: false,
  timeZone: 'UTC',
});

const UTC_DAY = new Intl.DateTimeFormat('en-GB', {
  day: '2-digit',
  month: 'short',
  year: 'numeric',
  timeZone: 'UTC',
});

export function clockUtc(date = new Date()): string {
  return UTC_TIME.format(date);
}

export function clockEt(date = new Date()): string {
  return ET_TIME.format(date);
}

/** `16 Aug 2026, 14:25` — for close/expiry timestamps. */
export function stamp(iso: string | number | null | undefined): string {
  if (iso === null || iso === undefined || iso === '') return EM_DASH;
  const date = typeof iso === 'number' ? new Date(iso * 1000) : new Date(iso);
  if (Number.isNaN(date.getTime())) return EM_DASH;
  return UTC_STAMP.format(date).replace(',', '');
}

/** `16 Aug 2026` — for chart weeks and observation dates. */
export function day(iso: string | null | undefined): string {
  if (!iso) return EM_DASH;
  // Treat a bare YYYY-MM-DD as calendar-local, not an instant, so a date near
  // midnight does not shift a day under timezone conversion.
  const date = /^\d{4}-\d{2}-\d{2}$/.test(iso) ? new Date(`${iso}T00:00:00Z`) : new Date(iso);
  if (Number.isNaN(date.getTime())) return EM_DASH;
  return UTC_DAY.format(date);
}

/** `14:25:07` in UTC, for a trade tape. */
export function timeOfDay(unixSeconds: number): string {
  return UTC_TIME.format(new Date(unixSeconds * 1000));
}

/** `3d 04h`, `04h 12m`, `12m 30s` — time remaining until a market closes. */
export function countdown(iso: string | null | undefined): string {
  if (!iso) return EM_DASH;
  const target = new Date(iso).getTime();
  if (Number.isNaN(target)) return EM_DASH;

  let remaining = Math.floor((target - Date.now()) / 1000);
  if (remaining <= 0) return 'CLOSED';

  const days = Math.floor(remaining / 86400);
  remaining %= 86400;
  const hours = Math.floor(remaining / 3600);
  remaining %= 3600;
  const minutes = Math.floor(remaining / 60);
  const seconds = remaining % 60;

  if (days > 0) return `${days}d ${String(hours).padStart(2, '0')}h`;
  if (hours > 0) return `${String(hours).padStart(2, '0')}h ${String(minutes).padStart(2, '0')}m`;
  return `${String(minutes).padStart(2, '0')}m ${String(seconds).padStart(2, '0')}s`;
}

/* ------------------------------------------------------------------- text */

/** Truncate with an ellipsis so a long title cannot break a table's columns. */
export function truncate(text: string, max: number): string {
  if (text.length <= max) return text;
  return `${text.slice(0, Math.max(0, max - 1))}…`;
}

/** `+3`/`-12`/`=` for a Billboard rank move. */
export function move(value: number | null): string {
  if (value === null) return 'NEW';
  if (value === 0) return '=';
  return value > 0 ? `+${value}` : `${value}`;
}

/**
 * Rank movement, as text and a tone, with `unknown` kept distinct from `held`.
 *
 * {@link move} reads a null as a debut, which is right for Billboard — where
 * the parser derives `move` and `isNew` from the same missing last-week rank —
 * and wrong for any chart that simply has no movement column, where it would
 * report every row as NEW. This reads the debut flag instead of inferring it,
 * so both kinds of feed render correctly from one helper.
 */
export function rankMove(value: number | null, isNew: boolean): { text: string; tone: string } {
  if (isNew) return { text: 'NEW', tone: 'new' };
  if (value === null) return { text: '—', tone: 'dim' };
  if (value === 0) return { text: '=', tone: 'dim' };
  return { text: value > 0 ? `+${value}` : `${value}`, tone: value > 0 ? 'up' : 'down' };
}

/** `$19,000,000` → `$19.0M`, for money that belongs in a fixed-width column. */
export function money(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return EM_DASH;
  return `$${compact(value)}`;
}

/** CSS class for a directional value. Drives the up/down colouring. */
export function direction(value: number | null | undefined): 'up' | 'down' | 'flat' {
  if (value === null || value === undefined || !Number.isFinite(value) || value === 0)
    return 'flat';
  return value > 0 ? 'up' : 'down';
}
