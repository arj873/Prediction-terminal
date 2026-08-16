/**
 * The command line: input, history, completion, and the message log above it.
 *
 * Deliberately keyboard-first. History is a cursor over the persisted buffer
 * with the in-progress line stashed at position 0, so walking up and back down
 * returns you to exactly what you were typing.
 */

import { el } from '../lib/dom.js';
import type { Workspace } from '../state.js';
import { ALL_VERBS, COMMAND_INDEX } from './registry.js';
import { parse } from './parser.js';

export type LogLevel = 'info' | 'warn' | 'error' | 'echo';

const MAX_LOG_LINES = 300;

export class CommandLine {
  readonly root: HTMLElement;
  readonly #input: HTMLInputElement;
  readonly #log: HTMLElement;
  readonly #hint: HTMLElement;
  readonly #workspace: Workspace;
  readonly #onSubmit: (command: string) => void;

  /** 0 = the live line; 1..n = history, counting back from the most recent. */
  #historyCursor = 0;
  #draft = '';
  /** Cycles through candidates on repeated Tab presses. */
  #completionIndex = 0;
  #completionPrefix: string | null = null;

  constructor(workspace: Workspace, onSubmit: (command: string) => void) {
    this.#workspace = workspace;
    this.#onSubmit = onSubmit;

    this.#log = el('div', { class: 'message-log', role: 'log', 'aria-live': 'polite' });
    this.#hint = el('div', { class: 'command-hint' });

    this.#input = el('input', {
      class: 'command-input',
      type: 'text',
      autocomplete: 'off',
      autocapitalize: 'off',
      autocorrect: 'off',
      spellcheck: 'false',
      'aria-label': 'Command line',
      placeholder: 'Type HELP and press Enter',
    });

    this.#input.addEventListener('keydown', (event) => this.#onKeyDown(event));
    this.#input.addEventListener('input', () => {
      this.#completionPrefix = null;
      this.#updateHint();
    });

    this.root = el('div', { class: 'command-bar' }, [
      this.#log,
      this.#hint,
      el('div', { class: 'command-row' }, [
        el('span', { class: 'command-prompt', text: '>' }),
        this.#input,
      ]),
    ]);
  }

  focus(): void {
    this.#input.focus();
  }

  /** Put text on the line without running it — used by "click to fill". */
  setValue(value: string): void {
    this.#input.value = value;
    this.#historyCursor = 0;
    this.#updateHint();
    this.focus();
  }

  log(message: string, level: LogLevel = 'info'): void {
    const now = new Date();
    const time = `${String(now.getUTCHours()).padStart(2, '0')}:${String(now.getUTCMinutes()).padStart(2, '0')}:${String(now.getUTCSeconds()).padStart(2, '0')}`;

    this.#log.append(
      el('div', { class: `log-line log-${level}` }, [
        el('span', { class: 'log-time', text: time }),
        el('span', { class: 'log-message', text: message }),
      ]),
    );

    while (this.#log.childElementCount > MAX_LOG_LINES) {
      this.#log.firstElementChild?.remove();
    }
    this.#log.scrollTop = this.#log.scrollHeight;
  }

  clearLog(): void {
    this.#log.replaceChildren();
  }

  /* ------------------------------------------------------------- keyboard */

  #onKeyDown(event: KeyboardEvent): void {
    switch (event.key) {
      case 'Enter': {
        event.preventDefault();
        this.#submit();
        return;
      }
      case 'ArrowUp': {
        event.preventDefault();
        this.#walkHistory(1);
        return;
      }
      case 'ArrowDown': {
        event.preventDefault();
        this.#walkHistory(-1);
        return;
      }
      case 'Tab': {
        event.preventDefault();
        this.#complete(event.shiftKey ? -1 : 1);
        return;
      }
      case 'Escape': {
        event.preventDefault();
        this.#input.value = '';
        this.#historyCursor = 0;
        this.#completionPrefix = null;
        this.#updateHint();
        return;
      }
      default:
        break;
    }

    if (event.ctrlKey && event.key.toLowerCase() === 'l') {
      event.preventDefault();
      this.clearLog();
    }
  }

  #submit(): void {
    const value = this.#input.value.trim();
    if (!value) return;

    this.log(value, 'echo');
    this.#workspace.pushHistory(value);
    this.#input.value = '';
    this.#historyCursor = 0;
    this.#draft = '';
    this.#completionPrefix = null;
    this.#updateHint();

    this.#onSubmit(value);
  }

  #walkHistory(direction: 1 | -1): void {
    const history = this.#workspace.history;
    if (history.length === 0) return;

    // Stash the live line the first time we leave it.
    if (this.#historyCursor === 0 && direction === 1) this.#draft = this.#input.value;

    const next = Math.min(Math.max(this.#historyCursor + direction, 0), history.length);
    if (next === this.#historyCursor) return;
    this.#historyCursor = next;

    this.#input.value = next === 0 ? this.#draft : (history[history.length - next] ?? '');
    // Put the caret at the end, where you would keep typing.
    requestAnimationFrame(() => {
      this.#input.setSelectionRange(this.#input.value.length, this.#input.value.length);
    });
    this.#updateHint();
  }

  /**
   * Complete the verb only.
   *
   * Arguments are tickers and free text — there is no closed set to complete
   * from, and guessing at one would be worse than leaving it alone.
   */
  #complete(direction: 1 | -1): void {
    const value = this.#input.value;
    // Only complete while still on the first token.
    if (/\s/.test(value.trim()) || value.trimStart() !== value) return;

    if (this.#completionPrefix === null) {
      this.#completionPrefix = value.toUpperCase();
      this.#completionIndex = direction === 1 ? -1 : 0;
    }

    const prefix = this.#completionPrefix;
    const candidates = prefix
      ? ALL_VERBS.filter((verb) => verb.startsWith(prefix))
      : [...ALL_VERBS];

    if (candidates.length === 0) return;

    this.#completionIndex =
      (this.#completionIndex + direction + candidates.length) % candidates.length;
    this.#input.value = candidates[this.#completionIndex] ?? value;
    this.#updateHint(candidates);
  }

  /** Show the usage line for the verb being typed, plus completion candidates. */
  #updateHint(candidates?: string[]): void {
    const value = this.#input.value.trim();
    this.#hint.replaceChildren();

    if (!value) return;

    const { verb } = parse(value);
    const command = COMMAND_INDEX.get(verb);

    if (command) {
      this.#hint.append(
        el('span', { class: 'hint-usage', text: command.usage }),
        el('span', { class: 'hint-summary', text: command.summary }),
      );
      return;
    }

    const matches = candidates ?? ALL_VERBS.filter((v) => v.startsWith(verb));
    if (matches.length > 0 && matches.length <= 12) {
      this.#hint.append(el('span', { class: 'hint-candidates', text: matches.join('  ') }));
    } else if (matches.length === 0) {
      this.#hint.append(
        el('span', { class: 'hint-unknown', text: `Unknown command "${verb}" — try HELP` }),
      );
    }
  }
}
