/**
 * Session state that outlives a reload: watchlist, theme, layout, key bindings,
 * and the command history.
 *
 * Everything is stored under one localStorage key and validated on read —
 * a corrupted or hand-edited blob degrades to defaults instead of throwing on
 * boot, because a terminal that will not start is worse than one that forgot
 * your watchlist.
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

function readStorage(): PersistedState {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULTS };

    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== 'object' || parsed === null) return { ...DEFAULTS };
    const candidate = parsed as Partial<PersistedState>;

    return {
      watchlist: Array.isArray(candidate.watchlist)
        ? candidate.watchlist.filter((t): t is string => typeof t === 'string').slice(0, MAX_WATCHLIST)
        : [],
      history: Array.isArray(candidate.history)
        ? candidate.history.filter((h): h is string => typeof h === 'string').slice(-MAX_HISTORY)
        : [],
      theme: THEMES.includes(candidate.theme as ThemeName) ? (candidate.theme as ThemeName) : 'amber',
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
  #state: PersistedState;
  #listeners = new Set<() => void>();

  constructor() {
    this.#state = readStorage();
  }

  subscribe(listener: () => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  #commit(): void {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(this.#state));
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
    const { [key]: _removed, ...rest } = this.#state.keys;
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
    document.documentElement.dataset['theme'] = theme;
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
    document.documentElement.dataset['theme'] = this.#state.theme;
  }
}
