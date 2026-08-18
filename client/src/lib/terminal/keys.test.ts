/**
 * Key binding tests.
 *
 * Two things here are worth pinning down, and they are both about a key press
 * and a typed binding having to name the same chord. The first is
 * normalisation: `Ctrl+K`, `control+k` and the press itself all have to reduce
 * to one string, or a binding is written down and then never fires. The second
 * is the overlay: presets and the reader's edits are stored separately, and the
 * merge is what decides whether switching a preset off actually sticks across a
 * reload.
 *
 * The preset table is checked against the command registry for the same reason
 * the registry checks itself for duplicate verbs — a preset that names a
 * command nobody wrote is a key that reports "Unknown command" under the
 * finger, and there is no reason for that to reach a reader.
 */

import { describe, expect, it } from 'vitest';
import {
  PRESET_BINDINGS,
  applySubject,
  chordFromEvent,
  formatChord,
  inferScope,
  isTypingTarget,
  Keymap,
  parseChord,
  parseChordSequence,
} from './keys';
import { Workspace } from '../state/workspace.svelte';

/**
 * The verbs the presets reach for.
 *
 * TODO(registry): this stands in for `COMMAND_INDEX` from `terminal/registry.ts`,
 * which has not been ported yet. When it lands, import it and check
 * `COMMAND_INDEX.has(verb)` instead — the point of the test is that the
 * registry, not this list, is what the keys are checked against.
 */
const COMMAND_INDEX = new Set([
  'CLR',
  'CLS',
  'DES',
  'FOCUS',
  'GP',
  'HELP',
  'KEYS',
  'LAY',
  'NEWS',
  'OB',
  'REFRESH',
  'ROW',
  'SRCH',
  'TAS',
  'TOP',
  'W',
  'XV',
  'ZOOM',
]);

/** A key press, with the modifiers nobody set defaulted to false. */
type Modifiers = Partial<Record<'ctrlKey' | 'altKey' | 'shiftKey' | 'metaKey', boolean>>;

function press(key: string, modifiers: Modifiers & { code?: string } = {}) {
  return {
    key,
    ...(modifiers.code ? { code: modifiers.code } : {}),
    ctrlKey: modifiers['ctrlKey'] === true,
    altKey: modifiers['altKey'] === true,
    shiftKey: modifiers['shiftKey'] === true,
    metaKey: modifiers['metaKey'] === true,
  };
}

describe('parseChord', () => {
  it('normalises modifier spelling and order', () => {
    expect(parseChord('Control+Alt+k')).toBe('ctrl+alt+k');
    expect(parseChord('alt+ctrl+k')).toBe('ctrl+alt+k');
    expect(parseChord('cmd+k')).toBe('meta+k');
    expect(parseChord('option+k')).toBe('alt+k');
  });

  it('reads a capital letter as the key, not as Shift', () => {
    // Every command in this terminal is shouted, so `KEYS W …` plainly means
    // the `w` key. Shift has to be asked for.
    expect(parseChord('W')).toBe('w');
    expect(parseChord('shift+w')).toBe('shift+w');
  });

  it('drops Shift from a character that already carries it', () => {
    expect(parseChord('shift+?')).toBe('?');
    expect(parseChord('shift+1')).toBe('1');
  });

  it('accepts named keys and their short forms', () => {
    expect(parseChord('Esc')).toBe('escape');
    expect(parseChord('alt+up')).toBe('alt+arrowup');
    expect(parseChord('PgDn')).toBe('pagedown');
    expect(parseChord('F4')).toBe('f4');
  });

  it('reads a trailing plus as the plus key', () => {
    expect(parseChord('ctrl++')).toBe('ctrl++');
    expect(parseChord('+')).toBe('+');
  });

  it('refuses a key or modifier it does not know', () => {
    expect(() => parseChord('hyper+k')).toThrow(/Unknown modifier/);
    expect(() => parseChord('ctrl+wibble')).toThrow(/Unknown key/);
    expect(() => parseChord('   ')).toThrow(/Missing <chord>/);
  });
});

describe('parseChordSequence', () => {
  it('normalises every chord in a sequence', () => {
    expect(parseChordSequence('G  Shift+W')).toBe('g shift+w');
  });

  it('refuses a sequence nobody could remember', () => {
    expect(() => parseChordSequence('a b c d')).toThrow(/at most three chords/);
  });
});

