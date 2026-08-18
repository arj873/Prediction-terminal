/**
 * The command contract, and the moves every command makes.
 *
 * A leaf module: it imports no panel and no registry, so feature modules can
 * type their handlers and throw usage errors without importing the table that
 * collects them. `registry.ts` and `panels/help.ts` already form a cycle —
 * benign only because neither dereferences the other at module scope — and
 * keeping this side of it dependency-free is what stops that cycle spreading.
 *
 * Every import here is type-only, `keys.ts`'s included: that module imports
 * `UsageError` from this one at runtime, and only an erased import keeps the
 * pair from becoming a real cycle.
 *
 * What has crossed to the Svelte client so far is the half that needs nothing
 * else: the usage error, the argument scanner, and the index that refuses a
 * duplicate verb. `CommandContext`, the `Command` record itself and the two
 * panel-opening helpers name `PanelManager`, `Panel`, `Workspace` and `Keymap`,
 * none of which exist here yet; they land with the panel system, and
 * {@link indexCommands} is written to take the command record as it finds it so
 * that arrival changes nothing at the call site.
 */

/** A command the user typed wrong. Reported with the command's usage line. */
export class UsageError extends Error {}

/* ------------------------------------------------------- argument scanning */

/**
 * A token claim. Returns true when it consumed the token.
 *
 * Claims are tried in order and the first to accept wins, which is exactly what
 * the hand-written if/else chains encoded — and several of those orderings are
 * load-bearing. An interval alias has to be read before a duration, or `GP X 1d`
 * charts a one-day window instead of daily bars. A bare integer has to be
 * claimed as a headline count before the symbol catch-all, because `30` is a
 * valid symbol shape. Keeping precedence as list order keeps those rules
 * visible instead of implicit in statement placement.
 */
export type Claim = (token: string, lower: string) => boolean;

/**
 * Run every token past the claims, in order.
 *
 * Anything nothing claims is a usage error naming the offending token, which is
 * the one error phrasing all six copies of this loop already agreed on.
 */
export function scanTokens(tokens: readonly string[], claims: readonly Claim[]): void {
  for (const token of tokens) {
    const lower = token.toLowerCase();
    if (!claims.some((claim) => claim(token, lower))) {
      throw new UsageError(`Unrecognised argument "${token}"`);
    }
  }
}

/** Claim any of `words`, calling `apply` with the value they stand for. */
export function keyword<T>(words: readonly string[], value: T, apply: (value: T) => void): Claim {
  return (_token, lower) => {
    if (!words.includes(lower)) return false;
    apply(value);
    return true;
  };
}

/** Claim a two-letter ISO country code. `cased` picks the form to store. */
export function countryCode(
  apply: (code: string) => void,
  cased: 'lower' | 'upper' = 'lower',
): Claim {
  return (_token, lower) => {
    if (!/^[a-z]{2}$/.test(lower)) return false;
    apply(cased === 'upper' ? lower.toUpperCase() : lower);
    return true;
  };
}

/** Claim a token a parser recognises, e.g. an interval or a duration. */
export function parsed<T>(
  parse: (token: string) => T | null,
  apply: (value: T) => void,
  accept: () => boolean = () => true,
): Claim {
  return (token) => {
    if (!accept()) return false;
    const value = parse(token);
    if (value === null) return false;
    apply(value);
    return true;
  };
}

/* ------------------------------------------------------------- validation */

/**
 * Index verbs and aliases, refusing a collision.
 *
 * The index was built with a bare `Map.set`, so a duplicate verb or alias
 * silently overwrote its predecessor — the losing command would vanish from
 * autocomplete and `HELP <verb>` would resolve to the winner while the group
 * table still listed both. That is precisely the drift the command table exists
 * to prevent, so it fails loudly at startup instead.
 *
 * Typed against the two fields it reads rather than the whole `Command` record,
 * which is the one concession to arriving before the panel layer: passing the
 * command table once it exists yields `Map<string, Command>` exactly as before.
 */
export function indexCommands<C extends { verb: string; aliases?: readonly string[] }>(
  commands: readonly C[],
): Map<string, C> {
  const index = new Map<string, C>();

  const claim = (key: string, command: C): void => {
    const owner = index.get(key);
    if (owner && owner !== command) {
      throw new Error(
        `Command "${key}" is claimed by both ${owner.verb} and ${command.verb}. ` +
          `Verbs and aliases share one namespace.`,
      );
    }
    index.set(key, command);
  };

  for (const command of commands) {
    claim(command.verb, command);
    for (const alias of command.aliases ?? []) claim(alias, command);
  }

  return index;
}
