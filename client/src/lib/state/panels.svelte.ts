/**
 * The workspace: a tiled grid of panels.
 *
 * Layout is a column count (1–4); panels flow into rows automatically, so the
 * grid never has holes and adding a panel never re-sorts the ones already
 * there. Panels are keyed, so re-running a command for a ticker you are already
 * watching focuses and reconfigures that panel instead of opening a duplicate —
 * which is what makes `GP X` safe to mash.
 *
 * A panel here is a *description*, not a component instance: an id, a kind, and
 * the props it was opened with. The grid renders each through a keyed `{#each}`,
 * so Svelte owns the component lifecycle and re-running a command with new
 * arguments becomes a prop change rather than a teardown. That is the one real
 * departure from the class-based manager this replaces, and it is what removes
 * the old "reconfigure in place or rebuild?" branch from every command handler.
 */

const MAX_PANELS = 12;
const MIN_COLUMNS = 1;
const MAX_COLUMNS = 4;

/** Every kind of panel the terminal can open. */
export type PanelKind =
  | 'quote'
  | 'depth'
  | 'trades'
  | 'chart'
  | 'search'
  | 'event'
  | 'top'
  | 'watchlist'
  | 'spot'
  | 'linked-series'
  | 'compare'
  | 'news'
  | 'fred'
  | 'fred-search'
  | 'billboard'
  | 'billboard-charts'
  | 'ent'
  | 'rt'
  | 'rt-search'
  | 'netflix'
  | 'stream-chart'
  | 'boxoffice'
  | 'steam'
  | 'tv'
  | 'awards'
  | 'trends'
  | 'releases'
  | 'podcasts'
  | 'help'
  | 'keys';

/**
 * A panel, as the workspace knows it.
 *
 * `props` is whatever the panel component takes. It is kept plain and
 * serialisable so a panel is fully described by its row here — nothing about a
 * live panel exists only inside its component.
 */
export interface PanelDesc<P extends Record<string, unknown> = Record<string, unknown>> {
  id: string;
  kind: PanelKind;
  props: P;
}

/** What a panel exposes to the workspace once it is on screen. */
export interface PanelHandle {
  /** Re-run this panel's load now. */
  refresh(): void;
  /** Move the row cursor. `false` when there are no rows to move through. */
  moveCursor(step: number | 'top' | 'end'): boolean;
  /** Activate the row under the cursor — the path a click takes. */
  activateCursor(): boolean;
  /** The row's second action: the `×` on a watchlist row, a feed link. */
  activateRowAction(): boolean;
  /** What `$` means with the cursor on a row. */
  cursorSubject(): string | undefined;
  /** What `$` means for the panel as a whole. */
  subject(): string | undefined;
  /** The tile's focusable element, so NAV mode has somewhere to put focus. */
  element(): HTMLElement | undefined;
}

export class PanelStore {
  #panels = $state<PanelDesc[]>([]);
  #focusedId = $state<string | undefined>(undefined);
  #zoomedId = $state<string | undefined>(undefined);
  #columns = $state(2);

  /** Live handles, registered by each panel component as it mounts. */
  readonly #handles = new Map<string, PanelHandle>();

  get panels(): readonly PanelDesc[] {
    return this.#panels;
  }

  get columns(): number {
    return this.#columns;
  }

  get focusedId(): string | undefined {
    return this.#focusedId;
  }

  get zoomedId(): string | undefined {
    return this.#zoomedId;
  }

  get count(): number {
    return this.#panels.length;
  }

  get focused(): PanelDesc | undefined {
    return this.#panels.find((panel) => panel.id === this.#focusedId);
  }

  find(id: string): PanelDesc | undefined {
    return this.#panels.find((panel) => panel.id === id);
  }

  /** The tile's position, counting from 1 as the header badges do. */
  indexOf(id: string): number {
    return this.#panels.findIndex((panel) => panel.id === id) + 1;
  }

  /* ------------------------------------------------------------ handles */

  /**
   * Called by a panel component on mount, and again on destroy with
   * `undefined`. Everything that reaches *into* a live panel — refreshing it,
   * walking its rows — goes through here rather than through the DOM.
   */
  register(id: string, handle: PanelHandle | undefined): void {
    if (handle) this.#handles.set(id, handle);
    else this.#handles.delete(id);
  }

  handle(id: string | undefined): PanelHandle | undefined {
    return id === undefined ? undefined : this.#handles.get(id);
  }

