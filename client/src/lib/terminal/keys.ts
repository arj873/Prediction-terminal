/**
 * Key bindings: the preset map, the user's edits, and the router that fires them.
 *
 * A binding is nothing but a chord and a command string, so every key press
 * goes down the same dispatch path as a typed line or a clicked row. That is
 * what keeps the terminal honest: there is no action a key can take that you
 * cannot also type, and none that `HELP` does not already document.
 *
 * Two scopes, because a terminal has two hands:
 *
 *   global — fires anywhere, including mid-word at the prompt. Needs a
 *            modifier (or a key that types nothing), or it would eat your
 *            typing.
 *   panel  — fires only in NAV mode, where the prompt is blurred and the
 *            keyboard belongs to the workspace. Plain letters are fair game
 *            there, which is what makes `j`/`k`/`Enter` possible.
 *
 * Chords are normalised on the way in and on the way out, so `Ctrl+K`,
 * `control+k` and the key press itself all name the same binding. Sequences —
 * `g w`, two chords in a row — are supported, which is where a user with
 * strong opinions puts the bindings this file did not think of.
 *
 * This module owns the key model and nothing else: it reads events it is
 * handed, it never listens for them. Attaching `keydown` is the shell's job
 * (`<svelte:window onkeydown={…}>`), which is also what keeps it testable in
 * Node.
 */

import type { Workspace } from '../state/workspace.svelte';
import { UsageError } from './command';

export type KeyScope = 'global' | 'panel';

/** `cmd` = typing at the prompt. `nav` = the keyboard drives the panels. */
export type KeyMode = 'cmd' | 'nav';

export interface Binding {
  scope: KeyScope;
  /** Normalised chord, or space-separated chords for a sequence. */
  chord: string;
  /** The command line this chord runs. `>` prefixed means "type, don't run". */
  command: string;
  source: 'preset' | 'custom';
  /** What it does, for the KEYS panel. Presets carry one; customs derive theirs. */
  note?: string;
}

