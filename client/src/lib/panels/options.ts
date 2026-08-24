/**
 * How the option panels print the numbers that carry units.
 *
 * Kept out of the four components because the units are the thing that goes
 * wrong, and three copies of a formatter is three chances to get one of them
 * wrong on its own. A volatility crosses the wire as a *decimal* — `0.42` is
 * 42% — and printing it took a `* 100` too many in the first cut of these
 * panels, which rendered a perfectly ordinary 43% Bitcoin vol as `4335.0%`.
 * `format.percent` already scales a fraction, so nothing here may scale it
 * again.
 */

import { percent } from '../format';

/** A volatility, which crosses the wire as a decimal fraction. */
export function vol(value: number | null | undefined, digits = 1): string {
  return value === null || value === undefined ? '--' : percent(value, digits);
}

/** A continuous rate or carry, likewise a decimal fraction. */
export function rate(value: number | null | undefined): string {
  return value === null || value === undefined ? '--' : percent(value, 2);
}

/**
 * A risk reversal, which is a *difference* of two volatilities and so wants its
 * sign shown: positive means the downside wing is bid.
 */
export function skew(value: number | null | undefined): string {
  if (value === null || value === undefined) return '--';
  return `${value > 0 ? '+' : ''}${percent(value, 2)}`;
}

/**
 * A Greek, at the precision its magnitude needs.
 *
 * `null` and not `0`: a contract with no derivable volatility has *unknown*
 * sensitivities, and a zero delta is a real and very different statement.
 */
export function greek(value: number | null | undefined, digits: number): string {
  return value === null || value === undefined ? '--' : value.toFixed(digits);
}

/** How a forward was arrived at, in the words the panel shows. */
export function forwardWord(source: string): string {
  if (source === 'venue') return 'venue';
  if (source === 'parity') return 'parity';
  return 'assumed';
}

/**
 * `assumed` is the one the reader has to be warned about: it means nothing in
 * the market would state a carry, so every Greek beside it is derived from a
 * configured guess rather than from a quote.
 */
export function forwardTone(source: string): string {
  return source === 'assumed' ? 'warn' : 'dim';
}
