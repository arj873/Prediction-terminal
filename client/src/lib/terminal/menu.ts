/**
 * The menu tree: what the top bar offers, and how it stays true.
 *
 * Nothing here describes what a command *does* — that is the command table's
 * job, and saying it twice is how the two would come to disagree. A menu entry
 * names a command line and a label for the thing you are trying to accomplish;
 * the usage, the summary and the key that runs it are all looked up live. So a
 * verb renamed in `commands.ts`, or a key rebound with `KEYS`, changes what the
 * menu says without anyone editing this file.
 *
 * The table below is the curated half — which commands are worth a click, in
 * what order, under which heading. The exhaustive half is generated: the LEARN
 * menu carries every command in the table, so "everything is reachable from the
 * bar" is a property of the code rather than a promise someone has to keep
 * updating.
 */

import { THEMES } from '../state/workspace.svelte';
import type { Command } from './command';
import { COMMAND_INDEX, COMMANDS } from './commands';
import { formatChord, type Binding, type Keymap } from './keys';
import { ENT_GENRES } from './registry';

/** A leaf: one command line, one click. */
export interface MenuItem {
  kind: 'item';
  label: string;
  /** The command line this entry runs. A leading `>` types it instead. */
  command: string;
  /** Overrides the verb's own summary, for an entry narrower than its verb. */
  note?: string;
}

/** A branch: opens a flyout of its own, to any depth. */
export interface MenuGroup {
  kind: 'group';
  label: string;
  entries: MenuEntry[];
}

/** A rule between sections, optionally carrying a caption for the one below. */
export interface MenuSeparator {
  kind: 'separator';
  label?: string;
}

export type MenuEntry = MenuItem | MenuGroup | MenuSeparator;

export interface Menu {
  /** The title on the bar, and the name `MENU <name>` takes. */
  title: string;
  /** Shown under the list until something is highlighted. */
  hint: string;
  entries: MenuEntry[];
}

/* ------------------------------------------------------------- constructors */

function item(label: string, command: string, note?: string): MenuItem {
  return note === undefined
    ? { kind: 'item', label, command }
    : { kind: 'item', label, command, note };
}

function group(label: string, entries: MenuEntry[]): MenuGroup {
  return { kind: 'group', label, entries };
}

const SEP: MenuSeparator = { kind: 'separator' };

function caption(label: string): MenuSeparator {
  return { kind: 'separator', label };
}

/* ---------------------------------------------------------------- reading */

/** The verb a command line names, `>` prefix and arguments stripped. */
export function verbOf(command: string): string {
  const line = command.startsWith('>') ? command.slice(1) : command;
  return line.trim().split(/\s+/)[0]?.toUpperCase() ?? '';
}

/** The command-table entry behind an entry's command, when it names one. */
export function commandOf(entry: MenuItem): Command | undefined {
  return COMMAND_INDEX.get(verbOf(entry.command));
}

/**
 * Split a usage line on its top-level `|`.
 *
 * `W | W ADD <ticker>` is two ways to say `W`; `GP <t> [1m|1h|1d]` is one way
 * with a choice inside it. Only the bars outside every bracket separate forms,
 * so the depth has to be counted rather than the string split.
 */
export function usageAlternatives(usage: string): string[] {
  const forms: string[] = [];
  let depth = 0;
  let current = '';

  for (const char of usage) {
    if (char === '<' || char === '[') depth++;
    else if (char === '>' || char === ']') depth = Math.max(0, depth - 1);
    else if (char === '|' && depth === 0) {
      forms.push(current.trim());
      current = '';
      continue;
    }
    current += char;
  }

  forms.push(current.trim());
  return forms.filter(Boolean);
}

/**
 * Does the plainest form of this command need an argument?
 *
 * This is what decides whether a menu entry runs or types. `TOP` opens a
 * leaderboard on its own, so the menu runs it; `GP` without a ticker is only an
 * error message, so the menu types `GP ` at the prompt and leaves the reader
 * there — which is also the moment the command line shows them its usage.
 *
 * A `<required>` group counts only outside every `[optional]` one: `SPOT
 * [global|<country>]` names an argument it does not need.
 */
export function needsArgument(usage: string): boolean {
  const simplest = usageAlternatives(usage)[0] ?? usage;
  let optional = 0;

  for (const char of simplest) {
    if (char === '[') optional++;
    else if (char === ']') optional = Math.max(0, optional - 1);
    else if (char === '<' && optional === 0) return true;
  }

  return false;
}

/** How the menu invokes a command with nothing else to go on. */
export function defaultInvocation(command: Command): string {
  return needsArgument(command.usage) ? `>${command.verb}` : command.verb;
}

