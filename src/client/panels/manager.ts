/**
 * Workspace: the tiled grid of panels.
 *
 * Layout is a column count (1–4); panels flow into rows automatically, so the
 * grid never has holes and adding a panel never re-sorts the ones already
 * there. Panels are keyed, so re-running a command for a ticker you are already
 * watching focuses and refreshes that panel instead of opening a duplicate —
 * which is what makes `GP X` safe to mash.
 */

import { el } from '../lib/dom.js';
import type { Panel, PanelContext } from './panel.js';

const MAX_PANELS = 12;

export class PanelManager {
  readonly root: HTMLElement;
  readonly #panels: Panel[] = [];
  #focusedId: string | undefined;
  #columns = 2;
  #onChange: (() => void) | undefined;

  constructor() {
    this.root = el('div', { class: 'workspace', 'data-columns': '2' });

    // Focus follows the click, anywhere inside a panel.
    this.root.addEventListener('mousedown', (event) => {
      const panelEl = (event.target as HTMLElement | null)?.closest<HTMLElement>('.panel');
      const id = panelEl?.dataset['panelId'];
      if (id) this.focus(id);
    });

    this.root.addEventListener('panel-close', (event) => {
      const panelEl = (event.target as HTMLElement | null)?.closest<HTMLElement>('.panel');
      const id = panelEl?.dataset['panelId'];
      if (id) this.close(id);
    });

    // Charts need an explicit resize signal; the grid gives them no event.
    const observer = new ResizeObserver(() => {
      for (const panel of this.#panels) panel.onResize();
    });
    observer.observe(this.root);
  }

  onChange(handler: () => void): void {
    this.#onChange = handler;
  }

  get panels(): readonly Panel[] {
    return this.#panels;
  }

  get columns(): number {
    return this.#columns;
  }

  get focused(): Panel | undefined {
    return this.#panels.find((p) => p.id === this.#focusedId);
  }

  find(id: string): Panel | undefined {
    return this.#panels.find((p) => p.id === id);
  }

  /**
   * Add a panel, or focus and refresh the existing one with the same id.
   *
   * `factory` is a thunk so an existing panel is never constructed only to be
   * thrown away — construction can be expensive (a chart allocates canvases).
   */
  open(id: string, factory: () => Panel): Panel {
    const existing = this.find(id);
    if (existing) {
      this.focus(id);
      void existing.refresh();
      return existing;
    }

    const panel = factory();
    this.#panels.push(panel);
    this.root.append(panel.root);
    panel.mount();
    this.focus(panel.id);

    // Oldest-first eviction keeps a long session from accumulating timers.
    while (this.#panels.length > MAX_PANELS) {
      const evicted = this.#panels.find((p) => p.id !== panel.id);
      if (!evicted) break;
      this.close(evicted.id, { silent: true });
    }

    this.#onChange?.();
    return panel;
  }

  close(id: string, options: { silent?: boolean } = {}): boolean {
    const index = this.#panels.findIndex((p) => p.id === id);
    if (index === -1) return false;

    const [panel] = this.#panels.splice(index, 1);
    panel!.destroy();

    if (this.#focusedId === id) {
      const next = this.#panels[Math.min(index, this.#panels.length - 1)];
      this.#focusedId = undefined;
      if (next) this.focus(next.id);
    }

    if (!options.silent) this.#onChange?.();
    return true;
  }

  closeAll(): number {
    const count = this.#panels.length;
    for (const panel of this.#panels.splice(0)) panel.destroy();
    this.#focusedId = undefined;
    this.#onChange?.();
    return count;
  }

  focus(id: string): void {
    if (this.#focusedId === id) return;
    this.#focusedId = id;
    for (const panel of this.#panels) {
      panel.root.classList.toggle('is-focused', panel.id === id);
    }
    this.focused?.onFocus();
    this.#onChange?.();
  }

  /** Move focus by `delta` panels, wrapping. Bound to Tab / Shift-Tab. */
  cycleFocus(delta: number): void {
    if (this.#panels.length === 0) return;
    const current = this.#panels.findIndex((p) => p.id === this.#focusedId);
    const next = (current + delta + this.#panels.length) % this.#panels.length;
    this.focus(this.#panels[next]!.id);
  }

  setColumns(columns: number): void {
    this.#columns = Math.min(Math.max(Math.trunc(columns), 1), 4);
    this.root.dataset['columns'] = String(this.#columns);
    // The grid template changed, so every chart needs to re-measure.
    requestAnimationFrame(() => {
      for (const panel of this.#panels) panel.onResize();
    });
    this.#onChange?.();
  }

  refreshAll(): void {
    for (const panel of this.#panels) void panel.refresh();
  }
}

export type { PanelContext };
