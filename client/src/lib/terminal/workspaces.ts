/**
 * Named workspaces, and the link that carries one.
 *
 * A grid is saved as the command lines that opened its panels, and reopened by
 * running them again. That is the whole design, and it is a security decision
 * before it is a convenience: a `#ws=` link is text somebody else wrote, and
 * the terminal already has exactly one thing it trusts text to do — be typed at
 * the prompt. So a link is not deserialised into panels. It is split into
 * lines, each line is checked against the short list of verbs that only open a
 * panel, and what survives goes through the same dispatch a typed line takes. A
 * link can therefore do nothing a reader typing commands could not, and less:
 * it cannot edit the watchlist, bind a key, change the theme, or save and
 * delete workspaces, because those verbs are not on the list.
 *
 * The same check runs when a workspace comes back out of storage. Storage is a
 * text box anyone can edit, and one rule applied to both is one rule to test.
 *
 * Nothing here imports the command table — the table imports this — so the
 * index of verbs is handed in wherever an alias has to be resolved.
 */

import type { PanelDesc } from '../state/panels.svelte';
import { MAX_GRID_PANELS, isWorkspaceName, type SavedGrid } from '../state/workspace.svelte';
import { UsageError, type CommandContext } from './command';
import { parse, type ParsedCommand } from './parser';

/** A link's hash, `#ws=` included, may not be longer than this. */
export const MAX_LINK_LENGTH = 4096;

/** One command line, the same cap a key binding's command has. */
const MAX_LINE_LENGTH = 200;

const LINK_PREFIX = 'ws=';

/**
 * The verbs a saved grid or a link may run: the ones whose only effect is to
 * open a panel.
 *
 * Written out rather than derived from a command's group, because "reads
 * something and shows it" is a property of a handler and not of where `HELP`
 * files it. A verb added to the table is left out until someone puts it here,
 * and `workspaces.test.ts` runs every one of these against a context in which
 * touching anything but the panel grid and the log throws.
 */
export const PANEL_VERBS: ReadonlySet<string> = new Set([
  ...['GP', 'DES', 'OB', 'TAS', 'SRCH', 'EVT', 'TOP', 'STK', 'CRY', 'IMP', 'XV', 'ENT'],
  ...['OPT', 'OPD', 'VOL', 'OI'],
  ...['NEWS', 'ECO', 'SEC', 'CONG', 'DGOV', 'ECOS', 'SRC'],
  ...['BB', 'RT', 'NFLX', 'SPOT', 'YT', 'BO', 'AWRD', 'TRND', 'REL', 'POD', 'STEAM', 'TV'],
  ...['W', 'KEYS', 'HELP'],
]);

/**
 * Verbs that open a panel bare and edit stored state with an argument.
 *
 * `W` is the watchlist and `W ADD X` changes it; `KEYS` is the key map and
 * `KEYS alt+b CLS` rebinds a key. Only the bare form is a panel command.
 */
const BARE_ONLY: ReadonlySet<string> = new Set(['W', 'KEYS']);

/** What the checks below need from the command table: a verb or alias, resolved. */
export type VerbIndex = ReadonlyMap<string, { verb: string }>;

function hasArguments(parsed: ParsedCommand): boolean {
  return parsed.args.length > 0 || Object.keys(parsed.flags).length > 0;
}

/**
 * Is this line one that only opens a panel?
 *
 * Returns the line to run, or `undefined` when it is anything else: too long,
 * not a command, a command off the list, or a listed one in a form that edits
 * something.
 */
export function panelCommand(line: string, index: VerbIndex): string | undefined {
  const text = line.trim();
  if (text === '' || text.length > MAX_LINE_LENGTH) return undefined;
  // No control characters: a line is shown in the log as well as run.
  if (/\p{Cc}/u.test(text)) return undefined;

  const parsed = parse(text);
  const verb = index.get(parsed.verb)?.verb;
  if (verb === undefined || !PANEL_VERBS.has(verb)) return undefined;
  if (BARE_ONLY.has(verb) && hasArguments(parsed)) return undefined;
  return text;
}