/** Two command lines are the same binding if they differ only in spacing or case. */
function sameCommand(a: string, b: string): boolean {
  return (
    a.trim().replace(/\s+/g, ' ').toUpperCase() === b.trim().replace(/\s+/g, ' ').toUpperCase()
  );
}

/**
 * The bindings that run exactly this command line.
 *
 * Exactly, deliberately: `Alt+T` runs `TOP`, and printing it against `TOP
 * gainers` would teach a key that opens the wrong leaderboard. Global bindings
 * come first — they are the ones that work while you are typing.
 */
export function bindingsFor(keymap: Keymap, command: string): Binding[] {
  const matches = keymap.list().filter((binding) => sameCommand(binding.command, command));
  matches.sort((a, b) => (a.scope === b.scope ? 0 : a.scope === 'global' ? -1 : 1));
  return matches;
}

/**
 * A chord, labelled with the half of the keyboard it belongs to.
 *
 * A panel binding is a plain letter that only fires in NAV mode; printed on its
 * own next to `Alt+T` it reads as a key you could press at the prompt, where it
 * would do nothing but type itself. `NAV` is three characters and saves the
 * reader that discovery.
 */
export function chordLabel(binding: Binding): string {
  const chord = formatChord(binding.chord);
  return binding.scope === 'panel' ? `NAV ${chord}` : chord;
}

/** Find a menu by title, in full or by prefix — `MENU mark` opens MARKETS. */
export function findMenu(menus: readonly Menu[], name: string): number {
  const wanted = name.trim().toUpperCase();
  if (!wanted) return -1;
  const exact = menus.findIndex((menu) => menu.title === wanted);
  return exact === -1 ? menus.findIndex((menu) => menu.title.startsWith(wanted)) : exact;
}

/** Walk every entry in a tree, branches included. */
export function walkEntries(entries: readonly MenuEntry[]): MenuEntry[] {
  const flat: MenuEntry[] = [];
  for (const entry of entries) {
    flat.push(entry);
    if (entry.kind === 'group') flat.push(...walkEntries(entry.entries));
  }
  return flat;
}

/* -------------------------------------------------------------- generated */

/** Every command in one table group, each invoked the way it prefers. */
function commandsIn(name: Command['group']): MenuEntry[] {
  return COMMANDS.filter((command) => command.group === name).map((command) =>
    item(command.summary, defaultInvocation(command)),
  );
}

/**
 * A verb's worked examples, straight from the command table.
 *
 * The example *is* the label: these entries exist to be read as much as
 * clicked, and rewriting `GP KXFEDDECISION-27JAN-H26 1d 1y` into prose would
 * hide the only part that teaches anything.
 */
function examplesOf(verb: string): MenuEntry[] {
  return (COMMAND_INDEX.get(verb)?.examples ?? []).map((example) => item(example, example));
}

/** The verbs whose examples the LEARN menu offers. Checked against the table. */
export const EXAMPLE_VERBS: readonly string[] = [
  'SRCH',
  'GP',
  'XV',
  'IMP',
  'STK',
  'NEWS',
  'FRED',
  'BB',
];

function titleCase(word: string): string {
  return word.charAt(0).toUpperCase() + word.slice(1);
}

/* ------------------------------------------------------------------ table */

