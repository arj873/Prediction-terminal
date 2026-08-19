<!--
  KEYS — the live key map, generated from the same table the router fires.

  Presets and the reader's own bindings are listed together and marked, so the
  answer to "what does this key do, and did I do that?" is one panel rather than
  a hunt through localStorage. Every row is clickable, and clicking one puts its
  binding back on the command line ready to edit.
-->
<script lang="ts">
  import PanelFrame from './PanelFrame.svelte';
  import DataTable from './DataTable.svelte';
  import { createPanelData } from './data.svelte';
  import { getTerminalContext } from '../context';
  import { COMMAND_INDEX } from '../terminal/commands';
  import { Keymap, formatChord, inferScope, type Binding } from '../terminal/keys';
  import type { Workspace } from '../state/workspace.svelte';

  const { id }: { id: string } = $props();

  const { workspace } = getTerminalContext();

  /**
   * One key map per workspace.
   *
   * A `Keymap` subscribes to its workspace to know when to drop its cache and
   * offers no way to unsubscribe, so building a fresh one every time the panel
   * opened would leave a listener behind each time. Presets plus the workspace
   * overlay is the whole definition of the map, so any two instances over the
   * same workspace agree by construction — this just keeps there being one.
   */
  const maps = new WeakMap<Workspace, Keymap>();
  function keymapFor(target: Workspace): Keymap {
    const existing = maps.get(target);
    if (existing) return existing;
    const created = new Keymap(target);
    maps.set(target, created);
    return created;
  }

  const keymap = keymapFor(workspace);

  /**
   * Nothing is fetched — the bindings are already in memory. Going through
   * `createPanelData` anyway is what keeps the `PanelFrame` contract honest:
   * the frame renders a body only once `data.data` is defined, and `KEYS`
   * re-opens this panel after every edit, which is a `refresh()` and therefore
   * a re-`load()`.
   */
  const data = createPanelData({ load: async () => keymap.list() });

  const custom = $derived(keymap.customCount);
  const subtitle = $derived(custom === 0 ? 'presets' : `presets · ${custom} edited`);

  /** The keys the command line handles itself; they are not bindings. */
  const COMMAND_LINE_KEYS: readonly (readonly [string, string])[] = [
    ['Enter', 'Run the line'],
    ['↑ / ↓', 'Previous / next command in history'],
    ['Tab / Shift+Tab', 'Complete the verb'],
    ['Esc', 'Clear the line — again on an empty line hands the keyboard to the panels'],
  ];

  const BINDING_NOTES: readonly string[] = [
    'KEYS <chord> <command> — bind it. KEYS DEL <chord> unbinds, KEYS RESET restores every preset.',
    'A chord is a key with optional modifiers: alt+b, ctrl+shift+k, f4, [. Two chords with a space between them make a sequence: "g w".',
    'A chord that types a character is a PANEL binding; anything with Ctrl, Alt or Meta is GLOBAL. Force it with --panel or --global.',
    '$ in a command is the row under the cursor, or the market the focused panel is about — so OB $ is "the book for whatever I am looking at".',
    '> in front of a command types it at the prompt instead of running it, for the verbs that need an argument.',
  ];

  const NAV_NOTE =
    'NAV mode: Esc on an empty command line, or Alt+J. The header says NAV while the keyboard is driving the panels; / or Esc gives it back.';

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

  const bindingColumns = [
    { header: 'KEY', cell: (b: Binding) => formatChord(b.chord), class: 'mono strong' },
    { header: 'RUNS', cell: (b: Binding) => b.command, class: 'mono dim' },
    { header: 'WHAT IT DOES', cell: (b: Binding) => describe(b) },
    // Preset or the reader's own: the unlabelled column that answers "did I do
    // that?" without a legend.
    { header: '', cell: (b: Binding) => (b.source === 'custom' ? 'custom' : ''), class: 'dim' },
  ];

  const keyColumns = [
    { header: 'KEY', cell: (hint: readonly string[]) => hint[0] ?? '', class: 'mono strong' },
    { header: 'ACTION', cell: (hint: readonly string[]) => hint[1] ?? '' },
  ];

  const GROUPS = [
    { title: 'GLOBAL — anywhere, even mid-word at the prompt', scope: 'global' as const },
    {
      title: 'PANEL — in NAV mode, where the keyboard belongs to the workspace',
      scope: 'panel' as const,
    },
  ];
</script>

<PanelFrame {id} kind="KEYS" title="KEY MAP" {subtitle} {data}>
  {@const bindings = data.data as Binding[]}

  <div class="result-note">
    <span>{bindings.length} bindings</span>
    <span class="dim"> · click a row to edit it</span>
  </div>

  {#each GROUPS as group (group.scope)}
    {@const rows = bindings.filter((b) => b.scope === group.scope)}
    {#if rows.length > 0}
      <div class="help-group">
        <h3 class="help-group-title">{group.title}</h3>
        <DataTable
          {rows}
          columns={bindingColumns}
          tableClass="keys-table"
          rowCommand={(b: Binding) => rebindCommand(b)}
          rowTitle={() => 'Edit this binding on the command line'}
        />
      </div>
    {/if}
  {/each}

  <div class="help-group">
    <h3 class="help-group-title">COMMAND LINE</h3>
    <DataTable rows={COMMAND_LINE_KEYS} columns={keyColumns} />
  </div>

  <div class="help-group">
    <h3 class="help-group-title">BINDING YOUR OWN</h3>
    <ul class="help-list">
      {#each BINDING_NOTES as note (note)}
        <li>{note}</li>
      {/each}
      <li class="dim">{NAV_NOTE}</li>
    </ul>
  </div>
</PanelFrame>
