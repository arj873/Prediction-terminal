/**
 * The row cursor: a keyboard caret over whatever a panel made clickable.
 *
 * Every panel renders rows that run a command when clicked, so every panel gets
 * a keyboard cursor over them for free and `Enter` reaches whatever the mouse
 * could. The old implementation found those rows by running a `querySelectorAll`
 * over the panel body on every move, matching a list of CSS classes — which
 * meant a row became navigable the day it gained a click handler, with nothing
 * else to remember, but also meant the DOM was the source of truth for what a
 * row *is*.
 *
 * Here a row registers itself instead, through the `navigable` action, carrying
 * the command it runs. That keeps the same "anything clickable is navigable"
 * property while letting `$` be answered from the registration rather than
 * parsed back out of a data attribute.
 *
 * Ordering is still resolved from the document, because that is genuinely where
 * it lives: rows are registered in mount order, which is not reading order once
 * a table re-renders a middle row.
 */

import { looksLikeTicker, parse } from '../terminal/parser';

/** How far a step scrolls a panel that has no rows to move through. */
const SCROLL_STEP = 56;

export interface NavigableEntry {
  element: HTMLElement;
  /** The command this row runs when activated. */
  command?: string;
  /** A second action, if the row has one — the `×` on a watchlist row. */
  action?: () => void;
  /** Run instead of clicking, when the row is not a plain button. */
  activate?: () => void;
}

export class RowCursor {
  readonly #entries = new Map<HTMLElement, NavigableEntry>();
  /** Position in the *ordered* list, kept across re-renders by index. */
  #index = -1;
  /** The panel body, for the scroll fallback. */
  #scrollHost: HTMLElement | undefined;

  setScrollHost(element: HTMLElement | undefined): void {
    this.#scrollHost = element;
  }

  register(entry: NavigableEntry): void {
    this.#entries.set(entry.element, entry);
    // A row that appears under the caret's index should wear the highlight
    // immediately, not after the next keypress.
    this.#paint();
  }

  update(entry: NavigableEntry): void {
    if (this.#entries.has(entry.element)) this.#entries.set(entry.element, entry);
  }

  unregister(element: HTMLElement): void {
    this.#entries.delete(element);
    element.classList.remove('is-cursor');
  }

  /**
   * Registered rows in reading order.
   *
   * Registration order is mount order, which diverges from document order as
   * soon as a list re-renders one row in place, so the document is asked.
   */
  ordered(): NavigableEntry[] {
    return [...this.#entries.values()].sort((a, b) => {
      if (a.element === b.element) return 0;
      const relation = a.element.compareDocumentPosition(b.element);
      if (relation & Node.DOCUMENT_POSITION_FOLLOWING) return -1;
      if (relation & Node.DOCUMENT_POSITION_PRECEDING) return 1;
      return 0;
    });
  }

  get index(): number {
    return this.#index;
  }

  /** Put the caret on a specific row — what a click does. */
  focusElement(element: HTMLElement, options: { scroll?: boolean } = {}): void {
    const index = this.ordered().findIndex((entry) => entry.element === element);
    if (index !== -1) this.#setIndex(index, options);
  }

  #setIndex(index: number, options: { scroll?: boolean } = {}): void {
    const rows = this.ordered();
    for (const row of rows) row.element.classList.remove('is-cursor');

    if (rows.length === 0) {
      this.#index = -1;
      return;
    }
    if (index < 0) {
      this.#index = -1;
      return;
    }

    this.#index = Math.min(index, rows.length - 1);
    const target = rows[this.#index]!.element;
    target.classList.add('is-cursor');
    if (options.scroll !== false) target.scrollIntoView({ block: 'nearest' });
  }

  /** Re-apply the highlight after the rows underneath it changed. */
  #paint(): void {
    if (this.#index >= 0) this.#setIndex(this.#index, { scroll: false });
  }

  /**
   * Move the caret, or scroll the body when there is nothing to move through —
   * a chart has no rows, and `j` should still take you down the panel rather
   * than doing nothing at all.
   */
  move(step: number | 'top' | 'end'): boolean {
    const rows = this.ordered();

    if (rows.length === 0) {
      const host = this.#scrollHost;
      if (!host) return false;
      if (step === 'top') host.scrollTop = 0;
      else if (step === 'end') host.scrollTop = host.scrollHeight;
      else host.scrollTop += step * SCROLL_STEP;
      return false;
    }

    if (step === 'top') this.#setIndex(0);
    else if (step === 'end') this.#setIndex(rows.length - 1);
    // From nowhere, a step up lands on the last row rather than refusing.
    else if (this.#index === -1) this.#setIndex(step > 0 ? 0 : rows.length - 1);
    else this.#setIndex(Math.min(Math.max(this.#index + step, 0), rows.length - 1));

    return true;
  }

  #current(): NavigableEntry | undefined {
    return this.ordered()[this.#index];
  }

  /** Activate the row under the caret — the same path a click takes. */
  activate(): boolean {
    const entry = this.#current();
    if (!entry) return false;
    if (entry.activate) entry.activate();
    else entry.element.click();
    return true;
  }

  /**
   * The row's second action: the `×` on a watchlist row, the feed link on a
   * market row. Nothing happens on a row that has none.
   */
  activateAction(): boolean {
    const entry = this.#current();
    if (!entry) return false;
    if (entry.action) {
      entry.action();
      return true;
    }
    // Rows that render their own nested control rather than declaring one.
    const nested = entry.element.querySelector<HTMLElement>('.row-action');
    if (!nested) return false;
    nested.click();
    return true;
  }

  /**
   * What `$` means with the caret on a row.
   *
   * A row records the command it runs, and the first argument of that command
   * is the thing the row is about — so `OB $` on a leaderboard row means the
   * book for *that* market. A row whose command takes words rather than a
   * reference (a Billboard entry searching for its own title) answers nothing,
   * and `$` falls back to the panel.
   */
  subject(): string | undefined {
    const command = this.#current()?.command;
    if (!command) return undefined;
    const argument = parse(command).args[0];
    return looksLikeTicker(argument) ? argument : undefined;
  }

  /** Drop every registration. Called when a panel unmounts. */
  clear(): void {
    for (const element of this.#entries.keys()) element.classList.remove('is-cursor');
    this.#entries.clear();
    this.#index = -1;
  }
}