  get focusedHandle(): PanelHandle | undefined {
    return this.handle(this.#focusedId);
  }

  /* --------------------------------------------------------- open/close */

  /**
   * Add a panel, or update the existing one with the same id.
   *
   * Re-running a command for a panel that is already open focuses it and
   * replaces its props. With a keyed `{#each}` that is a prop change, so a
   * chart keeps its instance and its visible range rather than being rebuilt —
   * the behaviour the old manager had to special-case per panel type.
   *
   * `merge` folds the incoming props into the existing ones. `IMP` uses it to
   * add an overlay to a spot panel already on screen instead of opening a
   * second one.
   */
  open<P extends Record<string, unknown>>(
    desc: PanelDesc<P>,
    merge?: (existing: P, incoming: P) => P,
  ): void {
    const existing = this.#panels.find((panel) => panel.id === desc.id);

    if (existing) {
      existing.props = merge
        ? (merge(existing.props as P, desc.props) as Record<string, unknown>)
        : desc.props;
      this.focus(desc.id);
      this.handle(desc.id)?.refresh();
      return;
    }

    this.#panels.push(desc as PanelDesc);
    this.focus(desc.id);

    // Oldest-first eviction keeps a long session from accumulating pollers.
    while (this.#panels.length > MAX_PANELS) {
      const evicted = this.#panels.find((panel) => panel.id !== desc.id);
      if (!evicted) break;
      this.close(evicted.id);
    }
  }

  close(id: string): boolean {
    const index = this.#panels.findIndex((panel) => panel.id === id);
    if (index === -1) return false;

    this.#panels.splice(index, 1);
    this.#handles.delete(id);
    if (this.#zoomedId === id) this.#zoomedId = undefined;

    if (this.#focusedId === id) {
      // Focus the tile that slid into this one's place, or the last one.
      const next = this.#panels[Math.min(index, this.#panels.length - 1)];
      this.#focusedId = next?.id;
    }

    return true;
  }

  closeAll(): number {
    const count = this.#panels.length;
    this.#panels = [];
    this.#handles.clear();
    this.#focusedId = undefined;
    this.#zoomedId = undefined;
    return count;
  }

  /* -------------------------------------------------------------- focus */

  focus(id: string): void {
    if (this.find(id)) this.#focusedId = id;
  }

  /** Move focus by `delta` tiles, wrapping. */
  cycleFocus(delta: number): void {
    if (this.#panels.length === 0) return;
    const current = this.#panels.findIndex((panel) => panel.id === this.#focusedId);
    const next = (current + delta + this.#panels.length) % this.#panels.length;
    this.focus(this.#panels[next]!.id);
  }

  /** Focus the nth tile, counting from 1 as the header badges do. */
  focusAt(position: number): boolean {
    const panel = this.#panels[position - 1];
    if (!panel) return false;
    this.focus(panel.id);
    return true;
  }

  /**
   * Hand the browser's keyboard focus to the focused tile.
   *
   * NAV mode needs a real focus target: without one the prompt keeps it and
   * every letter meant for the workspace lands in the command line instead.
   */
  focusElement(): boolean {
    const panel = this.focused ?? this.#panels[0];
    if (!panel) return false;
    this.focus(panel.id);
    const element = this.handle(panel.id)?.element();
    if (!element) return false;
    element.focus({ preventScroll: true });
    return true;
  }

  /* --------------------------------------------------------------- zoom */

  /**
   * Give one tile the whole workspace, or hand the grid back.
   *
   * The column count is deliberately kept while zoomed: restoring is one
   * keystroke away, and re-laying everything out twice is more disruptive than
   * the tiles that are temporarily hidden.
   */
  toggleZoom(id?: string): boolean {
    const target = id ?? this.#focusedId;
    if (!target || !this.find(target)) return false;
    this.#zoomedId = this.#zoomedId === target ? undefined : target;
    return true;
  }

  setColumns(columns: number): void {
    this.#columns = Math.min(Math.max(Math.trunc(columns), MIN_COLUMNS), MAX_COLUMNS);
  }

  /* ------------------------------------------------------------ subject */

  /**
   * What `$` stands for right now: the row under the cursor in the focused
   * panel, or failing that whatever the panel itself is about.
   */
  subject(): string | undefined {
    const handle = this.focusedHandle;
    return handle?.cursorSubject() ?? handle?.subject();
  }

  refreshAll(): void {
    for (const handle of this.#handles.values()) handle.refresh();
  }

  refreshFocused(): boolean {
    const handle = this.focusedHandle;
    if (!handle) return false;
    handle.refresh();
    return true;
  }
}

export const MAX_OPEN_PANELS = MAX_PANELS;
