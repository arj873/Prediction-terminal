/**
 * Marks an element as somewhere the row cursor can land.
 *
 * The rule the terminal has always followed is that anything the mouse can
 * click, the keyboard can reach. This action is how a row opts in: it registers
 * with its panel's cursor, carries the command it runs so `$` can be answered
 * without parsing a data attribute back out of the DOM, and moves the caret to
 * itself when clicked — so a reader who reaches for the mouse once does not then
 * have to walk the cursor back down.
 */

import type { Action } from 'svelte/action';

import type { RowCursor } from '../panels/cursor.svelte';

export interface NavigableParams {
  /** The panel's cursor. Read from context by the panel, passed in here. */
  cursor: RowCursor;
  /** The command this row runs when activated. */
  command?: string;
  /** The row's second action, reached by `Alt+Backspace` or `d` in NAV mode. */
  action?: () => void;
  /** Run instead of clicking, when the row is not a plain click target. */
  activate?: () => void;
}

export const navigable: Action<HTMLElement, NavigableParams> = (element, params) => {
  let current = params;

  const onClick = () => current.cursor.focusElement(element, { scroll: false });
  element.addEventListener('click', onClick);
  current.cursor.register({ element, ...entryOf(current) });

  return {
    update(next: NavigableParams) {
      if (next.cursor !== current.cursor) {
        current.cursor.unregister(element);
        next.cursor.register({ element, ...entryOf(next) });
      } else {
        next.cursor.update({ element, ...entryOf(next) });
      }
      current = next;
    },
    destroy() {
      element.removeEventListener('click', onClick);
      current.cursor.unregister(element);
    },
  };
};

function entryOf(params: NavigableParams) {
  return {
    command: params.command,
    action: params.action,
    activate: params.activate,
  };
}
