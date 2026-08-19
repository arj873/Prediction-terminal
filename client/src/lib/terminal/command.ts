/**
 * The command contract, and the moves every command makes.
 *
 * A leaf module: it imports no panel and no command table, so a feature module
 * can type its handlers and throw usage errors without importing the table that
 * collects them. In the client this replaces, this file was what stopped the
 * `registry.ts` ↔ `panels/help.ts` cycle from spreading — a cycle benign only
 * because neither side dereferenced the other at module scope. That cycle is
 * gone (`commands.ts` names panel *kinds*, so it imports no panel at all), and
 * keeping this side dependency-free is what keeps it gone.
 *
 * Every import here is type-only, `keys.ts`'s and `context.ts`'s included: those
 * modules import `UsageError` and Svelte's context API at runtime, and only an
 * erased import keeps the pair from becoming a real cycle.
 *
 * The two panel-opening helpers this module used to carry — `openPanel` and
 * `openOrReconfigure` — are gone rather than ported. They existed to decide
 * whether re-issuing a command should re-point a live panel object or build a
 * new one, and a panel is no longer an object: it is a description the grid
 * renders through a keyed `{#each}`. `panels.open({ id, kind, props })` already
 * *is* `openPanel`, and `panels.open(desc, merge)` already *is*
 * `openOrReconfigure` — wrapping either in a same-shaped function would be
 * indirection with nothing behind it.
 */

import type { TerminalContext } from '../context';
import type { KeyMode, Keymap } from './keys';
import type { ParsedCommand } from './parser';

/**
 * What a handler is handed.
 *
 * Deliberately the app's own {@link TerminalContext} widened, not a parallel
 * object: `run`, `log`, `panels` and `workspace` are the same four services
 * every panel already gets, and a handler that logged to a different log than a
 * clicked row is exactly the drift this shape prevents. What is added is the
 * three things only a command needs — the key map `KEYS` edits, the log `CLR`
 * wipes, and the mode `FOCUS NAV` switches to — so the shell can build one
 * object and pass it as either.
 */
/**
 * The menu bar, as `MENU` needs to see it.
 *
 * An interface rather than the component, so the command table stays a pure
 * module: it names what a menu must be able to do and knows nothing about how
 * one is drawn.
 */
export interface MenuControl {
  readonly isOpen: boolean;
  /** The bar's headings, in order — for the usage line and for `MENU` itself. */
  titles(): string[];
  /** Open one by name or unambiguous prefix. False when nothing matches. */
  open(name: string): boolean;
  toggle(): void;
  close(): void;
  /** Walk to the next or previous heading, opening it. */
  cycle(delta: number): void;
}

export interface CommandContext extends TerminalContext {
  /** The live key map, for `KEYS`. */
  keys: Keymap;
  /** Wipe the message log. */
  clearLog(): void;
  /** Move the keyboard between the command line and the panels. */
  setMode(mode: KeyMode): void;
  /** The menu bar, for `MENU`. */
  menu: MenuControl;
}

export interface Command {
  verb: string;
  aliases?: string[];
  /** One-line summary shown in `HELP`. */
  summary: string;
  /** Usage line, e.g. `GP <ticker> [1m|1h|1d] [range]`. */
  usage: string;
  /** Worked examples, shown in `HELP <verb>`. */
  examples?: string[];
  group: 'markets' | 'data' | 'workspace';
  handler(command: ParsedCommand, context: CommandContext): void | Promise<void>;
}

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
 * Generic over the two fields it actually reads rather than fixed to
 * {@link Command}, so nothing here has to know what a command *does* to check
 * that its name is free. Handed the real table it yields `Map<string, Command>`.
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
