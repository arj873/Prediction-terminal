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

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
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
} from '../src/client/terminal/keys.js';
import { COMMAND_INDEX } from '../src/client/terminal/registry.js';
import { Workspace } from '../src/client/state.js';

/** A key press, with the modifiers nobody set defaulted to false. */
function press(key: string, modifiers: Partial<Record<string, boolean>> & { code?: string } = {}) {
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
    assert.equal(parseChord('Control+Alt+k'), 'ctrl+alt+k');
    assert.equal(parseChord('alt+ctrl+k'), 'ctrl+alt+k');
    assert.equal(parseChord('cmd+k'), 'meta+k');
    assert.equal(parseChord('option+k'), 'alt+k');
  });

  it('reads a capital letter as the key, not as Shift', () => {
    // Every command in this terminal is shouted, so `KEYS W …` plainly means
    // the `w` key. Shift has to be asked for.
    assert.equal(parseChord('W'), 'w');
    assert.equal(parseChord('shift+w'), 'shift+w');
  });

  it('drops Shift from a character that already carries it', () => {
    assert.equal(parseChord('shift+?'), '?');
    assert.equal(parseChord('shift+1'), '1');
  });

  it('accepts named keys and their short forms', () => {
    assert.equal(parseChord('Esc'), 'escape');
    assert.equal(parseChord('alt+up'), 'alt+arrowup');
    assert.equal(parseChord('PgDn'), 'pagedown');
    assert.equal(parseChord('F4'), 'f4');
  });

  it('reads a trailing plus as the plus key', () => {
    assert.equal(parseChord('ctrl++'), 'ctrl++');
    assert.equal(parseChord('+'), '+');
  });

  it('refuses a key or modifier it does not know', () => {
    assert.throws(() => parseChord('hyper+k'), /Unknown modifier/);
    assert.throws(() => parseChord('ctrl+wibble'), /Unknown key/);
    assert.throws(() => parseChord('   '), /Missing <chord>/);
  });
});

describe('parseChordSequence', () => {
  it('normalises every chord in a sequence', () => {
    assert.equal(parseChordSequence('G  Shift+W'), 'g shift+w');
  });

  it('refuses a sequence nobody could remember', () => {
    assert.throws(() => parseChordSequence('a b c d'), /at most three chords/);
  });
});

describe('chordFromEvent', () => {
  it('names the same chord a typed binding does', () => {
    assert.equal(chordFromEvent(press('k', { ctrlKey: true })), 'ctrl+k');
    assert.equal(chordFromEvent(press('ArrowDown', { altKey: true })), 'alt+arrowdown');
    assert.equal(chordFromEvent(press(' ')), 'space');
    assert.equal(chordFromEvent(press('G', { shiftKey: true })), 'shift+g');
    assert.equal(chordFromEvent(press('?', { shiftKey: true })), '?');
  });

  it('ignores a bare modifier press', () => {
    assert.equal(chordFromEvent(press('Shift', { shiftKey: true })), null);
    assert.equal(chordFromEvent(press('Control', { ctrlKey: true })), null);
  });

  it('reads the physical key when Alt is down', () => {
    // macOS turns Option+S into `ß`, which would make every alt+letter preset
    // unreachable on that keyboard.
    assert.equal(chordFromEvent(press('ß', { altKey: true, code: 'KeyS' })), 'alt+s');
    assert.equal(chordFromEvent(press('¡', { altKey: true, code: 'Digit1' })), 'alt+1');
  });
});

describe('formatChord', () => {
  it('renders a chord the way a key map reads', () => {
    assert.equal(formatChord('alt+arrowdown'), 'Alt+↓');
    assert.equal(formatChord('ctrl+shift+k'), 'Ctrl+Shift+K');
    assert.equal(formatChord('escape'), 'Esc');
    assert.equal(formatChord('g w'), 'G W');
    assert.equal(formatChord('ctrl++'), 'Ctrl++');
  });
});

describe('inferScope', () => {
  it('keeps a typing key out of the global scope', () => {
    assert.equal(inferScope('j'), 'panel');
    assert.equal(inferScope('shift+g'), 'panel');
    assert.equal(inferScope('enter'), 'panel');
    assert.equal(inferScope('tab'), 'panel');
  });

  it('claims chords that type nothing', () => {
    assert.equal(inferScope('alt+s'), 'global');
    assert.equal(inferScope('ctrl+arrowleft'), 'global');
    assert.equal(inferScope('f4'), 'global');
    assert.equal(inferScope('escape'), 'global');
  });

  it('reads a sequence by its first chord', () => {
    assert.equal(inferScope('g w'), 'panel');
    assert.equal(inferScope('alt+g w'), 'global');
  });
});

describe('applySubject', () => {
  it('fills a whole $ token', () => {
    assert.equal(applySubject('OB $', 'KXFED-1'), 'OB KXFED-1');
    assert.equal(applySubject('W ADD $', 'pm:slug'), 'W ADD pm:slug');
  });

  it('leaves a $ that is part of a word alone', () => {
    assert.equal(applySubject('SRCH $100 bill', undefined), 'SRCH $100 bill');
  });

  it('refuses to run a command whose subject is missing', () => {
    assert.equal(applySubject('OB $', undefined), null);
  });

  it('passes a command with no $ straight through', () => {
    assert.equal(applySubject('TOP volume', undefined), 'TOP volume');
  });
});

