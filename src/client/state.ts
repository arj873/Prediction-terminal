/**
 * Session state that outlives a reload: watchlist, theme, layout, and the
 * command history.
 *
 * Everything is stored under one localStorage key and validated on read —
 * a corrupted or hand-edited blob degrades to defaults instead of throwing on
 * boot, because a terminal that will not start is worse than one that forgot
 * your watchlist.
 */

const STORAGE_KEY = 'prediction-terminal:v1';
const MAX_HISTORY = 200;
const MAX_WATCHLIST = 40;

export type ThemeName = 'amber' | 'green' | 'ice';

export const THEMES: ThemeName[] = ['amber', 'green', 'ice'];

interface PersistedState {
  watchlist: string[];
  history: string[];
  theme: ThemeName;
  columns: number;
}

const DEFAULTS: PersistedState = {
  watchlist: [],
  history: [],
  theme: 'amber',
  columns: 2,
};

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