/**
 * The line that would reopen a panel, from the line that opened it.
 *
 * Almost always the same line. The exception is the panels that an editing
 * command also opens — `W ADD X` leaves the watchlist on screen — where what
 * reopens the panel is the bare verb, and replaying the edit would be wrong.
 */
function reopeningLine(line: string | undefined, index: VerbIndex): string | undefined {
  if (line === undefined) return undefined;
  const verb = index.get(parse(line).verb)?.verb;
  return panelCommand(verb !== undefined && BARE_ONLY.has(verb) ? verb : line, index);
}

/** The open grid as command lines, and the panels no line reopens. */
export function snapshot(
  panels: readonly PanelDesc[],
  index: VerbIndex,
): { lines: string[]; skipped: string[] } {
  const lines: string[] = [];
  const skipped: string[] = [];

  for (const panel of panels) {
    const line = reopeningLine(panel.command, index);
    if (line === undefined) skipped.push(panel.command ?? panel.id);
    else lines.push(line);
  }

  return { lines, skipped };
}

/* -------------------------------------------------------------------- links */

/**
 * A grid as a URL hash: `#ws=2;GP+KXFED-1;W`.
 *
 * Readable on purpose. Somebody handed a link should be able to see what it
 * will open before they open it, so the lines are percent-encoded and nothing
 * more — columns first, then one line per panel, `;` between, `+` for a space.
 */
export function encodeLink(grid: SavedGrid): string {
  const lines = grid.panels.map((line) => encodeURIComponent(line).replace(/%20/g, '+'));
  return `#${LINK_PREFIX}${[String(grid.columns), ...lines].join(';')}`;
}

export type LinkReading =
  /** Not a workspace link at all — some other hash, or none. */
  | { kind: 'none' }
  | { kind: 'grid'; grid: SavedGrid }
  /** A workspace link that cannot be read. `reason` finishes a sentence. */
  | { kind: 'error'; reason: string };

/**
 * Read a URL hash back into a grid.
 *
 * All or nothing: a hash that is too long, names too many panels, or has a
 * part that does not decode is refused whole rather than half-opened. What the
 * lines *say* is not judged here — that is `panelCommand`, when they are run.
 */
export function decodeLink(hash: string): LinkReading {
  const body = hash.startsWith('#') ? hash.slice(1) : hash;
  if (!body.startsWith(LINK_PREFIX)) return { kind: 'none' };

  if (hash.length > MAX_LINK_LENGTH) {
    return { kind: 'error', reason: `it is longer than ${MAX_LINK_LENGTH} characters` };
  }

  const [columns, ...parts] = body.slice(LINK_PREFIX.length).split(';');
  if (columns === undefined || !/^[1-4]$/.test(columns)) {
    return { kind: 'error', reason: 'it does not start with a column count from 1 to 4' };
  }
  if (parts.length === 0) return { kind: 'error', reason: 'it names no panels' };
  if (parts.length > MAX_GRID_PANELS) {
    return {
      kind: 'error',
      reason: `it names ${parts.length} panels and the grid holds ${MAX_GRID_PANELS}`,
    };
  }

  const panels: string[] = [];
  for (const part of parts) {
    let line: string;
    try {
      line = decodeURIComponent(part.replace(/\+/g, ' ')).trim();
    } catch {
      return { kind: 'error', reason: 'part of it is not valid percent-encoding' };
    }
    if (line === '') return { kind: 'error', reason: 'it has an empty panel in it' };
    if (line.length > MAX_LINE_LENGTH) {
      return { kind: 'error', reason: `one of its commands is over ${MAX_LINE_LENGTH} characters` };
    }
    panels.push(line);
  }

  return { kind: 'grid', grid: { columns: Number(columns), panels } };
}

/* ---------------------------------------------------------------- reopening */

/** What `reopen` needs: the grid, and the dispatch a typed line takes. */
export type ReopenContext = Pick<CommandContext, 'panels' | 'replay'>;

