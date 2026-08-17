/**
 * KEYS — the live key map, generated from the same table the router fires.
 *
 * Presets and the reader's own bindings are listed together and marked, so the
 * answer to "what does this key do, and did I do that?" is one panel rather
 * than a hunt through localStorage. Every row is clickable, and clicking one
 * puts its binding back on the command line ready to edit.
 */

import { cell, el, row, table } from '../lib/dom.js';
import { formatChord, inferScope, type Binding, type Keymap } from '../terminal/keys.js';
import { COMMAND_INDEX } from '../terminal/registry.js';
import { Panel, type PanelContext } from './panel.js';
import { bindRow } from './table.js';

/** The keys the command line handles itself; they are not bindings. */
const COMMAND_LINE_KEYS: readonly (readonly [string, string])[] = [
  ['Enter', 'Run the line'],
  ['↑ / ↓', 'Previous / next command in history'],
  ['Tab / Shift+Tab', 'Complete the verb'],
  ['Esc', 'Clear the line — again on an empty line hands the keyboard to the panels'],
];

/**
 * What a binding does, in one line.
 *
 * Presets say so themselves. A binding the reader wrote is described by the
 * command it runs, which is the only honest source: the terminal has no idea
 * why they wanted it.
 */
function describe(binding: Binding): string {
  if (binding.note) return binding.note;

  const command = binding.command.startsWith('>') ? binding.command.slice(1) : binding.command;
  const verb = command.trim().split(/\s+/)[0]?.toUpperCase() ?? '';
  const known = COMMAND_INDEX.get(verb);

  if (binding.command.startsWith('>')) {
    return command.trim() ? `Start a ${verb} command` : 'Jump to the command line';
  }
  return known?.summary ?? 'Custom binding';
}

/** The line that reproduces this binding, for the command bar. */
function rebindCommand(binding: Binding): string {
  // A sequence carries a space, which the parser would read as two arguments.
  const chord = binding.chord.includes(' ') ? `"${binding.chord}"` : binding.chord;
  const scope = inferScope(binding.chord) === binding.scope ? '' : ` --${binding.scope}`;
  return `>KEYS ${chord}${scope} ${binding.command}`;
}

export class KeysPanel extends Panel<Binding[]> {
  override readonly kind = 'KEYS';

  static readonly ID = 'keys';

  readonly #keymap: Keymap;

  constructor(id: string, context: PanelContext, keymap: Keymap) {
    super(id, context);
    this.#keymap = keymap;
  }

  protected override title(): string {
    return 'KEY MAP';
  }

  protected override subtitle(): string {
    const custom = this.#keymap.customCount;
    return custom === 0 ? 'presets' : `presets · ${custom} edited`;
  }

  protected override load(): Promise<Binding[]> {
    return Promise.resolve(this.#keymap.list());
  }

  protected override render(bindings: Binding[]): void {
    this.body.append(
      el('div', { class: 'result-note' }, [
        el('span', { text: `${bindings.length} bindings` }),
        el('span', { class: 'dim', text: ' · click a row to edit it' }),
      ]),
    );

    this.#group(
      'GLOBAL — anywhere, even mid-word at the prompt',
      bindings.filter((b) => b.scope === 'global'),
    );
    this.#group(
      'PANEL — in NAV mode, where the keyboard belongs to the workspace',
      bindings.filter((b) => b.scope === 'panel'),
    );

    this.body.append(
      el('div', { class: 'help-group' }, [
        el('h3', { class: 'help-group-title', text: 'COMMAND LINE' }),
        table(
          ['KEY', 'ACTION'],
          COMMAND_LINE_KEYS.map(([key, action]) => row([cell(key, 'mono strong'), cell(action)])),
        ),
      ]),
      el('div', { class: 'help-group' }, [
        el('h3', { class: 'help-group-title', text: 'BINDING YOUR OWN' }),
        el('ul', { class: 'help-list' }, [
          el('li', { text: 'KEYS <chord> <command> — bind it. KEYS DEL <chord> unbinds, KEYS RESET restores every preset.' }),
          el('li', {
            text: 'A chord is a key with optional modifiers: alt+b, ctrl+shift+k, f4, [. Two chords with a space between them make a sequence: "g w".',
          }),
          el('li', {
            text: 'A chord that types a character is a PANEL binding; anything with Ctrl, Alt or Meta is GLOBAL. Force it with --panel or --global.',
          }),
          el('li', {
            text: '$ in a command is the row under the cursor, or the market the focused panel is about — so OB $ is "the book for whatever I am looking at".',
          }),
          el('li', {
            text: '> in front of a command types it at the prompt instead of running it, for the verbs that need an argument.',
          }),
          el('li', {
            class: 'dim',
            text: 'NAV mode: Esc on an empty command line, or Alt+J. The header says NAV while the keyboard is driving the panels; / or Esc gives it back.',
          }),
        ]),
      ]),
    );
  }

  #group(title: string, bindings: Binding[]): void {
    if (bindings.length === 0) return;

    const rows = bindings.map((binding) => {
      const tr = row([
        cell(formatChord(binding.chord), 'mono strong'),
        cell(binding.command, 'mono dim'),
        cell(describe(binding)),
        cell(binding.source === 'custom' ? 'custom' : '', 'dim'),
      ]);
      bindRow(
        tr,
        rebindCommand(binding),
        (command) => this.context.run(command),
        `Edit this binding on the command line`,
      );
      return tr;
    });

    this.body.append(
      el('div', { class: 'help-group' }, [
        el('h3', { class: 'help-group-title', text: title }),
        table(['KEY', 'RUNS', 'WHAT IT DOES', ''], rows, 'keys-table'),
      ]),
    );
  }
}