describe('chordFromEvent', () => {
  it('names the same chord a typed binding does', () => {
    expect(chordFromEvent(press('k', { ctrlKey: true }))).toBe('ctrl+k');
    expect(chordFromEvent(press('ArrowDown', { altKey: true }))).toBe('alt+arrowdown');
    expect(chordFromEvent(press(' '))).toBe('space');
    expect(chordFromEvent(press('G', { shiftKey: true }))).toBe('shift+g');
    expect(chordFromEvent(press('?', { shiftKey: true }))).toBe('?');
  });

  it('ignores a bare modifier press', () => {
    expect(chordFromEvent(press('Shift', { shiftKey: true }))).toBe(null);
    expect(chordFromEvent(press('Control', { ctrlKey: true }))).toBe(null);
  });

  it('reads the physical key when Alt is down', () => {
    // macOS turns Option+S into `ß`, which would make every alt+letter preset
    // unreachable on that keyboard.
    expect(chordFromEvent(press('ß', { altKey: true, code: 'KeyS' }))).toBe('alt+s');
    expect(chordFromEvent(press('¡', { altKey: true, code: 'Digit1' }))).toBe('alt+1');
  });
});

describe('formatChord', () => {
  it('renders a chord the way a key map reads', () => {
    expect(formatChord('alt+arrowdown')).toBe('Alt+↓');
    expect(formatChord('ctrl+shift+k')).toBe('Ctrl+Shift+K');
    expect(formatChord('escape')).toBe('Esc');
    expect(formatChord('g w')).toBe('G W');
    expect(formatChord('ctrl++')).toBe('Ctrl++');
  });
});

describe('inferScope', () => {
  it('keeps a typing key out of the global scope', () => {
    expect(inferScope('j')).toBe('panel');
    expect(inferScope('shift+g')).toBe('panel');
    expect(inferScope('enter')).toBe('panel');
    expect(inferScope('tab')).toBe('panel');
  });

  it('claims chords that type nothing', () => {
    expect(inferScope('alt+s')).toBe('global');
    expect(inferScope('ctrl+arrowleft')).toBe('global');
    expect(inferScope('f4')).toBe('global');
    expect(inferScope('escape')).toBe('global');
  });

  it('reads a sequence by its first chord', () => {
    expect(inferScope('g w')).toBe('panel');
    expect(inferScope('alt+g w')).toBe('global');
  });
});

describe('applySubject', () => {
  it('fills a whole $ token', () => {
    expect(applySubject('OB $', 'KXFED-1')).toBe('OB KXFED-1');
    expect(applySubject('W ADD $', 'pm:slug')).toBe('W ADD pm:slug');
  });

  it('leaves a $ that is part of a word alone', () => {
    expect(applySubject('SRCH $100 bill', undefined)).toBe('SRCH $100 bill');
  });

  it('refuses to run a command whose subject is missing', () => {
    expect(applySubject('OB $', undefined)).toBe(null);
  });

  it('passes a command with no $ straight through', () => {
    expect(applySubject('TOP volume', undefined)).toBe('TOP volume');
  });
});

describe('isTypingTarget', () => {
  it('claims text fields and nothing else', () => {
    expect(isTypingTarget({ tagName: 'INPUT' } as unknown as EventTarget)).toBe(true);
    expect(isTypingTarget({ tagName: 'TEXTAREA' } as unknown as EventTarget)).toBe(true);
    expect(
      isTypingTarget({ tagName: 'DIV', isContentEditable: true } as unknown as EventTarget),
    ).toBe(true);
    expect(isTypingTarget({ tagName: 'SECTION' } as unknown as EventTarget)).toBe(false);
    expect(isTypingTarget(null)).toBe(false);
  });
});

