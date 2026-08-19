/**
 * Session state that outlives a reload: watchlist, theme, layout, key bindings,
 * and the command history.
 *
 * Everything is stored under one localStorage key and validated on read —
 * a corrupted or hand-edited blob degrades to defaults instead of throwing on
 * boot, because a terminal that will not start is worse than one that forgot
 * your watchlist.
 *
 * The key and the shape of the blob are the ones the pre-Svelte client wrote:
 * an upgrade must not cost a reader their watchlist. What changed is the inside
 * — the state is a rune, so a component that reads `workspace.watchlist` sees
 * the next one without subscribing to anything. `subscribe` stays for the plain
 * `.ts` modules (the keymap caches off it) that runes do not reach.
 */

const STORAGE_KEY = 'prediction-terminal:v1';
const MAX_HISTORY = 200;
const MAX_WATCHLIST = 40;
const MAX_KEYBINDINGS = 200;
const MAX_COMMAND_LENGTH = 200;

export type ThemeName = 'amber' | 'green' | 'ice';

export const THEMES: ThemeName[] = ['amber', 'green', 'ice'];

interface PersistedState {
  watchlist: string[];
  history: string[];
  theme: ThemeName;
  columns: number;
  /**
   * Key-binding edits, as `scope:chord` → command line.
   *
   * A sparse overlay on the presets rather than a copy of them: an empty
   * command switches a preset off, and everything not mentioned here is
   * whatever the terminal currently ships. That is what lets a preset added in
   * a later version reach someone who has already customised three keys.
   */
  keys: Record<string, string>;
}

const DEFAULTS: PersistedState = {
  watchlist: [],
  history: [],
  theme: 'amber',
  columns: 2,
  keys: {},
};

/** `scope:chord`, where a chord may be a space-separated sequence. */
const BINDING_KEY = /^(global|panel):\S(?:.*\S)?$/;

/**
 * The two methods this module wants from `localStorage`.
 *
 * Narrow on purpose: it is what lets a test hand in a fake, and what lets the
 * module load at all where there is no `Storage` — server-side rendering, or a
 * Node test runner.
 */
export interface WorkspaceStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** A scratch store for when there is no real one. Lives and dies with the tab. */
function memoryStorage(): WorkspaceStorage {
  const entries = new Map<string, string>();
  return {
    getItem: (key) => entries.get(key) ?? null,
    setItem: (key, value) => {
      entries.set(key, value);
    },
  };
}

/**
 * The browser's storage where there is one, a private scratch store where there
 * is not — importing this module must not throw in Node, and a workspace with
 * nowhere to persist is still a usable workspace.
 */
function defaultStorage(): WorkspaceStorage {
  return typeof localStorage === 'undefined' ? memoryStorage() : localStorage;
}

/** Write the theme where the stylesheet can see it, if there is a document. */
function paintTheme(theme: ThemeName): void {
  if (typeof document === 'undefined') return;
  document.documentElement.dataset['theme'] = theme;
}

function readBindings(value: unknown): Record<string, string> {
  if (typeof value !== 'object' || value === null) return {};

  const bindings: Record<string, string> = {};
  for (const [key, command] of Object.entries(value as Record<string, unknown>)) {
    if (typeof command !== 'string' || command.length > MAX_COMMAND_LENGTH) continue;
    if (!BINDING_KEY.test(key) || key.length > 64) continue;
    bindings[key] = command;
    if (Object.keys(bindings).length >= MAX_KEYBINDINGS) break;
  }
  return bindings;
}