/** The shape of a `KeyboardEvent` this module reads — the rest is not its business. */
export interface KeyLike {
  key: string;
  /** Physical key, used to survive Option-produces-ümlauts on macOS. */
  code?: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

/* ------------------------------------------------------------------ chords */

const MODIFIERS: Record<string, string> = {
  ctrl: 'ctrl',
  control: 'ctrl',
  ctl: 'ctrl',
  alt: 'alt',
  option: 'alt',
  opt: 'alt',
  shift: 'shift',
  meta: 'meta',
  cmd: 'meta',
  command: 'meta',
  super: 'meta',
  win: 'meta',
};

/** Modifier order in a normalised chord. Fixed, so two spellings compare equal. */
const MODIFIER_ORDER = ['ctrl', 'alt', 'shift', 'meta'] as const;

const KEY_ALIASES: Record<string, string> = {
  esc: 'escape',
  ret: 'enter',
  return: 'enter',
  del: 'delete',
  ins: 'insert',
  bs: 'backspace',
  up: 'arrowup',
  down: 'arrowdown',
  left: 'arrowleft',
  right: 'arrowright',
  pgup: 'pageup',
  pgdn: 'pagedown',
  pagedn: 'pagedown',
  spc: 'space',
  spacebar: 'space',
  ' ': 'space',
  plus: '+',
  minus: '-',
  slash: '/',
  comma: ',',
  period: '.',
  dot: '.',
};

/** Keys that name themselves rather than typing a character. */
const NAMED_KEYS = new Set([
  'escape',
  'enter',
  'tab',
  'space',
  'backspace',
  'delete',
  'insert',
  'home',
  'end',
  'pageup',
  'pagedown',
  'arrowup',
  'arrowdown',
  'arrowleft',
  'arrowright',
]);

/** Named keys that are safe to bind globally: none of them type anything. */
const SAFE_GLOBAL_KEYS = new Set(['escape', 'insert', 'pageup', 'pagedown', 'home', 'end']);

const FUNCTION_KEY = /^f([1-9]|1[0-9]|2[0-4])$/;

const MODIFIER_KEY_NAMES = new Set(['control', 'alt', 'shift', 'meta', 'altgraph', 'capslock']);

/**
 * Split `ctrl+shift+k` into its parts, treating a trailing `+` as the plus key
 * rather than an empty one — `ctrl++` is a real chord someone will type.
 */
function splitChord(raw: string): string[] {
  const parts: string[] = [];
  let current = '';

  for (const char of raw) {
    if (char === '+' && current !== '') {
      parts.push(current);
      current = '';
      continue;
    }
    current += char;
  }

  parts.push(current === '' ? '+' : current);
  return parts;
}

function normaliseKey(raw: string): string {
  const lower = raw.toLowerCase();
  const alias = KEY_ALIASES[lower];
  const key = alias ?? lower;

  if (key.length === 1 || NAMED_KEYS.has(key) || FUNCTION_KEY.test(key)) return key;
  throw new UsageError(`Unknown key "${raw}"`);
}

/**
 * Normalise one chord.
 *
 * Shift is kept for letters (`Shift+G` is a distinct binding from `g`) and
 * dropped for punctuation and digits, where the shifted character already says
 * it: `?` is `?`, never `Shift+?`. A bare capital letter is read as the
 * lower-case key, because this terminal's own commands are shouted and
 * `KEYS W …` plainly means the `w` key.
 */
export function parseChord(text: string): string {
  const raw = text.trim();
  if (!raw) throw new UsageError('Missing <chord>');

  const parts = splitChord(raw);
  const key = normaliseKey(parts.at(-1) ?? '');

  const mods = new Set<string>();
  for (const part of parts.slice(0, -1)) {
    const modifier = MODIFIERS[part.trim().toLowerCase()];
    if (!modifier) throw new UsageError(`Unknown modifier "${part}" in "${text}"`);
    mods.add(modifier);
  }

  if (key.length === 1 && !/^[a-z]$/.test(key)) mods.delete('shift');

  return [...MODIFIER_ORDER.filter((m) => mods.has(m)), key].join('+');
}

/** Normalise a whole binding — one chord, or a sequence of them. */
export function parseChordSequence(text: string): string {
  const chords = text.trim().split(/\s+/).filter(Boolean);
  if (chords.length === 0) throw new UsageError('Missing <chord>');
  if (chords.length > 3) throw new UsageError('A binding is at most three chords long');
  return chords.map(parseChord).join(' ');
}

/**
 * The chord a key press names, or `null` for a bare modifier press.
 *
 * `event.key` is preferred because it respects the reader's layout, with one
 * exception: on macOS, Option+S produces `ß`, so every `alt+…` binding would be
 * unreachable. When Alt is down the physical key wins for the letter and digit
 * rows, which is exactly the range those bindings live in.
 */
export function chordFromEvent(event: KeyLike): string | null {
  let key = event.key === ' ' ? 'space' : event.key.toLowerCase();
  if (MODIFIER_KEY_NAMES.has(key)) return null;
  if (key === 'dead' || key === 'unidentified') key = '';

  if (event.altKey && event.code) {
    const letter = /^Key([A-Z])$/.exec(event.code);
    const digit = /^Digit([0-9])$/.exec(event.code);
    if (letter) key = letter[1]!.toLowerCase();
    else if (digit) key = digit[1]!;
  }

  if (!key) return null;

  const printable = key.length === 1;
  const mods: string[] = [];
  if (event.ctrlKey) mods.push('ctrl');
  if (event.altKey) mods.push('alt');
  // Same rule as `parseChord`: shift is part of a letter chord and implicit in
  // a punctuation one.
  if (event.shiftKey && (!printable || /^[a-z]$/.test(key))) mods.push('shift');
  if (event.metaKey) mods.push('meta');

  return [...MODIFIER_ORDER.filter((m) => mods.includes(m)), key].join('+');
}

const KEY_LABELS: Record<string, string> = {
  escape: 'Esc',
  enter: 'Enter',
  tab: 'Tab',
  space: 'Space',
  backspace: 'Backspace',
  delete: 'Del',
  insert: 'Ins',
  home: 'Home',
  end: 'End',
  pageup: 'PgUp',
  pagedown: 'PgDn',
  arrowup: '↑',
  arrowdown: '↓',
  arrowleft: '←',
  arrowright: '→',
};

const MODIFIER_LABELS: Record<string, string> = {
  ctrl: 'Ctrl',
  alt: 'Alt',
  shift: 'Shift',
  meta: 'Meta',
};

/** `alt+arrowdown` → `Alt+↓`. Display only; never parsed back. */
export function formatChord(chord: string): string {
  return chord
    .split(' ')
    .map((one) => {
      const parts = splitChord(one);
      const key = parts.at(-1) ?? '';
      return [
        ...parts.slice(0, -1).map((mod) => MODIFIER_LABELS[mod] ?? mod),
        KEY_LABELS[key] ?? key.toUpperCase(),
      ].join('+');
    })
    .join(' ');
}

/**
 * Which scope a chord belongs in when the binding does not say.
 *
 * A chord that types a character has to be `panel`-scoped: bound globally it
 * would fire mid-word at the prompt.
 */
export function inferScope(chord: string): KeyScope {
  const first = chord.split(' ')[0] ?? chord;
  const parts = first.split('+');
  const key = parts.at(-1) ?? '';
  const mods = parts.slice(0, -1);

  if (mods.includes('ctrl') || mods.includes('alt') || mods.includes('meta')) return 'global';
  if (FUNCTION_KEY.test(key) || SAFE_GLOBAL_KEYS.has(key)) return 'global';
  return 'panel';
}

/** Reject a global binding that would swallow ordinary typing. */
function assertBindable(chord: string, scope: KeyScope): void {
  if (scope !== 'global') return;
  if (inferScope(chord) === 'global') return;
  throw new UsageError(
    `${formatChord(chord)} types a character, so it can only be bound in the panel scope`,
  );
}

/* ---------------------------------------------------------------- presets */

/** `[scope, chord, command, note]`, in the order the KEYS panel lists them. */
const PRESET_TABLE: readonly (readonly [KeyScope, string, string, string])[] = [
  /* --- global: these fire even mid-word at the prompt. */
  ['global', 'alt+j', 'FOCUS NAV', 'Hand the keyboard to the panels (NAV mode)'],
  ['global', 'alt+p', 'FOCUS CMD', 'Back to the command line'],
  ['global', 'ctrl+arrowright', 'FOCUS NEXT', 'Focus the next panel'],
  ['global', 'ctrl+arrowleft', 'FOCUS PREV', 'Focus the previous panel'],
  ['global', 'alt+arrowdown', 'ROW NEXT', 'Row cursor down, in the focused panel'],
  ['global', 'alt+arrowup', 'ROW PREV', 'Row cursor up, in the focused panel'],
  ['global', 'alt+enter', 'ROW OPEN', 'Open the row under the cursor'],
  ['global', 'alt+backspace', 'ROW ALT', "The row's second action — remove, open the feed"],
  ['global', 'alt+f', 'ZOOM', 'Maximise the focused panel, or restore the grid'],
  ['global', 'alt+r', 'REFRESH', 'Reload every panel'],
  ['global', 'alt+q', 'CLS', 'Close the focused panel'],
  ['global', 'ctrl+w', 'CLS', 'Close the focused panel — most browsers claim this one'],
  ['global', 'ctrl+l', 'CLR', 'Clear the message log'],
  ['global', 'alt+w', 'W', 'Watchlist'],
  ['global', 'alt+t', 'TOP', 'Leaderboard'],
  ['global', 'alt+n', 'NEWS', 'News wire'],
  ['global', 'alt+x', 'XV', 'Cross-venue prices'],
  ['global', 'alt+k', 'KEYS', 'This key map'],
  ['global', 'alt+h', 'HELP', 'Command reference'],
  ['global', 'f1', 'HELP', 'Command reference'],
  ['global', 'alt+g', '>GP', 'Start a GP command'],
  ['global', 'alt+s', '>SRCH', 'Start a SRCH command'],
  ['global', 'alt+d', '>DES', 'Start a DES command'],
  ['global', 'alt+m', 'MENU', 'Open the menu bar'],
  ['global', 'f10', 'MENU', 'Open the menu bar — where a menu bar has always been'],

  /* --- panel: NAV mode only, so plain letters are free. */
  ['panel', 'j', 'ROW NEXT', 'Row cursor down (or scroll, with no rows)'],
  ['panel', 'arrowdown', 'ROW NEXT', 'Row cursor down'],
  ['panel', 'k', 'ROW PREV', 'Row cursor up'],
  ['panel', 'arrowup', 'ROW PREV', 'Row cursor up'],
  ['panel', 'g', 'ROW TOP', 'First row'],
  ['panel', 'home', 'ROW TOP', 'First row'],
  ['panel', 'shift+g', 'ROW END', 'Last row'],
  ['panel', 'end', 'ROW END', 'Last row'],
  ['panel', 'enter', 'ROW OPEN', 'Open the row under the cursor'],
  ['panel', 'o', 'ROW OPEN', 'Open the row under the cursor'],
  ['panel', 'd', 'ROW ALT', "The row's second action — remove, open the feed"],
  ['panel', 'delete', 'ROW ALT', "The row's second action"],
  ['panel', 'h', 'FOCUS PREV', 'Previous panel'],
  ['panel', 'arrowleft', 'FOCUS PREV', 'Previous panel'],
  ['panel', 'shift+tab', 'FOCUS PREV', 'Previous panel'],
  ['panel', 'l', 'FOCUS NEXT', 'Next panel'],
  ['panel', 'arrowright', 'FOCUS NEXT', 'Next panel'],
  ['panel', 'tab', 'FOCUS NEXT', 'Next panel'],
  ['panel', 'f', 'ZOOM', 'Maximise this panel, or restore the grid'],
  ['panel', 'r', 'REFRESH THIS', 'Reload this panel'],
  ['panel', 'shift+r', 'REFRESH', 'Reload every panel'],
  ['panel', 'x', 'CLS', 'Close this panel'],
  ['panel', '[', 'LAY -', 'One column fewer'],
  ['panel', ']', 'LAY +', 'One column more'],
  ['panel', 'c', 'GP $', 'Chart the row (or panel) under the cursor'],
  ['panel', 'b', 'OB $', 'Order book for it'],
  ['panel', 'q', 'DES $', 'Quote and description for it'],
  ['panel', 's', 'TAS $', 'Time and sales for it'],
  ['panel', 'v', 'XV $', 'Price it at every venue'],
  ['panel', 'a', 'W ADD $', 'Add it to the watchlist'],
  ['panel', 'w', 'W', 'Watchlist'],
  ['panel', 't', 'TOP', 'Leaderboard'],
  ['panel', 'n', 'NEWS', 'News wire'],
  ['panel', 'shift+h', 'HELP', 'Command reference'],
  ['panel', '?', 'KEYS', 'This key map'],
  ['panel', '/', 'FOCUS CMD', 'Back to the command line'],
  ['panel', ':', 'FOCUS CMD', 'Back to the command line'],
  ['panel', 'i', 'FOCUS CMD', 'Back to the command line'],
  ['panel', 'escape', 'FOCUS CMD', 'Back to the command line'],
  ['panel', 'm', 'MENU', 'Open the menu bar'],
];

function buildPresets(): Binding[] {
  const bindings: Binding[] = [];

  for (const [scope, chord, command, note] of PRESET_TABLE) {
    bindings.push({ scope, chord: parseChordSequence(chord), command, note, source: 'preset' });
  }

  // Panel numbers, in both hands: `Alt+3` from the prompt, `3` in NAV mode.
  for (let n = 1; n <= 9; n++) {
    bindings.push({
      scope: 'global',
      chord: `alt+${n}`,
      command: `FOCUS ${n}`,
      note: `Focus panel ${n}`,
      source: 'preset',
    });
    bindings.push({
      scope: 'panel',
      chord: String(n),
      command: `FOCUS ${n}`,
      note: `Focus panel ${n}`,
      source: 'preset',
    });
  }

  return bindings;
}

/**
 * The bindings the terminal ships with.
 *
 * Built once, through the same parser a typed binding goes through, so a
 * malformed preset fails at boot rather than silently never firing.
 */
export const PRESET_BINDINGS: readonly Binding[] = buildPresets();

/* ---------------------------------------------------------------- storage */

/** Storage key for one binding. The scope is part of it: `j` differs by scope. */
function storageKey(scope: KeyScope, chord: string): string {
  return `${scope}:${chord}`;
}

function splitStorageKey(key: string): { scope: KeyScope; chord: string } | null {
  const colon = key.indexOf(':');
  if (colon === -1) return null;
  const scope = key.slice(0, colon);
  const chord = key.slice(colon + 1);
  if ((scope !== 'global' && scope !== 'panel') || !chord) return null;
  return { scope, chord };
}

/**
 * Presets, plus the reader's edits on top.
 *
 * Edits are stored as a sparse overlay rather than a copy of the whole map, so
 * a preset added in a later version reaches someone who has customised three
 * keys. An empty command means "this preset is switched off" — which is how a
 * preset gets removed without the storage forgetting that it was.
 */
export class Keymap {
  readonly #workspace: Workspace;
  #cache: { list: Binding[]; index: Map<string, Binding>; prefixes: Set<string> } | undefined;

