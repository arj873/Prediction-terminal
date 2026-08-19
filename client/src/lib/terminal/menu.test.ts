/**
 * The menus, checked against the command table they claim to describe.
 *
 * The point of these is that the bar cannot quietly go stale. A menu is a
 * hand-written list of command lines, and the commands move; without something
 * asserting the two still agree, the first thing a new reader clicks would be
 * the first thing to have been renamed.
 */

import { describe, expect, it } from 'vitest';

import { COMMAND_INDEX, COMMANDS } from './commands';
import { Keymap } from './keys';
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
} from './menu';
import { Workspace } from '../state/workspace.svelte';

/** Every leaf in every menu. */
const items = MENUS.flatMap((menu) => walkEntries(menu.entries)).filter(
  (entry): entry is MenuItem => entry.kind === 'item',
);

describe('the menu tree', () => {
  it('offers something under every heading', () => {
    expect(MENUS.length).toBeGreaterThan(0);
    for (const menu of MENUS) {
      expect(menu.title, 'a heading with no title').toBeTruthy();
      expect(menu.hint, `${menu.title} has no hint`).toBeTruthy();
      expect(walkEntries(menu.entries).length, `${menu.title} is empty`).toBeGreaterThan(0);
    }
  });

  it('names no command the terminal does not have', () => {
    const unknown = items
      .map((entry) => ({ entry, verb: verbOf(entry.command) }))
      .filter(({ verb }) => !COMMAND_INDEX.has(verb));

    expect(unknown.map(({ entry, verb }) => `${entry.label} → ${verb}`)).toEqual([]);
  });

  it('never runs a bare verb that needs an argument', () => {
    // Such an entry can only produce a usage error. The menu types it at the
    // prompt instead, which is where the usage line is shown anyway.
    const wrong = items.filter((entry) => {
      if (entry.command.startsWith('>')) return false;
      const command = commandOf(entry);
      if (!command) return false;
      const bare = entry.command.trim().toUpperCase() === command.verb;
      return bare && needsArgument(command.usage);
    });

    expect(wrong.map((entry) => entry.command)).toEqual([]);
  });

  it('reaches every command in the table from the LEARN menu', () => {
    // The generated half. A verb added to the table is in the menus the same
    // day, and this is what says so.
    const reachable = new Set(items.map((entry) => verbOf(entry.command)));
    const missing = COMMANDS.map((command) => command.verb).filter((verb) => !reachable.has(verb));

    expect(missing).toEqual([]);
  });

  it('offers examples only for verbs that have them', () => {
    for (const verb of EXAMPLE_VERBS) {
      const command = COMMAND_INDEX.get(verb);
      expect(command, `${verb} is not a command`).toBeDefined();
      expect(command?.examples?.length, `${verb} has no examples to show`).toBeGreaterThan(0);
    }
  });

  it('gives every entry a label a reader can tell apart from its siblings', () => {
    for (const menu of MENUS) {
      const labels = menu.entries
        .filter((entry) => entry.kind === 'item' || entry.kind === 'group')
        .map((entry) => (entry as { label: string }).label);
      expect(new Set(labels).size, `${menu.title} repeats a label`).toBe(labels.length);
    }
  });
});

describe('verbOf', () => {
  it('reads the verb through a prompt prefix and arguments', () => {
    expect(verbOf('TOP volume kalshi')).toBe('TOP');
    expect(verbOf('>GP')).toBe('GP');
    expect(verbOf('  w add $ ')).toBe('W');
    expect(verbOf('')).toBe('');
  });
});

describe('usageAlternatives', () => {
  it('splits on the bars outside every bracket', () => {
    expect(usageAlternatives('W | W ADD <ticker> | W DEL <ticker>')).toEqual([
      'W',
      'W ADD <ticker>',
      'W DEL <ticker>',
    ]);
  });

  it('leaves a choice inside a bracket alone', () => {
    expect(usageAlternatives('GP <ticker> [1m|1h|1d] [range]')).toEqual([
      'GP <ticker> [1m|1h|1d] [range]',
    ]);
  });
});

describe('needsArgument', () => {
  it('is true when the plainest form names a required argument', () => {
    expect(needsArgument('GP <ticker> [1m|1h|1d]')).toBe(true);
    expect(needsArgument('DES [venue:]<ticker>')).toBe(true);
  });

  it('is false when every argument is optional', () => {
    expect(needsArgument('TOP [volume|gainers] [venue]')).toBe(false);
    expect(needsArgument('SPOT [global|<country>]')).toBe(false);
  });

  it('reads the first form, which is the one the menu would run', () => {
    expect(needsArgument('W | W ADD <ticker>')).toBe(false);
  });
});

describe('defaultInvocation', () => {
  it('runs a command that stands alone and types one that does not', () => {
    const top = COMMAND_INDEX.get('TOP');
    const gp = COMMAND_INDEX.get('GP');
    expect(top && defaultInvocation(top)).toBe('TOP');
    expect(gp && defaultInvocation(gp)).toBe('>GP');
  });
});

describe('findMenu', () => {
  it('takes a full title or an unambiguous prefix, case-insensitively', () => {
    expect(findMenu(MENUS, 'MARKETS')).toBe(0);
    expect(findMenu(MENUS, 'mark')).toBe(0);
    expect(findMenu(MENUS, 'learn')).toBe(MENUS.length - 1);
  });

  it('refuses a name nothing starts with', () => {
    expect(findMenu(MENUS, 'nonsense')).toBe(-1);
    expect(findMenu(MENUS, '   ')).toBe(-1);
  });
});

describe('bindingsFor', () => {
  const keymap = () => new Keymap(new Workspace());

  it('matches a command line exactly, not by verb', () => {
    // Alt+T runs TOP. Printing it beside `TOP gainers` would teach a key that
    // opens the wrong leaderboard.
    const map = keymap();
    expect(bindingsFor(map, 'TOP').length).toBeGreaterThan(0);
    expect(bindingsFor(map, 'TOP gainers')).toEqual([]);
  });

  it('ignores spacing and case, which are not part of the binding', () => {
    expect(bindingsFor(keymap(), '  top  ').length).toBeGreaterThan(0);
  });

  it('puts the global binding first, because it works while you type', () => {
    const map = keymap();
    const found = bindingsFor(map, 'W');
    expect(found.length).toBeGreaterThan(1);
    expect(found[0]?.scope).toBe('global');
  });

  it('marks a panel chord as NAV, so it is not read as a key for the prompt', () => {
    const panel = bindingsFor(keymap(), 'W').find((binding) => binding.scope === 'panel');
    expect(panel && chordLabel(panel).startsWith('NAV ')).toBe(true);
  });

  it('finds the menu bar its own key', () => {
    expect(bindingsFor(keymap(), 'MENU').length).toBeGreaterThan(0);
  });
});
