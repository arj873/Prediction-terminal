/**
 * Menu bar tests.
 *
 * The bar exists to teach the command line, which only works if what it prints
 * is true. Two kinds of untruth are possible and both are checked here: an
 * entry that names a command the terminal does not have — the menu equivalent
 * of a key bound to nothing — and an entry that runs a bare verb which needs an
 * argument, so clicking it can only ever produce a usage error.
 *
 * The third check is coverage. "Everything is reachable from the bar" is the
 * whole promise of the thing, and the only way to keep it is to assert it: a
 * command added to the registry has to turn up in the tree, which it does
 * because the LEARN menu is generated from the registry rather than typed out
 * a second time.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { Workspace } from '../src/client/state.js';
import { applySubject, Keymap } from '../src/client/terminal/keys.js';
import {
  bindingsFor,
  chordLabel,
  commandOf,
  defaultInvocation,
  EXAMPLE_VERBS,
  findMenu,
  MENUS,
  needsArgument,
  usageAlternatives,
  verbOf,
  walkEntries,
  type MenuItem,
} from '../src/client/terminal/menu.js';
import { COMMANDS, COMMAND_INDEX } from '../src/client/terminal/registry.js';

/** Every leaf in every menu, flattened. */
function items(): MenuItem[] {
  return MENUS.flatMap((menu) => walkEntries(menu.entries)).filter(
    (entry): entry is MenuItem => entry.kind === 'item',
  );
}

describe('usageAlternatives', () => {
  it('splits the forms a command takes, not the choices inside one', () => {
    assert.deepEqual(usageAlternatives('W | W ADD <ticker> | W CLEAR'), [
      'W',
      'W ADD <ticker>',
      'W CLEAR',
    ]);
    assert.deepEqual(usageAlternatives('GP <ticker> [1m|1h|1d] [range]'), [
      'GP <ticker> [1m|1h|1d] [range]',
    ]);
    assert.deepEqual(usageAlternatives('THEME <amber|green|ice>'), ['THEME <amber|green|ice>']);
  });
});

describe('needsArgument', () => {
  it('reads the plainest form, which is the one the menu would run', () => {
    // `W` on its own opens the watchlist, so the menu runs it.
    assert.equal(needsArgument('W | W ADD [venue:]<ticker> | W CLEAR'), false);
    assert.equal(needsArgument('KEYS | KEYS <chord> <command…>'), false);
    assert.equal(needsArgument('TOP [volume|gainers] [venue]'), false);
    assert.equal(needsArgument('ZOOM'), false);
  });

  it('claims a required argument, wherever the brackets put it', () => {
    assert.equal(needsArgument('GP [venue:]<ticker> [1m|1h|1d]'), true);
    assert.equal(needsArgument('THEME <amber|green|ice>'), true);
    assert.equal(needsArgument('RT <title> | RT SEARCH <words>'), true);
  });

  it('ignores an argument that only appears inside an optional group', () => {
    // `SPOT` charts the US daily on its own; the country is an option, not a
    // demand, and reading it as one would send the menu to the prompt for no
    // reason.
    assert.equal(needsArgument('SPOT [global|<country>] [daily|weekly]'), false);
    assert.equal(needsArgument('NFLX [tv|films] [global|<country>]'), false);
  });

  it('sends every command that needs one to the prompt instead of running it', () => {
    for (const command of COMMANDS) {
      const invocation = defaultInvocation(command);
      assert.equal(
        invocation.startsWith('>'),
        needsArgument(command.usage),
        `${command.verb} is invoked as "${invocation}" but its usage is "${command.usage}"`,
      );
    }
  });
});

