/**
 * The Panel base class.
 *
 * A panel owns a tile in the workspace grid and, usually, a polling loop. The
 * lifecycle contract is what keeps a long session from leaking:
 *
 *   mount()    → build DOM once, then `refresh()`
 *   refresh()  → fetch and render; called on a timer and on demand
 *   destroy()  → stop the timer, abort in-flight requests, drop DOM
 *
 * Subclasses implement `load()` and `render()`. They never touch timers or
 * AbortControllers themselves — `refresh()` cancels any previous request before
 * starting a new one, so a slow response can never overwrite a newer one.
 *
 * The row cursor lives here too, for the same reason the frame does: every
 * panel renders rows that run a command when clicked, so every panel gets a
 * keyboard cursor over them for free, and `Enter` reaches whatever the mouse
 * could.
 */

import { el } from '../lib/dom.js';
import { ApiRequestError } from '../lib/api.js';
import { looksLikeTicker, parse } from '../terminal/parser.js';

export interface PanelContext {
  /** Run a terminal command as if typed — used by clickable rows. */
  run(command: string): void;
  /** Print a line to the message log. */
  log(message: string, level?: 'info' | 'warn' | 'error'): void;
}

/**
 * Everything a keyboard can land on inside a panel body.
 *
 * Deliberately the same set the mouse can click, matched by the classes the
 * panels already put on those elements — a row that gained a click handler is
 * navigable the day it gains it, with nothing else to remember.
 */
const NAVIGABLE = '.clickable, .group-header, .picker-chip, .example, .action, .notes > summary';

/** How far `ROW NEXT` scrolls a panel that has no rows to move through. */
const SCROLL_STEP = 56;

export abstract class Panel<T = unknown> {
  /** Stable id, used for focus and the `CLS` command. */
  readonly id: string;
  /** Shown in the panel title bar. */
  abstract readonly kind: string;

  readonly root: HTMLElement;
  protected readonly body: HTMLElement;
  protected readonly context: PanelContext;

  #titleEl!: HTMLElement;
  #subtitleEl!: HTMLElement;
  #statusEl!: HTMLElement;
  #indexEl!: HTMLElement;
  #timer: number | undefined;
  #controller: AbortController | undefined;
  #destroyed = false;
  #lastLoadedAt: number | undefined;
  #lastData: T | undefined;
  /** Row under the keyboard cursor, as an index into {@link #navigable}. */
  #cursor = -1;

  /**
   * Poll interval in ms. `0` disables polling — for static content.
   *
   * Assign freely in a constructor, which runs before {@link mount} arms the
   * timer. After mounting, go through {@link setRefreshMs}: the timer is
   * already running and a bare assignment would not be read again.
   */
  protected refreshMs = 0;

  constructor(id: string, context: PanelContext) {
    this.id = id;
    this.context = context;
    this.body = el('div', { class: 'panel-body' });
    this.root = this.#buildFrame();
  }

  /** Fetch this panel's data. Must honour `signal`. */
  protected abstract load(signal: AbortSignal): Promise<T>;

  /** Render loaded data into `this.body`. Called only on success. */
  protected abstract render(data: T): void;

  /** Title bar text. Recomputed after every successful load. */
  protected abstract title(): string;

  /** Optional dimmer text after the title. */
  protected subtitle(): string {
    return '';
  }

  /**
   * The most recent successful load, or `undefined` before the first one.
   *
   * `title()` and `subtitle()` are called once from {@link mount} before any
   * data exists, and again after every successful load — so both have to cope
   * with having nothing. Holding the payload here saves every subclass from
   * keeping its own copy just to answer those two questions.
   */
  protected get latest(): T | undefined {
    return this.#lastData;
  }

  /**
   * What `$` means in a key binding while this panel is focused — its ticker,
   * symbol or series id.
   *
   * Panels built around one market override this; the list panels do not need
   * to, because their rows answer the question themselves (see
   * {@link cursorSubject}).
   */
  subject(): string | undefined {
    return undefined;
  }

  /* -------------------------------------------------------------- frame */

  #buildFrame(): HTMLElement {
    this.#titleEl = el('span', { class: 'panel-title' });
    this.#subtitleEl = el('span', { class: 'panel-subtitle' });
    this.#statusEl = el('span', { class: 'panel-status' });
    this.#indexEl = el('span', { class: 'panel-index' });

