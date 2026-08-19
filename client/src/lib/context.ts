/**
 * The two contexts the terminal passes down.
 *
 * `TerminalContext` is the app's services — running a command, printing to the
 * log, the workspace and panel stores. Every panel needs at least `run`, because
 * a clickable row *is* a command.
 *
 * `RowCursor` is per panel, and is what the `navigable` action registers with.
 * It is separate because a panel's rows belong to that panel: two panels each
 * have a caret, and only the focused one's moves.
 */

import { getContext, setContext } from 'svelte';

import type { RowCursor } from './panels/cursor.svelte';
import type { PanelStore } from './state/panels.svelte';
import type { Workspace } from './state/workspace.svelte';

export type LogLevel = 'info' | 'warn' | 'error' | 'echo';

export interface TerminalContext {
  /** Run a terminal command as if it had been typed. */
  run(command: string): void;
  /** Print a line to the message log. */
  log(message: string, level?: LogLevel): void;
  panels: PanelStore;
  workspace: Workspace;
}

const TERMINAL = Symbol('terminal');
const CURSOR = Symbol('row-cursor');

export function setTerminalContext(context: TerminalContext): TerminalContext {
  return setContext(TERMINAL, context);
}

export function getTerminalContext(): TerminalContext {
  const context = getContext<TerminalContext | undefined>(TERMINAL);
  if (!context) {
    throw new Error('No terminal context — this component must render inside the terminal root');
  }
  return context;
}

export function setRowCursor(cursor: RowCursor): RowCursor {
  return setContext(CURSOR, cursor);
}

export function getRowCursor(): RowCursor {
  const cursor = getContext<RowCursor | undefined>(CURSOR);
  if (!cursor) {
    throw new Error('No row cursor — a navigable element must render inside a PanelFrame');
  }
  return cursor;
}