function readStorage(storage: WorkspaceStorage): PersistedState {
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULTS };

    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== 'object' || parsed === null) return { ...DEFAULTS };
    const candidate = parsed as Partial<PersistedState>;

    return {
      watchlist: Array.isArray(candidate.watchlist)
        ? candidate.watchlist
            .filter((t): t is string => typeof t === 'string')
            .slice(0, MAX_WATCHLIST)
        : [],
      history: Array.isArray(candidate.history)
        ? candidate.history.filter((h): h is string => typeof h === 'string').slice(-MAX_HISTORY)
        : [],
      theme: THEMES.includes(candidate.theme as ThemeName)
        ? (candidate.theme as ThemeName)
        : 'amber',
      columns:
        typeof candidate.columns === 'number' && candidate.columns >= 1 && candidate.columns <= 4
          ? Math.trunc(candidate.columns)
          : 2,
      keys: readBindings(candidate.keys),
    };
  } catch {
    // Private browsing, quota, or a bad blob. Defaults are always usable.
    return { ...DEFAULTS };
  }
}

export class Workspace {
  readonly #storage: WorkspaceStorage;
  #state: PersistedState = $state({ ...DEFAULTS });
  #listeners = new Set<() => void>();

  constructor(storage: WorkspaceStorage = defaultStorage()) {
    this.#storage = storage;
    this.#state = readStorage(storage);
  }

  subscribe(listener: () => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  #commit(): void {
    try {
      this.#storage.setItem(STORAGE_KEY, JSON.stringify($state.snapshot(this.#state)));
    } catch {
      // Storage being unavailable must not break the running session.
    }
    for (const listener of this.#listeners) listener();
  }

  /* ------------------------------------------------------------ watchlist */

  get watchlist(): readonly string[] {
    return this.#state.watchlist;
  }

  addToWatchlist(ticker: string): boolean {
    const upper = ticker.toUpperCase();
    if (this.#state.watchlist.includes(upper)) return false;
    if (this.#state.watchlist.length >= MAX_WATCHLIST) return false;
    this.#state.watchlist = [...this.#state.watchlist, upper];
    this.#commit();
    return true;
  }

  removeFromWatchlist(ticker: string): boolean {
    const upper = ticker.toUpperCase();
    if (!this.#state.watchlist.includes(upper)) return false;
    this.#state.watchlist = this.#state.watchlist.filter((t) => t !== upper);
    this.#commit();
    return true;
  }

  clearWatchlist(): number {
    const count = this.#state.watchlist.length;
    this.#state.watchlist = [];
    this.#commit();
    return count;
  }

  /* -------------------------------------------------------------- history */

  get history(): readonly string[] {
    return this.#state.history;
  }

  pushHistory(command: string): void {
    const trimmed = command.trim();
    if (!trimmed) return;
    // Collapse an immediate repeat — mashing Enter should not fill the buffer.
    if (this.#state.history.at(-1) === trimmed) return;
    this.#state.history = [...this.#state.history, trimmed].slice(-MAX_HISTORY);
    this.#commit();
  }

  /* --------------------------------------------------------- keybindings */

  get keybindings(): Readonly<Record<string, string>> {
    return this.#state.keys;
  }

  setBinding(key: string, command: string): void {
    if (Object.keys(this.#state.keys).length >= MAX_KEYBINDINGS && !(key in this.#state.keys)) {
      return;
    }
    this.#state.keys = { ...this.#state.keys, [key]: command };
    this.#commit();
  }

  deleteBinding(key: string): boolean {
    if (!(key in this.#state.keys)) return false;
    // Copy and drop rather than delete in place: the field is reassigned as a
    // whole, which is what a rune watches.
    const rest = { ...this.#state.keys };
    delete rest[key];
    this.#state.keys = rest;
    this.#commit();
    return true;
  }

  resetBindings(): number {
    const count = Object.keys(this.#state.keys).length;
    this.#state.keys = {};
    this.#commit();
    return count;
  }

  /* ------------------------------------------------------- theme & layout */

  get theme(): ThemeName {
    return this.#state.theme;
  }

  setTheme(theme: ThemeName): void {
    this.#state.theme = theme;
    paintTheme(theme);
    this.#commit();
  }

  get columns(): number {
    return this.#state.columns;
  }

  setColumns(columns: number): void {
    this.#state.columns = Math.min(Math.max(Math.trunc(columns), 1), 4);
    this.#commit();
  }

  applyTheme(): void {
    paintTheme(this.#state.theme);
  }
}
