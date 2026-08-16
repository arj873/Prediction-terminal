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
 */

import { el } from '../lib/dom.js';
import { ApiRequestError } from '../lib/api.js';

export interface PanelContext {
  /** Run a terminal command as if typed — used by clickable rows. */
  run(command: string): void;
  /** Print a line to the message log. */
  log(message: string, level?: 'info' | 'warn' | 'error'): void;
}

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
  #timer: number | undefined;
  #controller: AbortController | undefined;
  #destroyed = false;
  #lastLoadedAt: number | undefined;

  /** Poll interval in ms. `0` disables polling — for static content. */
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

  /* -------------------------------------------------------------- frame */

  #buildFrame(): HTMLElement {
    this.#titleEl = el('span', { class: 'panel-title' });
    this.#subtitleEl = el('span', { class: 'panel-subtitle' });
    this.#statusEl = el('span', { class: 'panel-status' });

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
      this.#titleEl,
      this.#subtitleEl,
      el('span', { class: 'panel-spacer' }),
      this.#statusEl,
      close,
    ]);

    return el('section', { class: 'panel', tabindex: '-1', 'data-panel-id': this.id }, [
      header,
      this.body,
    ]);
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
    if (this.refreshMs > 0) {
      this.#timer = window.setInterval(() => void this.refresh(), this.refreshMs);
    }
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

      this.body.replaceChildren();
      this.render(data);
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
