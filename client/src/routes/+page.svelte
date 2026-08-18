<!--
  The terminal.

  Everything the app is made of is constructed here and threaded down through
  one context, so no module reaches for a global: the workspace, the panel grid,
  the key map, and `run` — the single dispatch path.

  `run` is worth naming. A typed line, a clicked row, a key binding, a tape item
  and an action button all go through it, which is why the keyboard can reach
  anything the mouse can: there is only one way to make something happen, and
  everything uses it.
-->
<script lang="ts">
  import { onDestroy, onMount } from 'svelte';

  import CommandLine, { type LogLine } from '$lib/components/CommandLine.svelte';
  import StatusBar from '$lib/components/StatusBar.svelte';
  import Tape from '$lib/components/Tape.svelte';
  import PanelGrid from '$lib/panels/PanelGrid.svelte';
  import { setTerminalContext, type LogLevel } from '$lib/context';
  import { ApiRequestError } from '$lib/api/client';
  import { COMMAND_INDEX } from '$lib/terminal/commands';
  import { UsageError, type CommandContext } from '$lib/terminal/command';
  import { KeyRouter, Keymap, type KeyMode } from '$lib/terminal/keys';
  import { parse } from '$lib/terminal/parser';
  import { PanelStore } from '$lib/state/panels.svelte';
  import { Workspace } from '$lib/state/workspace.svelte';

  const MAX_LOG_LINES = 300;

  const workspace = new Workspace();
  const panels = new PanelStore();
  const keymap = new Keymap(workspace);

  let lines = $state<LogLine[]>([]);
  let mode = $state<KeyMode>('cmd');
  let commandLine = $state<CommandLine | undefined>(undefined);
  let nextLineId = 0;

  function log(message: string, level: LogLevel = 'info') {
    lines = [...lines, { id: nextLineId++, at: stamp(), message, level }].slice(-MAX_LOG_LINES);
  }

  function stamp(): string {
    const now = new Date();
    const pad = (n: number) => String(n).padStart(2, '0');
    return `${pad(now.getUTCHours())}:${pad(now.getUTCMinutes())}:${pad(now.getUTCSeconds())}Z`;
  }

  /**
   * The one dispatch path.
   *
   * A leading `>` means "put this on the command line, do not run it" — used by
   * the bindings for verbs that need an argument, so `Alt+G` gets you as far as
   * `GP ` and waits for the ticker.
   */
  function run(input: string): void {
    const text = input.trim();
    if (text === '') return;

    if (text.startsWith('>')) {
      commandLine?.prefill(`${text.slice(1).trimStart()} `);
      return;
    }

    log(text, 'echo');
    workspace.pushHistory(text);

    const parsed = parse(text);
    const command = COMMAND_INDEX.get(parsed.verb);
    if (!command) {
      log(`Unknown command "${parsed.verb}". Type HELP for the list.`, 'error');
      return;
    }

    try {
      const result = command.handler(parsed, context);
      if (result instanceof Promise) result.catch(reportError);
    } catch (err) {
      reportError(err);
    }
  }

  /**
   * Failures are reported, never swallowed.
   *
   * A usage mistake gets the command's own usage line appended, and an API
   * failure gets the server's hint verbatim — that hint is written to be read by
   * a person and is often the only thing that says what to do next.
   */
  function reportError(err: unknown): void {
    if (err instanceof UsageError) {
      log(err.message, 'error');
      return;
    }
    if (err instanceof ApiRequestError) {
      log(err.hint ? `${err.message} · ${err.hint}` : err.message, 'error');
      return;
    }
    log(err instanceof Error ? err.message : String(err), 'error');
  }

  const context: CommandContext = {
    run,
    log,
    panels,
    workspace,
    keys: keymap,
    clearLog: () => {
      lines = [];
    },
    setMode: (next: KeyMode) => router.setMode(next),
  };

  setTerminalContext(context);

  const router = new KeyRouter({
    keymap,
    run,
    log,
    subject: () => panels.subject(),
    focusPrompt: () => commandLine?.focus(),
    blurPrompt: () => commandLine?.blur(),
    focusWorkspace: () => panels.focusElement(),
    onMode: (next) => {
      mode = next;
    },
  });

  onMount(() => {
    workspace.applyTheme();

    log('PREDICTION TERMINAL — three books, one prompt.');
    log('Type a command, or HELP for the list. Esc on an empty line enters NAV mode.');

    // Open something rather than presenting an empty grid: whatever the reader
    // was watching last session, or the busiest board if they have no list yet.
    run('HELP');
    run(workspace.watchlist.length > 0 ? 'W' : 'TOP volume');
  });

  onDestroy(() => {
    panels.closeAll();
  });
</script>

<svelte:window onkeydown={(event) => router.handle(event)} />

<StatusBar {mode} />
<Tape />
<PanelGrid />
<CommandLine
  bind:this={commandLine}
  {lines}
  onEscape={() => router.setMode(mode === 'nav' ? 'cmd' : 'nav')}
/>
