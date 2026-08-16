/**
 * Ordered provider chains: several upstreams behind one answer.
 *
 * Four places in this server already fall back from one source to another —
 * FRED scrapes then tries the official API, equities try Yahoo then Nasdaq,
 * the box office walks back through earlier dates, Rotten Tomatoes guesses a
 * slug then searches. All four were written independently. Two of them were
 * even called `withFallback`, with different signatures, different both-failed
 * behaviour, and no shared code; the rule that a `not_found` is a real answer
 * rather than a reason to try the next source was hand-copied into all four.
 *
 * A chain states the things those copies had in common and parameterises the
 * things they did not:
 *
 *   - the order, as a list rather than a hardcoded pair;
 *   - whether a provider is usable at all in this deployment (an unset key);
 *   - which error codes are a definitive answer and end the chain;
 *   - what error to raise when every provider has failed;
 *   - and which provider actually answered, returned rather than tracked by a
 *     mutated closure variable or a string literal at each construction site.
 *
 * That last one is why this returns {@link Attributed} rather than a bare
 * value: a reader deciding how much to trust a number wants to know whether it
 * came from the source of record or the fallback, and four of the response
 * types that have a fallback behind them could not say.
 */

import { UpstreamError } from './http.js';

export interface Provider<T> {
  /** Stable id, surfaced to the client as the answering source. */
  id: string;
  /** Human name, for error text and logs. */
  label: string;
  /**
   * Whether this deployment can use the provider at all — an unset API key,
   * say. An unavailable provider is skipped without being counted as a failure.
   */
  available?(): boolean;
  /**
   * A hint to attach when this provider was skipped and nothing else answered.
   * Lets FRED tell the operator that setting a key would have covered the gap.
   */
  skippedHint?(cause: UpstreamError | undefined): string | undefined;
  run(): Promise<T>;
}

/** A value and the provider that produced it. */
export interface Attributed<T> {
  value: T;
  /** The answering provider's `id`. */
  source: string;
}

export interface ProviderFailure {
  id: string;
  label: string;
  error: unknown;
}

export interface ChainOptions {
  /** What was being fetched, for the exhausted-chain error. e.g. `a quote for AAPL`. */
  what: string;
  /**
   * Codes that mean the upstream gave a real answer and no other provider will
   * contradict it. Ends the chain immediately, propagating that error.
   */
  terminalCodes?: readonly string[];
  /**
   * Codes that propagate untouched instead of being folded into the
   * exhausted-chain error — a capability gap the caller should see as itself.
   */
  passThroughCodes?: readonly string[];
  /** Build the error raised when every available provider failed. */
  onExhausted?(failures: readonly ProviderFailure[]): UpstreamError;
  /** Prefix for the warning logged when more than one provider failed. */
  logPrefix?: string;
}

const DEFAULT_TERMINAL = ['not_found'] as const;
const DEFAULT_PASS_THROUGH = ['unsupported'] as const;

function codeOf(error: unknown): string | undefined {
  return error instanceof UpstreamError ? error.code : undefined;
}

/**
 * Run providers in order and return the first answer.
 *
 * Skips providers this deployment cannot use, stops on a definitive answer,
 * and raises a single described error when nothing worked.
 */
export async function firstAnswer<T>(
  providers: readonly Provider<T>[],
  options: ChainOptions,
): Promise<Attributed<T>> {
  const terminal = options.terminalCodes ?? DEFAULT_TERMINAL;
  const passThrough = options.passThroughCodes ?? DEFAULT_PASS_THROUGH;

  const failures: ProviderFailure[] = [];
  const skipped: Provider<T>[] = [];

  for (const provider of providers) {
    if (provider.available && !provider.available()) {
      skipped.push(provider);
      continue;
    }

    try {
      return { value: await provider.run(), source: provider.id };
    } catch (error) {
      const code = codeOf(error);

      // A definitive answer from any provider is the answer. Asking the next
      // one cannot turn "no such series" into a series.
      if (code !== undefined && terminal.includes(code)) throw error;

      failures.push({ id: provider.id, label: provider.label, error });
    }
  }

  // Nothing answered. If a provider was skipped for want of configuration, and
  // it could have covered this, say so on the way out.
  const first = failures[0]?.error;
  const cause = first instanceof UpstreamError ? first : undefined;

  for (const provider of skipped) {
    const hint = provider.skippedHint?.(cause);
    if (hint !== undefined && cause) {
      throw new UpstreamError(cause.message, { code: cause.code, hint });
    }
  }

  if (failures.length === 0) {
    throw new UpstreamError(`No source is configured to serve ${options.what}`, {
      code: 'not_configured',
      hint: providers.map((p) => p.label).join(', ') + ' are all unavailable in this deployment.',
    });
  }

  if (failures.length > 1 && options.logPrefix) {
    console.warn(
      `[${options.logPrefix}] every provider failed for ${options.what}`,
      Object.fromEntries(failures.map((f) => [f.id, f.error])),
    );
  }

  // A capability gap is worth surfacing as itself rather than as a generic
  // "nothing could answer" — it is a fact about the provider, not an outage.
  for (const failure of failures) {
    const code = codeOf(failure.error);
    if (code !== undefined && passThrough.includes(code)) throw failure.error;
  }

  if (options.onExhausted) throw options.onExhausted(failures);

  // Default: the first failure is the most informative, because the first
  // provider is the source of record and its error says why it declined.
  throw first;
}
