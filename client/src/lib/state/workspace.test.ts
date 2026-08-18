/**
 * Workspace tests.
 *
 * The workspace is the only thing in the client that survives a reload, so what
 * matters here is the blob: the key it is written under, the shape it takes,
 * and what happens when it comes back wrong. A reader upgrading to this version
 * of the terminal has a blob written by the last one sitting in their browser,
 * and it has to load — the fixture at the bottom of this file is exactly that
 * blob, and it is not allowed to lose a field.
 *
 * Validation is on read rather than on write because storage is a text box
 * anyone can edit and every browser will eventually truncate: a workspace that
 * refuses to boot is worse than one that forgot your watchlist.
 */

import { beforeEach, describe, expect, it } from 'vitest';
import { THEMES, Workspace, type WorkspaceStorage } from './workspace.svelte';

const STORAGE_KEY = 'prediction-terminal:v1';

interface FakeStorage extends WorkspaceStorage {
  /** What is actually in the box, for a test that wants to look. */
  readonly entries: Map<string, string>;
  /** Fail every write, the way private browsing does once it fills up. */
  breakWrites(): void;
}

function fakeStorage(seed?: unknown): FakeStorage {
  const entries = new Map<string, string>();
  if (seed !== undefined) entries.set(STORAGE_KEY, JSON.stringify(seed));
  let broken = false;

  return {
    entries,
    breakWrites() {
      broken = true;
    },
    getItem: (key) => entries.get(key) ?? null,
    setItem: (key, value) => {
      if (broken) throw new Error('QuotaExceededError');
      entries.set(key, value);
    },
  };
}

/** The blob as it sits in storage, parsed. */
function persisted(storage: FakeStorage): Record<string, unknown> {
  const raw = storage.entries.get(STORAGE_KEY);
  expect(raw, 'nothing was persisted').toBeTypeOf('string');
  return JSON.parse(raw!) as Record<string, unknown>;
}

/** Storage with a bad value in one field and sane values in the rest. */
function withField(field: string, value: unknown): Workspace {
  return new Workspace(
    fakeStorage({
      watchlist: ['KXFED-1'],
      history: ['TOP'],
      theme: 'green',
      columns: 3,
      keys: { 'panel:j': 'ROW END' },
      [field]: value,
    }),
  );
}

describe('Workspace defaults', () => {
  it('starts empty when storage has nothing', () => {
    const workspace = new Workspace(fakeStorage());
    expect(workspace.watchlist).toEqual([]);
    expect(workspace.history).toEqual([]);
    expect(workspace.theme).toBe('amber');
    expect(workspace.columns).toBe(2);
    expect(workspace.keybindings).toEqual({});
  });

  it('runs without a real storage at all', () => {
    // The module is imported in Node by these tests and on the server by
    // SvelteKit; neither has `localStorage`, and neither may throw for it.
    const workspace = new Workspace();
    expect(workspace.addToWatchlist('kxfed-1')).toBe(true);
    expect(workspace.watchlist).toEqual(['KXFED-1']);
    expect(new Workspace().watchlist).toEqual([]);
  });

  it('offers the three themes', () => {
    expect(THEMES).toEqual(['amber', 'green', 'ice']);
  });
});

describe('Workspace persistence', () => {
  it('round-trips every field through storage', () => {
    const storage = fakeStorage();
    const first = new Workspace(storage);

    first.addToWatchlist('KXFED-1');
    first.addToWatchlist('pm:will-it');
    first.pushHistory('TOP volume');
    first.setTheme('ice');
    first.setColumns(4);
    first.setBinding('panel:j', 'ROW END');

    const reloaded = new Workspace(storage);
    expect(reloaded.watchlist).toEqual(['KXFED-1', 'PM:WILL-IT']);
    expect(reloaded.history).toEqual(['TOP volume']);
    expect(reloaded.theme).toBe('ice');
    expect(reloaded.columns).toBe(4);
    expect(reloaded.keybindings).toEqual({ 'panel:j': 'ROW END' });
  });

  it('writes one key, in the shape the last version wrote', () => {
    const storage = fakeStorage();
    const workspace = new Workspace(storage);
    workspace.addToWatchlist('KXFED-1');

    expect([...storage.entries.keys()]).toEqual([STORAGE_KEY]);
    expect(persisted(storage)).toEqual({
      watchlist: ['KXFED-1'],
      history: [],
      theme: 'amber',
      columns: 2,
      keys: {},
    });
  });

  it('keeps running when storage refuses to be written', () => {
    // Private browsing, or a full quota. The session must not fall over.
    const storage = fakeStorage();
    const workspace = new Workspace(storage);
    storage.breakWrites();

    expect(() => workspace.addToWatchlist('KXFED-1')).not.toThrow();
    expect(workspace.watchlist).toEqual(['KXFED-1']);
  });

  it('notifies subscribers on every commit, until they unsubscribe', () => {
    const workspace = new Workspace(fakeStorage());
    let calls = 0;
    const off = workspace.subscribe(() => {
      calls += 1;
    });

    workspace.addToWatchlist('KXFED-1');
    workspace.pushHistory('TOP');
    expect(calls).toBe(2);

    off();
    workspace.setColumns(3);
    expect(calls).toBe(2);
  });
});

