/**
 * Command-line parsing.
 *
 * Terminal-style, not shell-style: the verb is the first token, the rest are
 * positional arguments. Quoting is supported so multi-word arguments survive
 * (`FSRCH "real gdp"`), and `--flag` / `--key=value` are recognised, but there
 * is no expansion, substitution, or piping — a mistyped command must never do
 * something surprising.
 */

export interface ParsedCommand {
  /** Upper-cased verb, e.g. `GP`. Empty for a blank line. */
  verb: string;
  /** Positional arguments, in order, with original casing preserved. */
  args: string[];
  /** `--interval=1h` → `{ interval: '1h' }`; bare `--fit` → `{ fit: 'true' }`. */
  flags: Record<string, string>;
  /** The raw input, trimmed. */
  raw: string;
}

/**
 * Split on whitespace, honouring double and single quotes.
 *
 * An unterminated quote is treated as running to end of line rather than being
 * an error — half-typed input is normal at a prompt.
 */
export function tokenize(input: string): string[] {
  const tokens: string[] = [];
  let current = '';
  let quote: '"' | "'" | null = null;
  let hasContent = false;

  for (const char of input) {
    if (quote) {
      if (char === quote) {
        quote = null;
      } else {
        current += char;
      }
      continue;
    }

    if (char === '"' || char === "'") {
      quote = char;
      // An empty quoted string is still an argument.
      hasContent = true;
      continue;
    }

    if (/\s/.test(char)) {
      if (current || hasContent) {
        tokens.push(current);
        current = '';
        hasContent = false;
      }
      continue;
    }

    current += char;
  }

  if (current || hasContent) tokens.push(current);
  return tokens;
}

export function parse(input: string): ParsedCommand {
  const raw = input.trim();
  const tokens = tokenize(raw);

  const args: string[] = [];
  const flags: Record<string, string> = {};

  for (const token of tokens.slice(1)) {
    if (token.startsWith('--') && token.length > 2) {
      const body = token.slice(2);
      const eq = body.indexOf('=');
      if (eq === -1) flags[body.toLowerCase()] = 'true';
      else flags[body.slice(0, eq).toLowerCase()] = body.slice(eq + 1);
      continue;
    }
    args.push(token);
  }

  return {
    verb: (tokens[0] ?? '').toUpperCase(),
    args,
    flags,
    raw,
  };
}

/* --------------------------------------------------------- argument types */

const INTERVAL_ALIASES: Record<string, 1 | 60 | 1440> = {
  '1': 1,
  '1m': 1,
  m: 1,
  min: 1,
  '60': 60,
  '1h': 60,
  h: 60,
  hour: 60,
  hourly: 60,
  '1440': 1440,
  '1d': 1440,
  d: 1440,
  day: 1440,
  daily: 1440,
};

/** `1h` → 60. Returns `null` when the token is not an interval at all. */
export function parseInterval(token: string | undefined): 1 | 60 | 1440 | null {
  if (!token) return null;
  return INTERVAL_ALIASES[token.toLowerCase()] ?? null;
}

const DURATION = /^(\d+(?:\.\d+)?)\s*(m|h|d|w|mo|y)$/i;

const DURATION_SECONDS: Record<string, number> = {
  m: 60,
  h: 3600,
  d: 86400,
  w: 604800,
  mo: 2_592_000,
  y: 31_536_000,
};

/** `30d` → 2592000. Returns `null` when the token is not a duration. */
export function parseDuration(token: string | undefined): number | null {
  if (!token) return null;
  const match = DURATION.exec(token.trim());
  if (!match) return null;
  const amount = Number(match[1]);
  const unit = match[2]!.toLowerCase();
  const seconds = DURATION_SECONDS[unit];
  if (!seconds || !Number.isFinite(amount) || amount <= 0) return null;
  return Math.round(amount * seconds);
}

const ISO_DATE = /^\d{4}-\d{2}-\d{2}$/;

/**
 * Deliberately a plain boolean rather than a type predicate: callers branch on
 * the *negative* case (`arg && !isIsoDate(arg)` → treat as a chart slug), and a
 * `token is string` predicate would narrow that branch to `never`.
 */
export function isIsoDate(token: string | undefined): boolean {
  return typeof token === 'string' && ISO_DATE.test(token) && !Number.isNaN(Date.parse(token));
}

/**
 * A contract reference: an optional `venue:` prefix, then the identifier.
 *
 * Used to tell `GP KXBTC-26 1h` (ticker then interval) from `GP 1h` (a mistake)
 * without a lookup table. The colon is allowed because a Polymarket slug is
 * addressed as `pm:will-the-fed-…`; which prefixes are real is
 * `parseRef`'s business, not this predicate's.
 */
const TICKER = /^[A-Za-z0-9][A-Za-z0-9._:-]*$/;

export function looksLikeTicker(token: string | undefined): token is string {
  return typeof token === 'string' && token.length >= 2 && TICKER.test(token);
}

/**
 * A market symbol, which is a looser thing than a Kalshi ticker.
 *
 * Cash indices carry a caret (`^GSPC`), FX pairs an equals (`EURUSD=X`), crypto
 * pairs a slash-free hyphen (`BTC-USD`), and a single letter is a real NYSE
 * listing — so `looksLikeTicker`'s rules would reject several valid symbols.
 */
const SYMBOL = /^[\^]?[A-Za-z0-9][A-Za-z0-9.=^-]*$/;

export function looksLikeSymbol(token: string | undefined): token is string {
  return typeof token === 'string' && token.length >= 1 && SYMBOL.test(token);
}