/**
 * Replace the open grid with a stored one.
 *
 * Every line is checked, then run through `replay` — the terminal's one
 * dispatch, minus the echo and the history entry. A line that is not a panel
 * command is never run; a line that is one but fails (a ticker the parser
 * refuses) has already been reported by the dispatch. Both come back in
 * `skipped`, so the caller can name them.
 *
 * Touches the panel grid and nothing else. The column count is set on the grid
 * only: whether it is also *remembered* is for the caller, who knows whether
 * this grid is the reader's own or somebody's link.
 */
export function reopen(
  grid: SavedGrid,
  context: ReopenContext,
  index: VerbIndex,
): { opened: number; skipped: string[] } {
  const skipped: string[] = [];
  let opened = 0;

  context.panels.closeAll();
  context.panels.setColumns(grid.columns);

  for (const line of grid.panels.slice(0, MAX_GRID_PANELS)) {
    const allowed = panelCommand(line, index);
    if (allowed !== undefined && context.replay(allowed)) opened += 1;
    else skipped.push(line);
  }

  return { opened, skipped };
}

function plural(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? '' : 's'}`;
}

/** A line, short enough to quote in the log. */
function quote(line: string): string {
  // Control characters never reach the log, even from a line that was refused.
  const clean = line.replace(/\p{Cc}/gu, ' ');
  return `"${clean.length > 48 ? `${clean.slice(0, 47)}…` : clean}"`;
}

/** The sentence that names what was left out, or nothing. */
export function skippedNote(skipped: readonly string[]): string {
  if (skipped.length === 0) return '';
  return ` Skipped ${plural(skipped.length, 'line')} that ${
    skipped.length === 1 ? 'is not a panel command' : 'are not panel commands'
  } or would not open: ${skipped.map(quote).join(', ')}.`;
}

/**
 * Open the workspace a URL hash describes.
 *
 * False when the hash is not a workspace link, or is one that cannot be read —
 * the caller boots as it otherwise would. Nothing is written to storage either
 * way: a linked grid is a visit, and `WS SAVE` is how a reader keeps one.
 */
export function openLink(
  hash: string,
  context: ReopenContext & Pick<CommandContext, 'log'>,
  index: VerbIndex,
): boolean {
  const reading = decodeLink(hash);
  if (reading.kind === 'none') return false;
  if (reading.kind === 'error') {
    context.log(`That link is not a workspace: ${reading.reason}. Nothing was opened.`, 'error');
    return false;
  }

  const { opened, skipped } = reopen(reading.grid, context, index);
  context.log(
    `Opened a linked workspace: ${plural(opened, 'panel')}.${skippedNote(skipped)} ` +
      `It is not saved — WS SAVE <name> keeps it.`,
    skipped.length > 0 ? 'warn' : 'info',
  );
  return true;
}

/* -------------------------------------------------------------- the command */

/** The words `WS` reads as actions, which a workspace may therefore not be called. */
const RESERVED = new Set(['SAVE', 'DEL', 'RM', 'LINK', 'LIST']);

/** The two things `WS LINK` needs from a browser, so a test can stand in for one. */
export interface LinkEnvironment {
  /** The page's address without its hash, or `undefined` where there is no page. */
  base(): string | undefined;
  /** Put text on the clipboard. Rejects, or throws, where that is not allowed. */
  copy(text: string): Promise<void>;
}

const BROWSER: LinkEnvironment = {
  base: () =>
    typeof location === 'undefined'
      ? undefined
      : `${location.origin}${location.pathname}${location.search}`,
  copy: (text) => {
    // Absent outside a secure context, and on a page served over plain HTTP.
    if (typeof navigator === 'undefined' || !navigator.clipboard) {
      return Promise.reject(new Error('no clipboard'));
    }
    return navigator.clipboard.writeText(text);
  },
};

function requireName(command: ParsedCommand, index: number): string {
  const name = command.args[index]?.toUpperCase();
  if (!name) throw new UsageError('Missing <name>');
  if (!isWorkspaceName(name)) {
    throw new UsageError(
      `"${command.args[index]}" is not a name — letters, digits, - and _, up to 24`,
    );
  }
  return name;
}

/** The open grid, or a usage error when there is nothing in it to keep. */
function currentGrid(context: CommandContext, index: VerbIndex): SavedGrid & { note: string } {
  const { lines, skipped } = snapshot(context.panels.panels, index);
  if (lines.length === 0) {
    throw new UsageError(
      skipped.length > 0
        ? 'No open panel is one a command reopens'
        : 'No panels are open — there is nothing to save',
    );
  }
  const note =
    skipped.length > 0
      ? ` Left out ${plural(skipped.length, 'panel')} no command reopens: ${skipped.map(quote).join(', ')}.`
      : '';
  return { columns: context.panels.columns, panels: lines, note };
}

/**
 * `WS` — list, save, reopen, delete, link.
 *
 * The handler behind the table's entry. It lives here, beside the rules it
 * applies, and takes the verb index as an argument so this module never has to
 * import the table that imports it.
 */
export function workspaceCommand(
  command: ParsedCommand,
  context: CommandContext,
  index: VerbIndex,
  environment: LinkEnvironment = BROWSER,
): void | Promise<void> {
  const { workspace, panels, log } = context;
  const action = (command.args[0] ?? '').toUpperCase();

  if (action === '' || action === 'LIST') {
    const saved = Object.entries(workspace.workspaces);
    if (saved.length === 0) {
      log('No saved workspaces. WS SAVE <name> keeps the open grid.');
      return;
    }
    log(`${plural(saved.length, 'saved workspace')} — WS <name> reopens one:`);
    for (const [name, grid] of saved) {
      log(`  ${name} · ${plural(grid.panels.length, 'panel')} · ${grid.panels.join(' · ')}`);
    }
    return;
  }

  if (action === 'SAVE') {
    const name = requireName(command, 1);
    if (RESERVED.has(name)) throw new UsageError(`${name} is a WS action, not a name`);
    const { note, ...grid } = currentGrid(context, index);
    const replaced = workspace.workspace(name) !== undefined;
    if (!workspace.saveWorkspace(name, grid)) {
      throw new UsageError('The shelf is full — WS DEL <name> makes room');
    }
    log(
      `${replaced ? 'Replaced' : 'Saved'} workspace ${name}: ${plural(grid.panels.length, 'panel')}.${note}`,
      note ? 'warn' : 'info',
    );
    return;
  }

  if (action === 'DEL' || action === 'RM') {
    const name = requireName(command, 1);
    log(
      workspace.deleteWorkspace(name)
        ? `Deleted workspace ${name}.`
        : `There is no workspace called ${name}.`,
    );
    return;
  }

  if (action === 'LINK') {
    const { note, ...grid } = currentGrid(context, index);
    const hash = encodeLink(grid);
    if (hash.length > MAX_LINK_LENGTH) {
      throw new UsageError('This grid is too long to put in a link — close a panel or two');
    }
    const base = environment.base();
    if (base === undefined) throw new UsageError('There is no page address to link to');
    const url = `${base}${hash}`;
    const summary = `${plural(grid.panels.length, 'panel')}.${note}`;

    const fallback = (): void => {
      log(`The clipboard is not available — the link to this grid, ${summary}`, 'warn');
      log(url);
    };

    try {
      return environment
        .copy(url)
        .then(
          () => log(`Copied a link to this grid: ${summary}`, note ? 'warn' : 'info'),
          fallback,
        );
    } catch {
      fallback();
      return;
    }
  }

  const name = requireName(command, 0);
  const grid = workspace.workspace(name);
  if (!grid) throw new UsageError(`There is no workspace called ${name} — WS lists them`);

  const { opened, skipped } = reopen(grid, context, index);
  // The reader's own grid, so its column count is remembered the way `LAY`'s is.
  workspace.setColumns(panels.columns);
  log(
    `Opened workspace ${name}: ${plural(opened, 'panel')}.${skippedNote(skipped)}`,
    skipped.length > 0 ? 'warn' : 'info',
  );
}