    const close = el('button', {
      class: 'panel-close',
      type: 'button',
      title: 'Close panel',
      'aria-label': 'Close panel',
      text: '×',
    });
    close.addEventListener('click', (event) => {
      event.stopPropagation();
      this.root.dispatchEvent(new CustomEvent('panel-close', { bubbles: true }));
    });

    const header = el('div', { class: 'panel-header' }, [
      this.#indexEl,
      this.#titleEl,
      this.#subtitleEl,
      el('span', { class: 'panel-spacer' }),
      this.#statusEl,
      close,
    ]);

    // Clicking a row moves the keyboard cursor to it, so a reader who reaches
    // for the mouse once does not then have to walk the cursor back down.
    this.body.addEventListener('click', (event) => {
      const target = (event.target as HTMLElement | null)?.closest<HTMLElement>(NAVIGABLE);
      if (!target) return;
      const index = this.#navigable().indexOf(target);
      if (index !== -1) this.#setCursor(index, { scroll: false });
    });

    return el('section', { class: 'panel', tabindex: '-1', 'data-panel-id': this.id }, [
      header,
      this.body,
    ]);
  }

  /**
   * The panel's position in the workspace, shown in its header and typed as
   * `FOCUS <n>` — which is what `Alt+3` runs.
   */
  setIndex(index: number): void {
    this.#indexEl.textContent = String(index);
    this.root.dataset['index'] = String(index);
  }

  #setStatus(text: string, className = ''): void {
    this.#statusEl.textContent = text;
    this.#statusEl.className = `panel-status ${className}`.trim();
  }

  #updateHeader(): void {
    this.#titleEl.textContent = `${this.kind} ${this.title()}`.trim();
    this.#subtitleEl.textContent = this.subtitle();
  }

  /* ----------------------------------------------------------- lifecycle */

  mount(): void {
    this.#updateHeader();
    this.body.append(el('div', { class: 'panel-loading', text: 'LOADING…' }));
    void this.refresh();
    this.#armTimer();
  }

  /**
   * Change the poll interval on a panel that is already running.
   *
   * `mount()` reads `refreshMs` once, so a panel that re-cuts its own cadence
   * when reconfigured — a chart moving between minute and daily bars — has to
   * re-arm the timer rather than just assign the field.
   */
  protected setRefreshMs(ms: number): void {
    if (this.refreshMs === ms) return;
    this.refreshMs = ms;
    this.#armTimer();
  }

  #armTimer(): void {
    if (this.#timer !== undefined) window.clearInterval(this.#timer);
    this.#timer = undefined;
    if (this.#destroyed || this.refreshMs <= 0) return;
    this.#timer = window.setInterval(() => void this.refresh(), this.refreshMs);
  }

  async refresh(): Promise<void> {
    if (this.#destroyed) return;

    // Cancel any in-flight load so a slow reply cannot land after a fast one.
    this.#controller?.abort();
    const controller = new AbortController();
    this.#controller = controller;

    this.#setStatus('···', 'busy');

    try {
      const data = await this.load(controller.signal);
      if (this.#destroyed || controller.signal.aborted) return;

      // Recorded before `render()` so both it and the `title()`/`subtitle()`
      // pair below read the same payload.
      this.#lastData = data;

      this.body.replaceChildren();
      this.render(data);
      // The rows are new DOM, so the cursor has to be re-drawn onto them. It
      // survives by index rather than identity: a watchlist re-polls every six
      // seconds and losing your place that often would make the cursor useless.
      this.#setCursor(this.#cursor, { scroll: false });
      this.#updateHeader();
      this.#lastLoadedAt = Date.now();
      this.#setStatus(this.#stamp(), 'ok');
    } catch (err) {
      if (controller.signal.aborted || this.#destroyed) return;
      this.#renderError(err);
    } finally {
      if (this.#controller === controller) this.#controller = undefined;
    }
  }

  #stamp(): string {
    const now = new Date();
    return `${String(now.getUTCHours()).padStart(2, '0')}:${String(now.getUTCMinutes()).padStart(2, '0')}:${String(now.getUTCSeconds()).padStart(2, '0')}Z`;
  }

  /**
   * Render a failure in place.
   *
   * A panel that has never loaded shows the error as its whole body. One that
   * *has* loaded keeps its last good data on screen and only marks the status
   * red — a transient upstream blip should not blank a chart you are reading.
   */
  #renderError(err: unknown): void {
    const isApi = err instanceof ApiRequestError;
    const message = err instanceof Error ? err.message : String(err);
    const hint = isApi ? err.hint : undefined;

    this.#setStatus('ERR', 'error');

    if (this.#lastLoadedAt !== undefined) {
      this.#setStatus(`STALE · ${message}`, 'error');
      return;
    }

    this.body.replaceChildren(
      el('div', { class: 'panel-error' }, [
        el('div', { class: 'panel-error-title', text: message }),
        hint ? el('div', { class: 'panel-error-hint', text: hint }) : null,
      ]),
    );
  }

  /* ---------------------------------------------------------- row cursor */

  #navigable(): HTMLElement[] {
    return [...this.body.querySelectorAll<HTMLElement>(NAVIGABLE)];
  }

  #setCursor(index: number, options: { scroll?: boolean } = {}): void {
    const targets = this.#navigable();
    for (const target of targets) target.classList.remove('is-cursor');

    if (targets.length === 0 || index < 0) {
      this.#cursor = targets.length === 0 ? -1 : Math.min(Math.max(index, -1), targets.length - 1);
      return;
    }

    this.#cursor = Math.min(index, targets.length - 1);
    const target = targets[this.#cursor];
    if (!target) return;
    target.classList.add('is-cursor');
    if (options.scroll !== false) target.scrollIntoView({ block: 'nearest' });
  }

  /**
   * Move the row cursor, or scroll the body when there is nothing to move
   * through — a chart has no rows, and `j` should still take you down the
   * panel rather than doing nothing at all.
   */
  moveCursor(step: number | 'top' | 'end'): boolean {
    const targets = this.#navigable();

    if (targets.length === 0) {
      const body = this.body;
      if (step === 'top') body.scrollTop = 0;
      else if (step === 'end') body.scrollTop = body.scrollHeight;
      else body.scrollTop += step * SCROLL_STEP;
      return false;
    }

    if (step === 'top') this.#setCursor(0);
    else if (step === 'end') this.#setCursor(targets.length - 1);
    // From nowhere, a step up lands on the last row rather than refusing.
    else if (this.#cursor === -1) this.#setCursor(step > 0 ? 0 : targets.length - 1);
    else this.#setCursor(Math.min(Math.max(this.#cursor + step, 0), targets.length - 1));

    return true;
  }

  /** Activate the row under the cursor — the same code path a click takes. */
  activateCursor(): boolean {
    const target = this.#navigable()[this.#cursor];
    if (!target) return false;
    target.click();
    return true;
  }

  /**
   * The row's second action: the `×` on a watchlist row, the feed link on a
   * market row. Nothing happens on a row that has none.
   */
  activateRowAction(): boolean {
    const target = this.#navigable()[this.#cursor];
    const action = target?.querySelector<HTMLElement>('.row-action');
    if (!action) return false;
    action.click();
    return true;
  }

  /**
   * What `$` means with the cursor on a row.
   *
   * Rows record the command they run, and the first argument of that command is
   * the thing the row is about — so `OB $` on a leaderboard row means the book
   * for *that* market. A row whose command takes words rather than a reference
   * (a Billboard entry searching for its own title) answers nothing, and `$`
   * falls back to the panel.
   */
  cursorSubject(): string | undefined {
    const target = this.#navigable()[this.#cursor];
    const command = target?.dataset['command'];
    if (!command) return undefined;

    const argument = parse(command).args[0];
    return looksLikeTicker(argument) ? argument : undefined;
  }

  destroy(): void {
    this.#destroyed = true;
    if (this.#timer !== undefined) window.clearInterval(this.#timer);
    this.#timer = undefined;
    this.#controller?.abort();
    this.#controller = undefined;
    this.root.remove();
  }

  /** Called by the manager when this panel takes focus. */
  onFocus(): void {}

  /** Called after the tile is resized, so charts can re-fit. */
  onResize(): void {}
}