describe('Workspace watchlist', () => {
  it('upper-cases, refuses duplicates, and removes', () => {
    const workspace = new Workspace(fakeStorage());

    expect(workspace.addToWatchlist('kxfed-1')).toBe(true);
    expect(workspace.addToWatchlist('KXFED-1')).toBe(false);
    expect(workspace.watchlist).toEqual(['KXFED-1']);

    expect(workspace.removeFromWatchlist('kxfed-1')).toBe(true);
    expect(workspace.removeFromWatchlist('KXFED-1')).toBe(false);
    expect(workspace.watchlist).toEqual([]);
  });

  it('stops at forty tickers', () => {
    const workspace = new Workspace(fakeStorage());
    for (let n = 0; n < 40; n++) expect(workspace.addToWatchlist(`T${n}`)).toBe(true);

    expect(workspace.addToWatchlist('ONE-TOO-MANY')).toBe(false);
    expect(workspace.watchlist).toHaveLength(40);
  });

  it('trims an over-long stored watchlist to the cap', () => {
    const stored = Array.from({ length: 60 }, (_, n) => `T${n}`);
    const workspace = new Workspace(fakeStorage({ watchlist: stored }));

    expect(workspace.watchlist).toHaveLength(40);
    expect(workspace.watchlist[0]).toBe('T0');
    expect(workspace.watchlist.at(-1)).toBe('T39');
  });

  it('clears and says how many it forgot', () => {
    const workspace = new Workspace(fakeStorage());
    workspace.addToWatchlist('A');
    workspace.addToWatchlist('B');

    expect(workspace.clearWatchlist()).toBe(2);
    expect(workspace.watchlist).toEqual([]);
    expect(workspace.clearWatchlist()).toBe(0);
  });
});

describe('Workspace history', () => {
  it('collapses an immediate repeat', () => {
    const workspace = new Workspace(fakeStorage());
    workspace.pushHistory('TOP');
    workspace.pushHistory('TOP');
    workspace.pushHistory('  TOP  ');
    expect(workspace.history).toEqual(['TOP']);

    // Only the immediate repeat: the same command later is a new entry.
    workspace.pushHistory('W');
    workspace.pushHistory('TOP');
    expect(workspace.history).toEqual(['TOP', 'W', 'TOP']);
  });

  it('ignores an empty line', () => {
    const workspace = new Workspace(fakeStorage());
    workspace.pushHistory('');
    workspace.pushHistory('   ');
    expect(workspace.history).toEqual([]);
  });

  it('keeps the last two hundred lines', () => {
    const workspace = new Workspace(fakeStorage());
    for (let n = 0; n < 250; n++) workspace.pushHistory(`CMD ${n}`);

    expect(workspace.history).toHaveLength(200);
    expect(workspace.history[0]).toBe('CMD 50');
    expect(workspace.history.at(-1)).toBe('CMD 249');
  });

  it('trims an over-long stored history from the front', () => {
    const stored = Array.from({ length: 250 }, (_, n) => `CMD ${n}`);
    const workspace = new Workspace(fakeStorage({ history: stored }));

    expect(workspace.history).toHaveLength(200);
    expect(workspace.history[0]).toBe('CMD 50');
  });
});