  constructor(workspace: Workspace) {
    this.#workspace = workspace;
    workspace.subscribe(() => {
      this.#cache = undefined;
    });
  }

  #build(): { list: Binding[]; index: Map<string, Binding>; prefixes: Set<string> } {
    if (this.#cache) return this.#cache;

    const merged = new Map<string, Binding>();
    for (const preset of PRESET_BINDINGS) {
      merged.set(storageKey(preset.scope, preset.chord), { ...preset });
    }

    for (const [key, command] of Object.entries(this.#workspace.keybindings)) {
      const parsed = splitStorageKey(key);
      if (!parsed) continue;

      if (!command) {
        merged.delete(key);
        continue;
      }

      merged.set(key, {
        scope: parsed.scope,
        chord: parsed.chord,
        command,
        source: 'custom',
      });
    }

    const list = [...merged.values()];
    const index = new Map<string, Binding>();
    const prefixes = new Set<string>();

    for (const binding of list) {
      index.set(storageKey(binding.scope, binding.chord), binding);
      // `g w` makes `g` a prefix, so a `g` press has to wait to find out which
      // binding it is.
      const chords = binding.chord.split(' ');
      for (let i = 1; i < chords.length; i++) {
        prefixes.add(storageKey(binding.scope, chords.slice(0, i).join(' ')));
      }
    }

    this.#cache = { list, index, prefixes };
    return this.#cache;
  }

  /** Every live binding, presets first, in the order the KEYS panel shows them. */
  list(): Binding[] {
    return this.#build().list;
  }

  find(scope: KeyScope, chord: string): Binding | undefined {
    return this.#build().index.get(storageKey(scope, chord));
  }

  /** True when a longer binding in this scope starts with `chord`. */
  isPrefix(scope: KeyScope, chord: string): boolean {
    return this.#build().prefixes.has(storageKey(scope, chord));
  }

  /** Look a chord up as typed, in one scope or either. */
  describe(chordText: string, scope?: KeyScope): Binding | undefined {
    const chord = parseChordSequence(chordText);
    if (scope) return this.find(scope, chord);
    return this.find('global', chord) ?? this.find('panel', chord);
  }

  /**
   * Bind a chord.
   *
   * A binding that restores a preset's own command drops the override instead
   * of storing a copy of it, so `KEYS RESET` stays meaningful and the stored
   * overlay only ever holds real differences.
   */
  set(chordText: string, command: string, scope?: KeyScope): Binding {
    const chord = parseChordSequence(chordText);
    const resolved = scope ?? inferScope(chord);
    assertBindable(chord, resolved);

    const trimmed = command.trim();
    if (!trimmed) throw new UsageError('Missing <command>');

    const key = storageKey(resolved, chord);
    const preset = PRESET_BINDINGS.find((b) => storageKey(b.scope, b.chord) === key);

    if (preset && preset.command === trimmed) {
      this.#workspace.deleteBinding(key);
      return { ...preset };
    }

    this.#workspace.setBinding(key, trimmed);
    return { scope: resolved, chord, command: trimmed, source: 'custom' };
  }

  /**
   * Unbind a chord.
   *
   * Switching a preset off has to be recorded — deleting the overlay entry
   * would just let the preset come back on the next load.
   */
  remove(chordText: string, scope?: KeyScope): Binding | undefined {
    const chord = parseChordSequence(chordText);
    const scopes: KeyScope[] = scope ? [scope] : ['global', 'panel'];

    for (const candidate of scopes) {
      const binding = this.find(candidate, chord);
      if (!binding) continue;

      const key = storageKey(candidate, chord);
      if (binding.source === 'preset') this.#workspace.setBinding(key, '');
      else {
        this.#workspace.deleteBinding(key);
        // A custom binding may have been sitting on top of a preset; the preset
        // underneath it is not what "unbind" meant.
        if (this.find(candidate, chord)) this.#workspace.setBinding(key, '');
      }
      return binding;
    }

    return undefined;
  }

  /** Drop every edit, presets included. Returns how many were forgotten. */
  reset(): number {
    return this.#workspace.resetBindings();
  }

  /** How many bindings the reader has changed. */
  get customCount(): number {
    return Object.keys(this.#workspace.keybindings).length;
  }
}

/* ------------------------------------------------------------ substitution */

/**
 * Fill `$` from the focused panel — the row under the cursor, or the panel's
 * own subject.
 *
 * This is what makes one binding useful in every panel: `b` is `OB $`, and what
 * `$` means is wherever you are looking. Only a whole `$` token is replaced, so
 * `SRCH $100 bill` is left alone.
 *
 * Returns `null` when the command wants a subject and there is none, which the
 * caller reports rather than running a command with a literal `$` in it.
 */
/**
 * What to say when `$` has nothing to stand for.
 *
 * Named once because three places say it — the router, the menu's hint strip
 * and the log line a disabled entry prints — and a reader who meets the same
 * sentence in all three learns the rule once instead of three times.
 */
export const SUBJECT_HINT =
  'needs a market: put the row cursor on one, or focus a panel that has one.';

export function applySubject(command: string, subject: string | undefined): string | null {
  if (!command.includes('$')) return command;

  let missing = false;
  const filled = command
    .split(/(\s+)/)
    .map((part) => {
      if (part !== '$') return part;
      if (!subject) {
        missing = true;
        return part;
      }
      return subject;
    })
    .join('');

  return missing ? null : filled;
}

/* ----------------------------------------------------------------- router */

/** True when the key press belongs to a text field and nothing else. */
export function isTypingTarget(target: EventTarget | null): boolean {
  const node = target as (HTMLElement & { tagName?: string }) | null;
  if (!node) return false;
  return node.tagName === 'INPUT' || node.tagName === 'TEXTAREA' || node.isContentEditable === true;
}

/**
 * What the router needs off a key press.
 *
 * A real `KeyboardEvent` satisfies this, and so does an object literal, which
 * is how the router is exercised without a DOM.
 */
export interface RoutableKeyEvent extends KeyLike {
  target?: EventTarget | null;
  preventDefault(): void;
}

export interface KeyRouterOptions {
  keymap: Keymap;
  /** Dispatch a command line, exactly as if it had been typed. */
  run(command: string): void;
  log(message: string, level?: 'info' | 'warn' | 'error'): void;
  /** `$` — the row under the cursor, or the focused panel's own subject. */
  subject(): string | undefined;
  focusPrompt(): void;
  blurPrompt(): void;
  /** Give the workspace keyboard focus. False when there is no panel to focus. */
  focusWorkspace(): boolean;
  /**
   * Did the command that just ran take the keyboard deliberately?
   *
   * NAV mode hands focus back to the workspace after a binding fires, which is
   * right for a binding that opened a panel and wrong for one that opened the
   * menu — the menu asked for the keyboard, and snatching it back on the way
   * out would close the thing the key was pressed to open.
   */
  keyboardClaimed?(): boolean;
  onMode?(mode: KeyMode): void;
}

/** How long a sequence prefix waits for its second chord. */
const SEQUENCE_MS = 700;

/**
 * The routing half of the key model: it decides what a press means, and the
 * caller decides where presses come from.
 *
 * Deliberately no `attach(window)`. The shell owns the listener
 * (`<svelte:window onkeydown={(e) => router.handle(e)}>`) so that nothing here
 * runs at import time and every branch below can be driven from a test.
 */
export class KeyRouter {
  readonly #options: KeyRouterOptions;
  #mode: KeyMode = 'cmd';
  #pending: { chord: string; fallback: Binding | undefined } | undefined;
  #timer: ReturnType<typeof setTimeout> | undefined;

  constructor(options: KeyRouterOptions) {
    this.#options = options;
  }

  get mode(): KeyMode {
    return this.#mode;
  }

  /**
   * Enter or leave NAV mode.
   *
   * NAV blurs the prompt — otherwise every unbound letter would land in it —
   * and hands keyboard focus to the focused panel, so the browser scrolls the
   * thing the reader is looking at.
   */
  setMode(mode: KeyMode): void {
    if (mode === 'nav') {
      if (!this.#options.focusWorkspace()) {
        this.#options.log('No panel to navigate. Open one first.', 'warn');
        return;
      }
      this.#options.blurPrompt();
    } else {
      this.#options.focusPrompt();
    }

    if (this.#mode === mode) return;
    this.#mode = mode;
    this.#clearPending();
    this.#options.onMode?.(mode);
  }

  handle(event: RoutableKeyEvent): void {
    const chord = chordFromEvent(event);
    if (chord === null) return;

    const typing = isTypingTarget(event.target ?? null);
    // Panel bindings are plain letters; they only make sense when nothing is
    // waiting for typed input.
    const scopes: KeyScope[] = this.#mode === 'nav' && !typing ? ['panel', 'global'] : ['global'];

    const sequence = this.#pending ? `${this.#pending.chord} ${chord}` : chord;

    let exact: Binding | undefined;
    let prefix = false;
    for (const scope of scopes) {
      exact ??= this.#options.keymap.find(scope, sequence);
      prefix ||= this.#options.keymap.isPrefix(scope, sequence);
    }

    if (prefix) {
      event.preventDefault();
      this.#pend(sequence, exact);
      return;
    }

    if (exact) {
      event.preventDefault();
      this.#clearPending();
      this.#fire(exact.command);
      return;
    }

    if (this.#pending) {
      // A sequence that went nowhere. Swallow the key rather than let half of
      // it act on its own.
      event.preventDefault();
      this.#clearPending();
      return;
    }

    this.#fallthrough(event, typing);
  }

  /**
   * Nothing was bound.
   *
   * A printable key with the workspace focused starts a command — the terminal
   * has always done this, and it is what makes NAV mode a soft mode: the letters
   * it does not claim still take you to the prompt with the character intact.
   * Read from `event.key` rather than the chord, so a capital letter counts as
   * the one character it types rather than as `shift+` something.
   */
  #fallthrough(event: RoutableKeyEvent, typing: boolean): void {
    if (typing) return;
    if (event.key.length !== 1) return;
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    this.setMode('cmd');
  }

  #pend(chord: string, fallback: Binding | undefined): void {
    this.#clearPending();
    this.#pending = { chord, fallback };
    this.#timer = setTimeout(() => {
      const pending = this.#pending;
      this.#pending = undefined;
      this.#timer = undefined;
      // The prefix is a binding in its own right (`g` is ROW TOP as well as the
      // start of `g w`), so a pause means the shorter one was meant.
      if (pending?.fallback) this.#fire(pending.fallback.command);
    }, SEQUENCE_MS);
  }

  #clearPending(): void {
    if (this.#timer !== undefined) clearTimeout(this.#timer);
    this.#timer = undefined;
    this.#pending = undefined;
  }

  #fire(command: string): void {
    const resolved = applySubject(command, this.#options.subject());
    if (resolved === null) {
      this.#options.log(`${command} ${SUBJECT_HINT}`, 'warn');
      return;
    }

    this.#options.run(resolved);

    // Opening a panel from NAV mode should leave the keyboard in NAV mode, on
    // the panel that just opened. Two exceptions, both of which asked for
    // somewhere else: a binding that types at the prompt, and a command that
    // took the keyboard for itself.
    if (this.#mode === 'nav' && !resolved.startsWith('>') && !this.#options.keyboardClaimed?.()) {
      this.#options.focusWorkspace();
    }
  }
}
