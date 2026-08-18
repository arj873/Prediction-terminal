<!--
  The prompt, its message log, and the hint line between them.

  Three behaviours here are the terminal's feel, and all three are deliberate.

  History walks with the arrow keys, and the line you were part-way through
  typing is stashed at position 0 — so walking up and back down returns you to
  your own draft rather than an empty prompt.

  Tab completes *verbs only*. Arguments are open-ended by design: a ticker, a
  slug, a search phrase and a date all arrive here, and offering completions
  for them would be guessing.

  Any keystroke carrying Ctrl, Alt or Meta is left alone for the key router, so
  a global chord fires even mid-word. Shift is deliberately not in that list, or
  Shift+Tab would stop completing.
-->
<script lang="ts">
  import { tick } from 'svelte';

  import { getTerminalContext, type LogLevel } from '../context';
  import { ALL_VERBS, COMMAND_INDEX } from '../terminal/commands';

  /** Candidate verbs offered on the hint line before it gives up listing them. */
  const MAX_HINTS = 12;

  export interface LogLine {
    id: number;
    at: string;
    message: string;
    level: LogLevel;
  }

  interface Props {
    lines: readonly LogLine[];
    /** Called when Esc is pressed on an already-empty line. */
    onEscape?: () => void;
  }

  const { lines, onEscape }: Props = $props();
  const { run, workspace } = getTerminalContext();

  let value = $state('');
  let input = $state<HTMLInputElement | undefined>(undefined);
  let logEl = $state<HTMLElement | undefined>(undefined);

  /** Where in history we are. `-1` is the live draft. */
  let historyAt = $state(-1);
  let draft = '';

  /** Tab-completion state: the prefix captured on the first Tab, and the cycle. */
  let completionPrefix: string | undefined;
  let completionAt = -1;

  const hint = $derived.by(() => {
    const text = value.trim();
    if (text === '') return '';

    const verb = text.split(/\s+/)[0]!.toUpperCase();
    const command = COMMAND_INDEX.get(verb);
    if (command) return `${command.usage}  ·  ${command.summary}`;

    const candidates = ALL_VERBS.filter((v) => v.startsWith(verb));
    if (candidates.length === 0) return `Unknown command "${verb}"`;
    return candidates.slice(0, MAX_HINTS).join('  ');
  });

  // Keep the newest line in view as the log grows.
  $effect(() => {
    void lines.length;
    if (logEl) logEl.scrollTop = logEl.scrollHeight;
  });

  /** Put the caret at the end after replacing the line programmatically. */
  async function caretToEnd() {
    await tick();
    input?.setSelectionRange(value.length, value.length);
  }

  function submit() {
    const command = value.trim();
    if (command === '') return;
    value = '';
    historyAt = -1;
    draft = '';
    resetCompletion();
    run(command);
  }

  function resetCompletion() {
    completionPrefix = undefined;
    completionAt = -1;
  }

  function complete(step: number) {
    if (completionPrefix === undefined) {
      completionPrefix = value.trim().split(/\s+/)[0]?.toUpperCase() ?? '';
    }
    const candidates = ALL_VERBS.filter((verb) => verb.startsWith(completionPrefix!));
    if (candidates.length === 0) return;

    completionAt = (completionAt + step + candidates.length) % candidates.length;
    const rest = value.trim().split(/\s+/).slice(1);
    value = [candidates[completionAt]!, ...rest].join(' ');
    void caretToEnd();
  }

  function walkHistory(step: number) {
    const history = workspace.history;
    if (history.length === 0) return;

    if (historyAt === -1) draft = value;
    // History is newest-first; index 0 is the most recent command.
    const next = Math.min(Math.max(historyAt + step, -1), history.length - 1);
    historyAt = next;
    value = next === -1 ? draft : (history[next] ?? '');
    void caretToEnd();
  }

  function onKeyDown(event: KeyboardEvent) {
    // Leave chords to the key router, so a global binding fires mid-word.
    if (event.ctrlKey || event.altKey || event.metaKey) return;

    switch (event.key) {
      case 'Enter':
        event.preventDefault();
        submit();
        return;
      case 'Tab':
        event.preventDefault();
        complete(event.shiftKey ? -1 : 1);
        return;
      case 'ArrowUp':
        event.preventDefault();
        walkHistory(1);
        return;
      case 'ArrowDown':
        event.preventDefault();
        walkHistory(-1);
        return;
      case 'Escape':
        event.preventDefault();
        // First Esc clears the line; a second one, on an empty line, leaves the
        // prompt for NAV mode.
        if (value === '') onEscape?.();
        else {
          value = '';
          historyAt = -1;
        }
        resetCompletion();
        return;
      default:
        resetCompletion();
    }
  }

  export function focus() {
    input?.focus();
  }

  export function blur() {
    input?.blur();
  }

  export function prefill(text: string) {
    value = text;
    void caretToEnd();
    input?.focus();
  }
</script>

<div class="command-bar">
  <div class="message-log" bind:this={logEl}>
    {#each lines as line (line.id)}
      <div class="log-line {line.level}">
        <span class="log-time dim">{line.at}</span>
        <span class="log-text">{line.message}</span>
      </div>
    {/each}
  </div>

  <div class="hint-line dim">{hint}</div>

  <div class="prompt-row">
    <span class="prompt-mark">&gt;</span>
    <!-- svelte-ignore a11y_autofocus -->
    <input
      bind:this={input}
      bind:value
      class="prompt-input mono"
      type="text"
      autocomplete="off"
      autocapitalize="off"
      autocorrect="off"
      spellcheck="false"
      aria-label="Command"
      autofocus
      onkeydown={onKeyDown}
    />
  </div>
</div>

<style>
  /* The log grows upward from the prompt, so the newest line sits nearest it. */
  .message-log {
    display: flex;
    flex-direction: column;
    justify-content: flex-end;
  }
</style>