describe('Workspace key bindings', () => {
  it('sets, deletes and resets', () => {
    const workspace = new Workspace(fakeStorage());

    workspace.setBinding('global:alt+w', 'TOP gainers');
    // An empty command is a real value: it switches a preset off.
    workspace.setBinding('panel:j', '');
    expect(workspace.keybindings).toEqual({ 'global:alt+w': 'TOP gainers', 'panel:j': '' });

    expect(workspace.deleteBinding('global:alt+w')).toBe(true);
    expect(workspace.deleteBinding('global:alt+w')).toBe(false);

    expect(workspace.resetBindings()).toBe(1);
    expect(workspace.keybindings).toEqual({});
    expect(workspace.resetBindings()).toBe(0);
  });

  it('stops at two hundred bindings but still edits the ones it has', () => {
    const workspace = new Workspace(fakeStorage());
    for (let n = 0; n < 200; n++) workspace.setBinding(`panel:f${(n % 24) + 1} ${n}`, 'W');
    expect(Object.keys(workspace.keybindings)).toHaveLength(200);

    workspace.setBinding('panel:z', 'W');
    expect(Object.keys(workspace.keybindings)).toHaveLength(200);
    expect(workspace.keybindings['panel:z']).toBe(undefined);

    // A key already stored is an edit, not a new binding, so it goes through.
    workspace.setBinding('panel:f1 0', 'TOP');
    expect(workspace.keybindings['panel:f1 0']).toBe('TOP');
  });

  it('drops stored bindings past the cap', () => {
    const keys: Record<string, string> = {};
    for (let n = 0; n < 260; n++) keys[`panel:seq-${n}`] = 'W';
    const workspace = new Workspace(fakeStorage({ keys }));

    expect(Object.keys(workspace.keybindings)).toHaveLength(200);
  });

  it('rejects a stored key that is not scope:chord', () => {
    const workspace = new Workspace(
      fakeStorage({
        keys: {
          'panel:j': 'ROW END',
          'global:alt+w': 'TOP',
          'panel:g w': 'W',
          'window:j': 'W', // not a scope
          'panel:': 'W', // no chord
          'panel: j': 'W', // leading space
          'panel:j ': 'W', // trailing space
          j: 'W', // no scope at all
          [`panel:${'x'.repeat(70)}`]: 'W', // longer than 64 characters
        },
      }),
    );

    expect(workspace.keybindings).toEqual({
      'panel:j': 'ROW END',
      'global:alt+w': 'TOP',
      'panel:g w': 'W',
    });
  });

  it('rejects a stored command that is not a short string', () => {
    const workspace = new Workspace(
      fakeStorage({
        keys: {
          'panel:j': 'ROW END',
          'panel:k': 42,
          'panel:l': null,
          'panel:o': ['W'],
          'panel:p': 'W '.repeat(200),
        },
      }),
    );

    expect(workspace.keybindings).toEqual({ 'panel:j': 'ROW END' });
  });
});

describe('Workspace theme and layout', () => {
  it('takes the three themes', () => {
    const workspace = new Workspace(fakeStorage());
    for (const theme of THEMES) {
      workspace.setTheme(theme);
      expect(workspace.theme).toBe(theme);
    }
  });

  it('clamps columns to one through four', () => {
    const workspace = new Workspace(fakeStorage());

    workspace.setColumns(0);
    expect(workspace.columns).toBe(1);
    workspace.setColumns(9);
    expect(workspace.columns).toBe(4);
    workspace.setColumns(-3);
    expect(workspace.columns).toBe(1);
    workspace.setColumns(2.7);
    expect(workspace.columns).toBe(2);
    workspace.setColumns(3);
    expect(workspace.columns).toBe(3);
  });

  it('applies the theme without a document to apply it to', () => {
    // Node and SSR: nothing to paint, and nothing to throw either.
    const workspace = new Workspace(fakeStorage({ theme: 'ice' }));
    expect(() => workspace.applyTheme()).not.toThrow();
    expect(() => workspace.setTheme('green')).not.toThrow();
    expect(workspace.theme).toBe('green');
  });
});

describe('Workspace malformed storage', () => {
  it('ignores a blob that is not an object', () => {
    for (const raw of ['null', '"amber"', '[1,2,3]', '42', 'not json at all', '']) {
      const storage = fakeStorage();
      storage.entries.set(STORAGE_KEY, raw);
      const workspace = new Workspace(storage);

      expect(workspace.watchlist).toEqual([]);
      expect(workspace.theme).toBe('amber');
      expect(workspace.columns).toBe(2);
    }
  });

  it('degrades a bad watchlist rather than throwing', () => {
    expect(withField('watchlist', 'KXFED-1').watchlist).toEqual([]);
    expect(withField('watchlist', { 0: 'KXFED-1' }).watchlist).toEqual([]);
    // A good array with rubbish in it keeps the strings.
    expect(withField('watchlist', ['KXFED-1', 7, null, 'PM:X']).watchlist).toEqual([
      'KXFED-1',
      'PM:X',
    ]);
  });

  it('degrades a bad history rather than throwing', () => {
    expect(withField('history', 'TOP').history).toEqual([]);
    expect(withField('history', ['TOP', 3, undefined, 'W']).history).toEqual(['TOP', 'W']);
  });

  it('falls back to amber for a theme it does not ship', () => {
    expect(withField('theme', 'chartreuse').theme).toBe('amber');
    expect(withField('theme', 7).theme).toBe('amber');
    expect(withField('theme', null).theme).toBe('amber');
    expect(withField('theme', 'ice').theme).toBe('ice');
  });

  it('falls back to two columns for a count it cannot use', () => {
    expect(withField('columns', 0).columns).toBe(2);
    expect(withField('columns', 5).columns).toBe(2);
    expect(withField('columns', '3').columns).toBe(2);
    expect(withField('columns', null).columns).toBe(2);
    expect(withField('columns', 3.9).columns).toBe(3);
  });

  it('degrades bad bindings rather than throwing', () => {
    expect(withField('keys', 'panel:j').keybindings).toEqual({});
    expect(withField('keys', null).keybindings).toEqual({});
    expect(withField('keys', ['panel:j']).keybindings).toEqual({});
  });

  it('keeps the good fields when one is bad', () => {
    const workspace = withField('theme', 'chartreuse');
    expect(workspace.watchlist).toEqual(['KXFED-1']);
    expect(workspace.history).toEqual(['TOP']);
    expect(workspace.columns).toBe(3);
    expect(workspace.keybindings).toEqual({ 'panel:j': 'ROW END' });
    expect(workspace.theme).toBe('amber');
  });
});