describe('the menu tree', () => {
  it('only names commands the terminal has', () => {
    for (const entry of items()) {
      const verb = verbOf(entry.command);
      assert.ok(
        COMMAND_INDEX.has(verb),
        `"${entry.label}" runs "${entry.command}", but ${verb} is not a command`,
      );
    }
  });

  it('never runs a bare verb that would answer with a usage error', () => {
    for (const entry of items()) {
      if (entry.command.startsWith('>')) continue;
      const command = commandOf(entry);
      // Only a verb with nothing after it: an entry that passes arguments is
      // the registry's business to validate, not this test's.
      if (!command || entry.command.trim() !== command.verb) continue;
      assert.equal(
        needsArgument(command.usage),
        false,
        `"${entry.label}" runs a bare ${command.verb}, which needs an argument`,
      );
    }
  });

  it('writes $ where the substitution can find it', () => {
    for (const entry of items()) {
      if (!entry.command.includes('$')) continue;
      assert.ok(
        !entry.command.startsWith('>'),
        `"${entry.label}" would type a literal $ at the prompt`,
      );
      assert.notEqual(
        applySubject(entry.command, 'KXTEST-1'),
        entry.command,
        `"${entry.command}" has a $ that is part of a word, so nothing will fill it`,
      );
    }
  });

  it('reaches every command in the registry', () => {
    const reachable = new Set(items().map((entry) => verbOf(entry.command)));
    for (const command of COMMANDS) {
      assert.ok(
        reachable.has(command.verb),
        `${command.verb} is in the registry but not in any menu`,
      );
    }
  });

  it('gives every menu a title MENU can name', () => {
    const seen = new Set<string>();
    for (const menu of MENUS) {
      assert.match(menu.title, /^[A-Z]+$/, `"${menu.title}" is not one shouted word`);
      assert.ok(!seen.has(menu.title), `${menu.title} appears twice on the bar`);
      seen.add(menu.title);
      assert.ok(menu.hint.length > 0, `${menu.title} has no hint`);
    }
  });

  it('offers examples the registry actually carries', () => {
    for (const verb of EXAMPLE_VERBS) {
      const command = COMMAND_INDEX.get(verb);
      assert.ok(command, `${verb} is offered as an example but is not a command`);
      assert.ok((command.examples ?? []).length > 0, `${verb} has no examples to offer`);
    }
  });
});

describe('findMenu', () => {
  it('takes a full title or the start of one', () => {
    assert.equal(findMenu(MENUS, 'markets'), 0);
    assert.equal(findMenu(MENUS, 'MARK'), 0);
    assert.equal(findMenu(MENUS, 'learn'), MENUS.length - 1);
  });

  it('reports a name no menu goes by', () => {
    assert.equal(findMenu(MENUS, 'wibble'), -1);
    assert.equal(findMenu(MENUS, '  '), -1);
  });
});

describe('bindingsFor', () => {
  const keymap = new Keymap(new Workspace());
  const chords = (command: string): string[] =>
    bindingsFor(keymap, command).map((binding) => binding.chord);

  it('finds the key that runs exactly this line', () => {
    assert.ok(chords('W').includes('alt+w'));
    assert.ok(chords('OB $').includes('b'));
  });

  it('puts the key that works while you are typing first', () => {
    // `ROW NEXT` is Alt+↓ at the prompt and `j` in NAV mode; the one that works
    // without changing modes is the one to teach first.
    assert.deepEqual(chords('ROW NEXT').slice(0, 2), ['alt+arrowdown', 'j']);
  });

  it('never claims a key that runs something else', () => {
    // Alt+T runs TOP. It does not run TOP gainers, and saying so would teach a
    // key that opens the wrong leaderboard.
    assert.deepEqual(chords('TOP gainers'), []);
  });

  it('ignores spacing and case, which the parser ignores too', () => {
    assert.deepEqual(chords('top'), chords('TOP'));
    assert.deepEqual(chords('ROW  NEXT'), chords('ROW NEXT'));
  });

  it('says when a key only fires in NAV mode', () => {
    // `C` next to `Alt+T` reads as a key you could press at the prompt, where
    // it would type a letter and nothing else.
    assert.deepEqual(bindingsFor(keymap, 'TOP').map(chordLabel), ['Alt+T', 'NAV T']);
    assert.deepEqual(bindingsFor(keymap, 'GP $').map(chordLabel), ['NAV C']);
  });
});
