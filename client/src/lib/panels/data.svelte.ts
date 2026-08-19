/**
 * A panel's data: load, poll, abort, and what to show when it fails.
 *
 * This is the lifecycle the old `Panel` base class owned. Two rules in it are
 * behaviour, not plumbing, and are the reason it is shared rather than written
 * per panel:
 *
 * A newer request always wins. Every load aborts the one before it, so a slow
 * reply cannot land on top of a fast one and show a price that has already been
 * superseded.
 *
 * A panel that has data keeps it. When a poll fails on a panel that has already
 * loaded, the last good render stays on screen and only the status line goes
 * red — a transient upstream blip should not blank a chart someone is reading.
 * A panel that has *never* loaded has nothing to keep, so it shows the failure
 * in full, including the server's remediation hint.
 */

import { ApiRequestError } from '../api/client';

export interface PanelDataOptions<T> {
  /** Fetch this panel's data. Must honour `signal`. */
  load: (signal: AbortSignal) => Promise<T>;
  /**
   * Poll interval in ms; `0` disables polling. Read on every re-arm, so a panel
   * that re-cuts its own cadence — a chart moving between minute and daily bars
   * — can simply return a different number.
   */
  refreshMs?: number | (() => number);
}

export interface PanelError {
  message: string;
  /** The server's operator-facing hint, shown verbatim. */
  hint?: string;
}

export interface PanelData<T> {
  /** The most recent successful load. */
  readonly data: T | undefined;
  /** Set only when the panel has never loaded — otherwise `stale` is used. */
  readonly error: PanelError | undefined;
  /** A failure on a panel that still has good data on screen. */
  readonly stale: string | undefined;
  readonly loading: boolean;
  /** `HH:MM:SSZ` of the last successful load. */
  readonly stamp: string | undefined;
  refresh(): void;
  destroy(): void;
}

export function createPanelData<T>(options: PanelDataOptions<T>): PanelData<T> {
  let data = $state<T | undefined>(undefined);
  let error = $state<PanelError | undefined>(undefined);
  let stale = $state<string | undefined>(undefined);
  let loading = $state(false);
  let stamp = $state<string | undefined>(undefined);

  let controller: AbortController | undefined;
  let timer: ReturnType<typeof setInterval> | undefined;
  let armedFor = -1;
  let destroyed = false;
  let hasLoaded = false;

  const interval = () =>
    typeof options.refreshMs === 'function' ? options.refreshMs() : (options.refreshMs ?? 0);

  function arm(): void {
    const ms = interval();
    if (timer !== undefined && armedFor === ms) return;

    if (timer !== undefined) clearInterval(timer);
    timer = undefined;
    armedFor = ms;
    if (destroyed || ms <= 0) return;
    timer = setInterval(() => void refresh(), ms);
  }

  async function refresh(): Promise<void> {
    if (destroyed) return;

    // Cancel any in-flight load so a slow reply cannot land after a fast one.
    controller?.abort();
    const own = new AbortController();
    controller = own;
    loading = true;

    try {
      const loaded = await options.load(own.signal);
      if (destroyed || own.signal.aborted) return;

      data = loaded;
      error = undefined;
      stale = undefined;
      hasLoaded = true;
      stamp = utcStamp();
    } catch (err) {
      if (destroyed || own.signal.aborted || isAbort(err)) return;

      const message = err instanceof Error ? err.message : String(err);
      if (hasLoaded) {
        // Keep the last good render; say only that it is no longer fresh.
        stale = message;
      } else {
        error = {
          message,
          hint: err instanceof ApiRequestError ? err.hint : undefined,
        };
      }
    } finally {
      if (controller === own) {
        controller = undefined;
        loading = false;
      }
      // Re-read the interval every time, so a panel that changed cadence
      // mid-flight is re-armed rather than left on its old timer.
      if (!destroyed) arm();
    }
  }

  void refresh();
  arm();

  return {
    get data() {
      return data;
    },
    get error() {
      return error;
    },
    get stale() {
      return stale;
    },
    get loading() {
      return loading;
    },
    get stamp() {
      return stamp;
    },
    refresh() {
      void refresh();
    },
    destroy() {
      destroyed = true;
      if (timer !== undefined) clearInterval(timer);
      timer = undefined;
      controller?.abort();
      controller = undefined;
    },
  };
}

/**
 * An aborted fetch is not a failure — it means a newer request replaced this
 * one, and the newer one owns the panel now.
 */
function isAbort(err: unknown): boolean {
  return err instanceof DOMException && err.name === 'AbortError';
}

function utcStamp(): string {
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${pad(now.getUTCHours())}:${pad(now.getUTCMinutes())}:${pad(now.getUTCSeconds())}Z`;
}