describe('Workspace upgrade', () => {
  /**
   * A blob written by the pre-Svelte client, byte for byte as it appears in a
   * browser that has been used for a while. Nothing about this may need
   * changing when the client does.
   */
  const LEGACY_BLOB =
    '{"watchlist":["KXPRESPARTY-28","PM:WILL-BTC-HIT-100K","SPY","BTC-USD"],' +
    '"history":["W","TOP volume","GP KXPRESPARTY-28","OB PM:WILL-BTC-HIT-100K",' +
    '"XV SPY","THEME green","KEYS alt+b OB $"],' +
    '"theme":"green","columns":3,' +
    '"keys":{"global:alt+b":"OB $","panel:j":"","panel:g w":"W","global:ctrl+l":""}}';

  it('loads a workspace the old client wrote, intact', () => {
    const storage = fakeStorage();
    storage.entries.set(STORAGE_KEY, LEGACY_BLOB);
    const workspace = new Workspace(storage);

    expect(workspace.watchlist).toEqual([
      'KXPRESPARTY-28',
      'PM:WILL-BTC-HIT-100K',
      'SPY',
      'BTC-USD',
    ]);
    expect(workspace.history).toEqual([
      'W',
      'TOP volume',
      'GP KXPRESPARTY-28',
      'OB PM:WILL-BTC-HIT-100K',
      'XV SPY',
      'THEME green',
      'KEYS alt+b OB $',
    ]);
    expect(workspace.theme).toBe('green');
    expect(workspace.columns).toBe(3);
    // The switched-off presets (`""`) have to survive: an absent entry would
    // mean "whatever the preset says", which is the opposite of switched off.
    expect(workspace.keybindings).toEqual({
      'global:alt+b': 'OB $',
      'panel:j': '',
      'panel:g w': 'W',
      'global:ctrl+l': '',
    });
  });

  it('writes it back in the same shape it read it', () => {
    const storage = fakeStorage();
    storage.entries.set(STORAGE_KEY, LEGACY_BLOB);
    const workspace = new Workspace(storage);

    workspace.pushHistory('W');

    const blob = persisted(storage);
    expect(Object.keys(blob).sort()).toEqual(['columns', 'history', 'keys', 'theme', 'watchlist']);
    expect(blob['watchlist']).toEqual(JSON.parse(LEGACY_BLOB)['watchlist']);
    expect(blob['keys']).toEqual(JSON.parse(LEGACY_BLOB)['keys']);
    expect(blob['theme']).toBe('green');
    expect(blob['columns']).toBe(3);
  });
});

describe('Workspace instances', () => {
  let storage: FakeStorage;

  beforeEach(() => {
    storage = fakeStorage();
  });

  it('does not share state between two workspaces on separate storage', () => {
    const a = new Workspace(storage);
    const b = new Workspace(fakeStorage());

    a.addToWatchlist('KXFED-1');
    expect(b.watchlist).toEqual([]);
  });

  it('reads what the other wrote only on construction', () => {
    const a = new Workspace(storage);
    a.addToWatchlist('KXFED-1');

    const b = new Workspace(storage);
    a.addToWatchlist('SPY');

    // `b` is a snapshot of the moment it was built — the terminal has one
    // workspace per tab, and this is what "per reload" means.
    expect(b.watchlist).toEqual(['KXFED-1']);
  });
});