describe('preset bindings', () => {
  it('runs commands the terminal actually has', () => {
    for (const binding of PRESET_BINDINGS) {
      const line = binding.command.startsWith('>') ? binding.command.slice(1) : binding.command;
      const verb = line.trim().split(/\s+/)[0]?.toUpperCase() ?? '';
      if (!verb) continue;
      expect(
        COMMAND_INDEX.has(verb),
        `${binding.chord} runs "${binding.command}", but ${verb} is not a command`,
      ).toBe(true);
    }
  });

  it('binds each chord once per scope', () => {
    const seen = new Set<string>();
    for (const binding of PRESET_BINDINGS) {
      const key = `${binding.scope}:${binding.chord}`;
      expect(!seen.has(key), `${key} is bound twice`).toBe(true);
      seen.add(key);
    }
  });

  it('never binds a typing key globally', () => {
    for (const binding of PRESET_BINDINGS) {
      if (binding.scope !== 'global') continue;
      expect(inferScope(binding.chord), `${binding.chord} would fire mid-word at the prompt`).toBe(
        'global',
      );
    }
  });

  it('ships every chord already normalised', () => {
    for (const binding of PRESET_BINDINGS) {
      expect(parseChordSequence(binding.chord)).toBe(binding.chord);
    }
  });
});

describe('Keymap', () => {
  it('resolves a preset', () => {
    const keys = new Keymap(new Workspace());
    expect(keys.find('panel', 'j')?.command).toBe('ROW NEXT');
    expect(keys.find('global', 'alt+w')?.command).toBe('W');
    // Scope is part of the identity: `j` is not a global binding.
    expect(keys.find('global', 'j')).toBe(undefined);
  });

  it('lets a custom binding replace a preset', () => {
    const keys = new Keymap(new Workspace());
    keys.set('alt+w', 'TOP gainers');

    const bound = keys.find('global', 'alt+w');
    expect(bound?.command).toBe('TOP gainers');
    expect(bound?.source).toBe('custom');
  });

  it('infers the scope from the chord, and takes an override', () => {
    const keys = new Keymap(new Workspace());

    expect(keys.set('alt+b', 'OB $').scope).toBe('global');
    expect(keys.set('y', 'W').scope).toBe('panel');
    expect(keys.set('alt+y', 'W', 'panel').scope).toBe('panel');
  });

  it('refuses a global binding that would eat typing', () => {
    const keys = new Keymap(new Workspace());
    expect(() => keys.set('y', 'W', 'global')).toThrow(/types a character/);
  });

  it('keeps a preset switched off across a reload', () => {
    const workspace = new Workspace();
    const keys = new Keymap(workspace);

    keys.remove('j');
    expect(keys.find('panel', 'j')).toBe(undefined);

    // The overlay has to carry the removal, not just the absence of an edit —
    // an absent entry means "whatever the preset says", which is the opposite.
    const reloaded = new Keymap(workspace);
    expect(reloaded.find('panel', 'j')).toBe(undefined);
  });

  it('removes a custom binding without exposing the preset under it', () => {
    const keys = new Keymap(new Workspace());
    keys.set('alt+w', 'TOP gainers');
    keys.remove('alt+w');
    expect(keys.find('global', 'alt+w')).toBe(undefined);
  });

  it('stores nothing when a binding restores a preset', () => {
    const keys = new Keymap(new Workspace());
    keys.set('alt+w', 'TOP gainers');
    expect(keys.customCount).toBe(1);

    keys.set('alt+w', 'W');
    expect(keys.customCount).toBe(0);
    expect(keys.find('global', 'alt+w')?.source).toBe('preset');
  });

  it('restores every preset on reset', () => {
    const keys = new Keymap(new Workspace());
    keys.set('alt+w', 'TOP gainers');
    keys.remove('j');

    expect(keys.reset()).toBe(2);
    expect(keys.find('global', 'alt+w')?.command).toBe('W');
    expect(keys.find('panel', 'j')?.command).toBe('ROW NEXT');
  });

  it('reports a sequence prefix so the router knows to wait', () => {
    const keys = new Keymap(new Workspace());
    keys.set('g w', 'W');

    expect(keys.find('panel', 'g w')?.command).toBe('W');
    expect(keys.isPrefix('panel', 'g')).toBe(true);
    expect(keys.isPrefix('panel', 'j')).toBe(false);
  });

  it('looks a chord up as typed', () => {
    const keys = new Keymap(new Workspace());
    expect(keys.describe('Alt+W')?.command).toBe('W');
    expect(keys.describe('ctrl+shift+f9')).toBe(undefined);
  });
});
