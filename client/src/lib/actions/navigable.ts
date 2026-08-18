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
  /**
   * Whether this element is a cursor stop at all. Default `true`.
   *
   * A table that renders inert rows — the key crib in `HELP`, the reference
   * tables in `KEYS` — passes `false` for them, because a caret that stops
   * somewhere `Enter` does nothing is worse than one that skips it. The old
   * client got this for free by matching a `.clickable` class that only
   * command-bound rows carried.
   */
  enabled?: boolean;
}

export const navigable: Action<HTMLElement, NavigableParams> = (element, params) => {
  let current = params;
  let registered = false;

  const onClick = () => current.cursor.focusElement(element, { scroll: false });

  function apply(next: NavigableParams) {
    const wanted = next.enabled ?? true;

    if (!wanted) {
      if (registered) {
        next.cursor.unregister(element);
        element.removeEventListener('click', onClick);
        registered = false;
      }
      return;
    }

    if (!registered) {
      element.addEventListener('click', onClick);
      next.cursor.register({ element, ...entryOf(next) });
      registered = true;
      return;
    }

    if (next.cursor !== current.cursor) {
      current.cursor.unregister(element);
      next.cursor.register({ element, ...entryOf(next) });
    } else {
      next.cursor.update({ element, ...entryOf(next) });
    }
  }

  apply(params);

  return {
    update(next: NavigableParams) {
      apply(next);
      current = next;
    },
    destroy() {
      element.removeEventListener('click', onClick);
      if (registered) current.cursor.unregister(element);
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