describe('isTypingTarget', () => {
  it('claims text fields and nothing else', () => {
    assert.equal(isTypingTarget({ tagName: 'INPUT' } as unknown as EventTarget), true);
    assert.equal(isTypingTarget({ tagName: 'TEXTAREA' } as unknown as EventTarget), true);
    assert.equal(
      isTypingTarget({ tagName: 'DIV', isContentEditable: true } as unknown as EventTarget),
      true,
    );
    assert.equal(isTypingTarget({ tagName: 'SECTION' } as unknown as EventTarget), false);
    assert.equal(isTypingTarget(null), false);
  });
});

describe('preset bindings', () => {
  it('runs commands the terminal actually has', () => {
    for (const binding of PRESET_BINDINGS) {
      const line = binding.command.startsWith('>') ? binding.command.slice(1) : binding.command;
      const verb = line.trim().split(/\s+/)[0]?.toUpperCase() ?? '';
      if (!verb) continue;
      assert.ok(
        COMMAND_INDEX.has(verb),
        `${binding.chord} runs "${binding.command}", but ${verb} is not a command`,
      );
    }
  });

  it('binds each chord once per scope', () => {
    const seen = new Set<string>();
    for (const binding of PRESET_BINDINGS) {
      const key = `${binding.scope}:${binding.chord}`;
      assert.ok(!seen.has(key), `${key} is bound twice`);
      seen.add(key);
    }
  });

  it('never binds a typing key globally', () => {
    for (const binding of PRESET_BINDINGS) {
      if (binding.scope !== 'global') continue;
      assert.equal(
        inferScope(binding.chord),
        'global',
        `${binding.chord} would fire mid-word at the prompt`,
      );
    }
  });

  it('ships every chord already normalised', () => {
    for (const binding of PRESET_BINDINGS) {
      assert.equal(parseChordSequence(binding.chord), binding.chord);
    }
  });
});

describe('Keymap', () => {
  it('resolves a preset', () => {
    const keys = new Keymap(new Workspace());
    assert.equal(keys.find('panel', 'j')?.command, 'ROW NEXT');
    assert.equal(keys.find('global', 'alt+w')?.command, 'W');
    // Scope is part of the identity: `j` is not a global binding.
    assert.equal(keys.find('global', 'j'), undefined);
  });

  it('lets a custom binding replace a preset', () => {
    const keys = new Keymap(new Workspace());
    keys.set('alt+w', 'TOP gainers');

    const bound = keys.find('global', 'alt+w');
    assert.equal(bound?.command, 'TOP gainers');
    assert.equal(bound?.source, 'custom');
  });

  it('infers the scope from the chord, and takes an override', () => {
    const keys = new Keymap(new Workspace());

    assert.equal(keys.set('alt+b', 'OB $').scope, 'global');
    assert.equal(keys.set('y', 'W').scope, 'panel');
    assert.equal(keys.set('alt+y', 'W', 'panel').scope, 'panel');
  });

  it('refuses a global binding that would eat typing', () => {
    const keys = new Keymap(new Workspace());
    assert.throws(() => keys.set('y', 'W', 'global'), /types a character/);
  });

  it('keeps a preset switched off across a reload', () => {
    const workspace = new Workspace();
    const keys = new Keymap(workspace);

    keys.remove('j');
    assert.equal(keys.find('panel', 'j'), undefined);

    // The overlay has to carry the removal, not just the absence of an edit —
    // an absent entry means "whatever the preset says", which is the opposite.
    const reloaded = new Keymap(workspace);
    assert.equal(reloaded.find('panel', 'j'), undefined);
  });

  it('removes a custom binding without exposing the preset under it', () => {
    const keys = new Keymap(new Workspace());
    keys.set('alt+w', 'TOP gainers');
    keys.remove('alt+w');
    assert.equal(keys.find('global', 'alt+w'), undefined);
  });

  it('stores nothing when a binding restores a preset', () => {
    const keys = new Keymap(new Workspace());
    keys.set('alt+w', 'TOP gainers');
    assert.equal(keys.customCount, 1);

    keys.set('alt+w', 'W');
    assert.equal(keys.customCount, 0);
    assert.equal(keys.find('global', 'alt+w')?.source, 'preset');
  });

  it('restores every preset on reset', () => {
    const keys = new Keymap(new Workspace());
    keys.set('alt+w', 'TOP gainers');
    keys.remove('j');

    assert.equal(keys.reset(), 2);
    assert.equal(keys.find('global', 'alt+w')?.command, 'W');
    assert.equal(keys.find('panel', 'j')?.command, 'ROW NEXT');
  });

  it('reports a sequence prefix so the router knows to wait', () => {
    const keys = new Keymap(new Workspace());
    keys.set('g w', 'W');

    assert.equal(keys.find('panel', 'g w')?.command, 'W');
    assert.equal(keys.isPrefix('panel', 'g'), true);
    assert.equal(keys.isPrefix('panel', 'j'), false);
  });

  it('looks a chord up as typed', () => {
    const keys = new Keymap(new Workspace());
    assert.equal(keys.describe('Alt+W')?.command, 'W');
    assert.equal(keys.describe('ctrl+shift+f9'), undefined);
  });
});