export const MENUS: readonly Menu[] = [
  {
    title: 'MARKETS',
    hint: 'Prediction markets — Kalshi, Polymarket, Polymarket US',
    entries: [
      item('Search every venue at once…', '>SRCH'),
      item('What more than one broker lists', 'XV'),
      item('Leaderboard', 'TOP'),
      group('Leaderboard by…', [
        item('Volume', 'TOP volume'),
        item('Gainers', 'TOP gainers'),
        item('Losers', 'TOP losers'),
        item('Open interest', 'TOP oi'),
        item('Liquidity', 'TOP liquidity'),
        SEP,
        item('Kalshi only', 'TOP volume kalshi'),
        item('Polymarket only', 'TOP volume pm'),
        item('Polymarket US only', 'TOP volume pmus'),
      ]),
      item('Watchlist', 'W'),
      item('Every contract in an event…', '>EVT'),
      group('Entertainment markets', [
        item('Every genre', 'ENT'),
        SEP,
        ...ENT_GENRES.map((genre) => item(titleCase(genre), `ENT ${genre}`)),
      ]),
      caption('THE MARKET UNDER THE ROW CURSOR — this is what $ means'),
      item('Chart it', 'GP $'),
      item('Quote and description', 'DES $'),
      item('Order book', 'OB $'),
      item('Time and sales', 'TAS $'),
      item('Price it at every venue', 'XV $'),
      item('Add it to the watchlist', 'W ADD $'),
    ],
  },
  {
    title: 'PRICES',
    hint: 'Stocks, crypto, and the price Kalshi implies for them',
    entries: [
      item('Stock, ETF or index chart…', '>STK'),
      item('Crypto chart…', '>CRY'),
      item("Kalshi's implied price, over the real one…", '>IMP'),
      SEP,
      group('Chart the market under the cursor', [
        item('Default — hourly bars', 'GP $'),
        SEP,
        item('Minute bars, six hours', 'GP $ 1m 6h'),
        item('Hourly bars, seven days', 'GP $ 1h 7d'),
        item('Daily bars, one year', 'GP $ 1d 1y'),
        item('A line instead of candles', 'GP $ 1h 7d line'),
      ]),
      group('Implied price', examplesOf('IMP')),
      group('Stocks and crypto', [...examplesOf('STK'), SEP, ...examplesOf('CRY')]),
    ],
  },
  {
    title: 'DATA',
    hint: 'The wire, the economy, and the feeds these markets settle against',
    entries: [
      item('News wire', 'NEWS'),
      item('News for a symbol…', '>NEWS'),
      SEP,
      item('FRED economic series…', '>FRED'),
      item('Search FRED for a series id…', '>FSRCH'),
      group('FRED examples', examplesOf('FRED')),
      SEP,
      group('Charts and box office', [
        item('Billboard Hot 100', 'BB'),
        item('Every Billboard chart', 'BB CHARTS'),
        item('Domestic box office', 'BO'),
        item('Netflix Top 10', 'NFLX'),
        item('Netflix films', 'NFLX films'),
        item('Spotify streams', 'SPOT'),
        item('YouTube music videos', 'YT'),
      ]),
      group('Screens and scores', [
        item('Rotten Tomatoes score…', '>RT'),
        item('Search Rotten Tomatoes…', '>RT SEARCH'),
        item('Steam concurrent players', 'STEAM'),
        item("Today's TV schedule", 'TV'),
      ]),
    ],
  },
  {
    title: 'WORKSPACE',
    hint: 'Panels, layout and the colour scheme',
    entries: [
      item('Maximise the focused panel', 'ZOOM'),
      item('Close the focused panel', 'CLS'),
      item('Close every panel', 'CLS ALL'),
      SEP,
      item('Reload the focused panel', 'REFRESH THIS'),
      item('Reload every panel', 'REFRESH'),
      item('Clear the message log', 'CLR'),
      SEP,
      group('Columns', [
        item('One column', 'LAY 1'),
        item('Two columns', 'LAY 2'),
        item('Three columns', 'LAY 3'),
        item('Four columns', 'LAY 4'),
        SEP,
        item('One fewer', 'LAY -'),
        item('One more', 'LAY +'),
      ]),
      group(
        'Theme',
        THEMES.map((theme) => item(titleCase(theme), `THEME ${theme}`)),
      ),
      SEP,
      group('Panel focus', [
        item('Next panel', 'FOCUS NEXT'),
        item('Previous panel', 'FOCUS PREV'),
        item('The last panel opened', 'FOCUS LAST'),
        SEP,
        item('Hand the keyboard to the panels (NAV)', 'FOCUS NAV'),
        item('Back to the command line', 'FOCUS CMD'),
      ]),
      group('Row cursor', [
        item('Down a row', 'ROW NEXT'),
        item('Up a row', 'ROW PREV'),
        item('First row', 'ROW TOP'),
        item('Last row', 'ROW END'),
        SEP,
        item('Open the row under the cursor', 'ROW OPEN'),
        item("The row's second action", 'ROW ALT'),
      ]),
    ],
  },
  {
    title: 'LEARN',
    hint: 'Every command, every key, and what the two have to do with each other',
    entries: [
      item('Command reference', 'HELP'),
      item('Key map', 'KEYS'),
      SEP,
      group('Every command', [
        group('Prediction markets', commandsIn('markets')),
        group('Data sources', commandsIn('data')),
        group('Workspace', commandsIn('workspace')),
      ]),
      group(
        'Worked examples',
        EXAMPLE_VERBS.map((verb) => group(verb, examplesOf(verb))),
      ),
      SEP,
      caption('THE THREE THINGS WORTH KNOWING'),
      item(
        'A venue prefix picks the exchange: pm: and pmus:',
        'HELP DES',
        'An unprefixed ticker is Kalshi. pm: takes a Polymarket slug, pmus: a Polymarket US one.',
      ),
      item(
        '$ is whatever you are looking at',
        'KEYS',
        'In a key binding or a menu entry, $ is the row under the cursor — or, failing that, the market the focused panel is about.',
      ),
      item(
        'NAV mode gives the letters to the panels',
        'FOCUS NAV',
        'Esc on an empty command line hands the keyboard to the workspace, where j and k walk rows and c charts one. / gives it back.',
      ),
      SEP,
      item('Bind a key of your own…', '>KEYS'),
      item('Forget my key edits', 'KEYS RESET'),
    ],
  },
];
