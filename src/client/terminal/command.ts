/**
 * The command contract, and the moves every command makes.
 *
 * A leaf module: it imports no panel and no registry, so feature modules can
 * type their handlers and throw usage errors without importing the table that
 * collects them. `registry.ts` and `panels/help.ts` already form a cycle —
 * benign only because neither dereferences the other at module scope — and
 * keeping this side of it dependency-free is what stops that cycle spreading.
 */

import type { PanelManager } from '../panels/manager.js';
import type { Panel, PanelContext } from '../panels/panel.js';
import type { Workspace } from '../state.js';
import type { ParsedCommand } from './parser.js';

export interface CommandContext {
  panels: PanelManager;
  workspace: Workspace;
  panelContext: PanelContext;
  log(message: string, level?: 'info' | 'warn' | 'error'): void;
  /** Re-dispatch a command string. */
  run(command: string): void;
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

/* --------------------------------------------------------- opening panels */

/**
 * A panel that can be re-pointed in place rather than reopened.
 *
 * Re-issuing a command for a market already on screen should re-cut that panel,
 * not tile a second copy of it — which is what makes `GP X` safe to mash.
 */
export interface Reconfigurable<O> {
  reconfigure(options: Partial<O>): void;
}

/** Open a panel, or focus and refresh the one already carrying that id. */
export function openPanel(
  { panels, panelContext }: CommandContext,
  id: string,
  build: (id: string, context: PanelContext) => Panel,
): void {
  panels.open(id, () => build(id, panelContext));
}

/**
 * Open a panel, or reconfigure the open one with the same id.
 *
 * `merge` is how a command says what re-issuing it means. `GP` replaces the
 * chart's arguments wholesale; `IMP` is additive, folding the overlays named on
 * the command line into the ones already drawn — the same thing clicking the
 * picker does. Both were hand-written, and diverged in more than that: the
 * bespoke copies also disagreed on whether to focus before or after refreshing.
 */
export function openOrReconfigure<P extends Panel & Reconfigurable<O>, O>(
  context: CommandContext,
  id: string,
  is: (panel: Panel) => panel is P,
  options: O,
  build: (id: string, panelContext: PanelContext) => P,
  merge?: (existing: P, incoming: O) => Partial<O>,
): void {
  const { panels, panelContext } = context;
  const existing = panels.find(id);

  if (existing && is(existing)) {
    existing.reconfigure(merge ? merge(existing, options) : options);
    panels.focus(id);
    return;
  }

  panels.open(id, () => build(id, panelContext));
}

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
 */
export function indexCommands(commands: readonly Command[]): Map<string, Command> {
  const index = new Map<string, Command>();

  const claim = (key: string, command: Command): void => {
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
