/**
 * Workspace tests: what a saved grid is, and what a link is allowed to do.
 *
 * The second half is the one that matters. A `#ws=` hash is written by somebody
 * else and opened with a click, so these tests are mostly the hashes a hostile
 * or merely broken link would carry — too long, too many, half-encoded, or
 * naming a command that edits something — run against a real command table, a
 * real panel grid and a real workspace, with the assertion that storage did not
 * move.
 */

import { beforeEach, describe, expect, it } from 'vitest';

import type { LogLevel } from '../context';
import { PanelStore } from '../state/panels.svelte';
import { Workspace, type WorkspaceStorage } from '../state/workspace.svelte';
import { UsageError, type CommandContext } from './command';
import { COMMAND_INDEX, COMMANDS } from './commands';
import { Keymap } from './keys';
import { parse } from './parser';
import {
  MAX_LINK_LENGTH,
  PANEL_VERBS,
  decodeLink,
  encodeLink,
  openLink,
  panelCommand,
  reopen,
  skippedNote,
  snapshot,
  workspaceCommand,
  type LinkEnvironment,
} from './workspaces';

const STORAGE_KEY = 'prediction-terminal:v1';

interface Terminal {
  context: CommandContext;
  panels: PanelStore;
  workspace: Workspace;
  /** Everything logged, as `level: message`. */
  lines: string[];
  /** The stored blob, or `undefined` when nothing has been written. */
  stored(): string | undefined;
  /** Type a line: the page's `run`, without the page. */
  run(line: string): void;
  /** The commands the open panels remember, in grid order. */
  commands(): (string | undefined)[];
}

/**
 * The terminal's services, wired the way `+page.svelte` wires them.
 *
 * `replay` is the page's own, restated: parse, look the verb up, call the
 * handler inside `panels.opening`. It is short enough that restating it is
 * cheaper than mounting a component to borrow it.
 */
function terminal(seed?: unknown): Terminal {
  const entries = new Map<string, string>();
  if (seed !== undefined) entries.set(STORAGE_KEY, JSON.stringify(seed));
  const storage: WorkspaceStorage = {
    getItem: (key) => entries.get(key) ?? null,
    setItem: (key, value) => {
      entries.set(key, value);
    },
  };

  const workspace = new Workspace(storage);
  const panels = new PanelStore();
  const lines: string[] = [];
  const log = (message: string, level: LogLevel = 'info'): void => {
    lines.push(`${level}: ${message}`);
  };

  const replay = (line: string): boolean => {
    const parsed = parse(line);
    const command = COMMAND_INDEX.get(parsed.verb);
    if (!command) {
      log(`Unknown command "${parsed.verb}".`, 'error');
      return false;
    }
    try {
      void panels.opening(line, () => command.handler(parsed, context));
      return true;
    } catch (err) {
      if (!(err instanceof UsageError)) throw err;
      log(err.message, 'error');
      return false;
    }
  };

  const context: CommandContext = {
    run: (line) => {
      workspace.pushHistory(line);
      replay(line);
    },
    replay,
    log,
    panels,
    workspace,
    keys: new Keymap(workspace),
    clearLog: () => {
      lines.length = 0;
    },
    setMode: () => {},
    menu: {
      isOpen: false,
      titles: () => [],
      open: () => false,
      toggle: () => {},
      close: () => {},
      cycle: () => {},
    },
  };

  return {
    context,
    panels,
    workspace,
    lines,
    stored: () => entries.get(STORAGE_KEY),
    run: context.run,
    commands: () => panels.panels.map((panel) => panel.command),
  };
}

/** `WS …`, with a clipboard that works unless told otherwise. */
async function ws(
  t: Terminal,
  line: string,
  environment: Partial<LinkEnvironment> = {},
): Promise<string[]> {
  const copied: string[] = [];
  await workspaceCommand(parse(line), t.context, COMMAND_INDEX, {
    base: () => 'https://terminal.test/',
    copy: (text) => {
      copied.push(text);
      return Promise.resolve();
    },
    ...environment,
  });
  return copied;
}

describe('the command a panel remembers', () => {
  it('is the line that opened it', () => {
    const t = terminal();
    t.run('GP KXFED-1 1d');
    t.run('TOP volume');

    expect(t.commands()).toEqual(['GP KXFED-1 1d', 'TOP volume']);
  });

  it('follows the panel when a second line re-points it', () => {
    const t = terminal();
    t.run('GP KXFED-1 1d');
    t.run('GP KXFED-1 1h line');

    expect(t.panels.count).toBe(1);
    expect(t.commands()).toEqual(['GP KXFED-1 1h line']);
  });

  it('is absent for a panel nothing dispatched, and that is not an error', () => {
    const panels = new PanelStore();
    panels.open({ id: 'help', kind: 'help', props: {} });

    expect(panels.panels[0]?.command).toBeUndefined();
    expect(snapshot(panels.panels, COMMAND_INDEX)).toEqual({ lines: [], skipped: ['help'] });
  });

  it('is the innermost line when one command runs another', () => {
    const panels = new PanelStore();
    panels.opening('WS FED', () => {
      panels.opening('GP KXFED-1', () => {
        panels.open({ id: 'gp', kind: 'chart', props: {} });
      });
      panels.open({ id: 'outer', kind: 'help', props: {} });
    });
    panels.open({ id: 'after', kind: 'help', props: {} });

    expect(panels.panels.map((panel) => panel.command)).toEqual([
      'GP KXFED-1',
      'WS FED',
      undefined,
    ]);
  });
});

describe('panel commands', () => {
  it('lets through a line that only opens a panel, aliases included', () => {
    for (const line of [
      'GP KXFED-1 1d',
      'chart KXFED-1',
      'W',
      'KEYS',
      'HELP GP',
      'SRCH "fed cut"',
    ]) {
      expect(panelCommand(line, COMMAND_INDEX), line).toBe(line);
    }
  });

  it('refuses every line that would change something', () => {
    const refused = [
      'W ADD KXFED-1',
      'W DEL KXFED-1',
      'W CLEAR',
      'WATCH ADD KXFED-1',
      'KEYS alt+b CLS ALL',
      'KEYS RESET',
      'KEYS DEL alt+b',
      'KEYS --global',
      'BIND j CLS',
      'WS SAVE X',
      'WS DEL X',
      'WS X',
      'WS LINK',
      'THEME ice',
      'LAY 4',
      'CLS ALL',
      'CLR',
      'REFRESH',
      'ZOOM',
      'FOCUS NAV',
      'ROW OPEN',
      'MENU',
    ];
    for (const line of refused) expect(panelCommand(line, COMMAND_INDEX), line).toBeUndefined();
  });

  it('refuses what is not a command at all', () => {
    const refused = [
      '',
      '   ',
      'NOPE X',
      '>GP',
      '$',
      'javascript:alert(1)',
      '<img src=x onerror=alert(1)>',
      'GP KXFED-1\nW ADD X',
      `GP ${'X'.repeat(200)}`,
    ];
    for (const line of refused) expect(panelCommand(line, COMMAND_INDEX), line).toBeUndefined();
  });

  it('lists only verbs the table has', () => {
    const verbs = new Set(COMMANDS.map((command) => command.verb));
    expect([...PANEL_VERBS].filter((verb) => !verbs.has(verb))).toEqual([]);
  });

  it('lists only verbs whose handlers touch nothing but the grid and the log', () => {
    // Every example of every listed verb that passes the check is run against a
    // context where using anything except the panel grid and the log throws.
    // A verb that edited the watchlist, bound a key or ran another command
    // would fail here rather than in somebody's browser. Naming a service is
    // allowed — `W` destructures the workspace it only uses in `W ADD` — so it
    // is the service that is booby-trapped, not the mention of it.
    const panels = new PanelStore();
    const trap = (name: string): unknown =>
      new Proxy(() => {}, {
        get(_target, property) {
          throw new Error(`a panel command read context.${name}.${String(property)}`);
        },
        apply() {
          throw new Error(`a panel command called context.${name}()`);
        },
      });
    const sealed = new Proxy({ panels, log: () => {} } as unknown as CommandContext, {
      get(target, property) {
        if (property === 'panels') return target.panels;
        if (property === 'log') return target.log;
        return trap(String(property));
      },
    });

    let ran = 0;
    for (const command of COMMANDS.filter((entry) => PANEL_VERBS.has(entry.verb))) {
      const lines = [command.verb, ...(command.examples ?? [])]
        .map((line) => panelCommand(line, COMMAND_INDEX))
        .filter((line): line is string => line !== undefined);

      for (const line of lines) {
        try {
          void command.handler(parse(line), sealed);
          ran += 1;
        } catch (err) {
          // A bare verb that needs an argument says so, and that is fine.
          if (!(err instanceof UsageError)) throw new Error(`${line}: ${String(err)}`);
        }
      }
    }

    expect(ran).toBeGreaterThan(PANEL_VERBS.size);
    expect(panels.count).toBeGreaterThan(0);
  });
});

describe('snapshot', () => {
  it('saves the bare verb for a panel an editing command left open', () => {
    const t = terminal();
    t.run('W ADD KXFED-1');
    t.run('KEYS alt+b OB $');

    expect(t.commands()).toEqual(['W ADD KXFED-1', 'KEYS alt+b OB $']);
    expect(snapshot(t.panels.panels, COMMAND_INDEX)).toEqual({ lines: ['W', 'KEYS'], skipped: [] });
  });

  it('names the panels no listed command reopens', () => {
    const panels = new PanelStore();
    panels.opening('GP KXFED-1', () => panels.open({ id: 'a', kind: 'chart', props: {} }));
    panels.opening('THEME ice', () => panels.open({ id: 'b', kind: 'help', props: {} }));

    expect(snapshot(panels.panels, COMMAND_INDEX)).toEqual({
      lines: ['GP KXFED-1'],
      skipped: ['THEME ice'],
    });
  });
});

describe('a workspace link', () => {
  const GRID = { columns: 3, panels: ['GP KXFED-1 1d', 'SRCH "fed cut"', 'OB pm:will-it', 'W'] };

  it('is readable, and reads back to the grid it was made from', () => {
    const hash = encodeLink(GRID);

    expect(hash).toBe('#ws=3;GP+KXFED-1+1d;SRCH+%22fed+cut%22;OB+pm%3Awill-it;W');
    expect(decodeLink(hash)).toEqual({ kind: 'grid', grid: GRID });
  });

  it('survives the characters that would break a naive separator', () => {
    const grid = { columns: 1, panels: ['SRCH a;b+c', 'ECO ecb:EXR/D.USD.EUR.SP00.A', 'SRCH 50%'] };
    expect(decodeLink(encodeLink(grid))).toEqual({ kind: 'grid', grid });
  });

  it('leaves any other hash alone', () => {
    for (const hash of ['', '#', '#top', '#workspace=1;W', 'ws', '#WS=2;W']) {
      expect(decodeLink(hash), hash).toEqual({ kind: 'none' });
    }
  });

  it('refuses a malformed hash whole', () => {
    const malformed = [
      '#ws=',
      '#ws=2',
      '#ws=2;',
      '#ws=;W',
      '#ws=0;W',
      '#ws=5;W',
      '#ws=22;W',
      '#ws=two;W',
      '#ws=2;W;;TOP',
      '#ws=2;%E0%A4%A',
      '#ws=2;GP%',
      '#ws=2;+++',
      '#ws={"panels":[{"id":"x","kind":"chart","props":{}}]}',
      '#ws=W;TOP',
    ];
    for (const hash of malformed) expect(decodeLink(hash).kind, hash).toBe('error');
  });

  it('refuses an oversized hash before reading any of it', () => {
    const long = `#ws=2;GP+${'X'.repeat(MAX_LINK_LENGTH)}`;
    expect(decodeLink(long)).toEqual({
      kind: 'error',
      reason: `it is longer than ${MAX_LINK_LENGTH} characters`,
    });

    // Under the cap as a whole, but one command is longer than any command is.
    expect(decodeLink(`#ws=2;GP+${'X'.repeat(300)}`).kind).toBe('error');
  });

  it('refuses more panels than the grid holds', () => {
    const lines = Array.from({ length: 13 }, (_, i) => `GP X${i}`);
    expect(decodeLink(encodeLink({ columns: 2, panels: lines }))).toEqual({
      kind: 'error',
      reason: 'it names 13 panels and the grid holds 12',
    });
    expect(decodeLink(encodeLink({ columns: 2, panels: lines.slice(0, 12) })).kind).toBe('grid');
  });
});

describe('opening a link', () => {
  /** A reader with things worth losing. */
  const SEED = {
    watchlist: ['KXFED-1'],
    history: ['TOP'],
    theme: 'green',
    columns: 2,
    keys: { 'panel:j': 'ROW END' },
    workspaces: { MINE: { columns: 2, panels: ['W'] } },
    session: ['TOP volume'],
  };

  let t: Terminal;

  beforeEach(() => {
    t = terminal(SEED);
  });

  it('opens the panels it names, through the dispatch a typed line takes', () => {
    expect(openLink('#ws=3;GP+KXFED-1;W', t.context, COMMAND_INDEX)).toBe(true);

    expect(t.commands()).toEqual(['GP KXFED-1', 'W']);
    expect(t.panels.columns).toBe(3);
    expect(t.lines).toEqual([
      'info: Opened a linked workspace: 2 panels. It is not saved — WS SAVE <name> keeps it.',
    ]);
  });

  it('runs none of the commands that would change stored state, and names them', () => {
    const hash = encodeLink({
      columns: 4,
      panels: [
        'W ADD pm:planted',
        'W CLEAR',
        'KEYS j CLS ALL',
        'KEYS RESET',
        'WS SAVE MINE',
        'WS DEL MINE',
        'THEME ice',
        'LAY 1',
        'GP KXFED-1',
      ],
    });

    expect(openLink(hash, t.context, COMMAND_INDEX)).toBe(true);

    expect(t.commands()).toEqual(['GP KXFED-1']);
    // Nothing was written at all — not the watchlist, not a key, not a
    // workspace, not the history, not even the column count the link asked for.
    expect(t.stored()).toBe(JSON.stringify(SEED));
    expect(t.workspace.watchlist).toEqual(['KXFED-1']);
    expect(t.workspace.keybindings).toEqual({ 'panel:j': 'ROW END' });
    expect(t.workspace.workspaces).toEqual({ MINE: { columns: 2, panels: ['W'] } });
    expect(t.workspace.theme).toBe('green');
    expect(t.workspace.columns).toBe(2);

    expect(t.lines).toHaveLength(1);
    expect(t.lines[0]).toMatch(/^warn: Opened a linked workspace: 1 panel\. Skipped 8 lines/);
    expect(t.lines[0]).toContain('"W ADD pm:planted"');
    expect(t.lines[0]).toContain('"WS DEL MINE"');
  });

  it('names a panel command the parser would not take', () => {
    expect(openLink('#ws=2;GP;TOP+volume', t.context, COMMAND_INDEX)).toBe(true);

    expect(t.commands()).toEqual(['TOP volume']);
    expect(t.lines).toEqual([
      'error: Missing <ticker>',
      'warn: Opened a linked workspace: 1 panel. Skipped 1 line that is not a panel command ' +
        'or would not open: "GP". It is not saved — WS SAVE <name> keeps it.',
    ]);
  });

  it('opens nothing for a hash it cannot read, and leaves the grid as it was', () => {
    t.run('TOP volume');
    const before = t.stored();

    for (const hash of ['#ws=9;W', `#ws=2;${'W;'.repeat(40)}W`, '#ws=2;%ZZ']) {
      t.lines.length = 0;
      expect(openLink(hash, t.context, COMMAND_INDEX), hash).toBe(false);
      expect(t.lines).toHaveLength(1);
      expect(t.lines[0]).toMatch(/^error: That link is not a workspace: .*Nothing was opened\.$/);
    }

    expect(t.commands()).toEqual(['TOP volume']);
    expect(t.stored()).toBe(before);
  });

  it('says nothing about a hash that is not a workspace link', () => {
    expect(openLink('#top', t.context, COMMAND_INDEX)).toBe(false);
    expect(openLink('', t.context, COMMAND_INDEX)).toBe(false);
    expect(t.lines).toEqual([]);
  });

  it('never puts a control character in the log', () => {
    expect(skippedNote(['W ADD X\u0007\u001b[2J'])).not.toMatch(/\p{Cc}/u);
    expect(skippedNote([])).toBe('');
    expect(skippedNote(['X'.repeat(80)]).length).toBeLessThan(120);
  });
});

describe('reopening a stored grid', () => {
  it('applies the same list to storage that it applies to a link', () => {
    // Storage is a text box too. A hand-edited workspace gets no more trust.
    const t = terminal({
      workspaces: { EDITED: { columns: 2, panels: ['W CLEAR', 'TOP volume', 'CLS ALL'] } },
      watchlist: ['KXFED-1'],
    });

    const result = reopen(t.workspace.workspace('EDITED')!, t.context, COMMAND_INDEX);

    expect(result).toEqual({ opened: 1, skipped: ['W CLEAR', 'CLS ALL'] });
    expect(t.commands()).toEqual(['TOP volume']);
    expect(t.workspace.watchlist).toEqual(['KXFED-1']);
  });

  it('replaces the grid rather than adding to it', () => {
    const t = terminal();
    t.run('HELP');
    t.run('TOP volume');

    reopen({ columns: 1, panels: ['W'] }, t.context, COMMAND_INDEX);
    expect(t.commands()).toEqual(['W']);
    expect(t.panels.columns).toBe(1);
  });
});

describe('WS', () => {
  let t: Terminal;

  beforeEach(() => {
    t = terminal();
    t.run('GP KXFED-1 1d');
    t.run('OB pm:will-it');
    t.run('LAY 3');
    t.lines.length = 0;
  });

  it('saves the open grid, lists it, reopens it and deletes it', async () => {
    await ws(t, 'WS SAVE fed');
    expect(t.workspace.workspaces).toEqual({
      FED: { columns: 3, panels: ['GP KXFED-1 1d', 'OB pm:will-it'] },
    });

    await ws(t, 'WS');
    expect(t.lines).toEqual([
      'info: Saved workspace FED: 2 panels.',
      'info: 1 saved workspace — WS <name> reopens one:',
      'info:   FED · 2 panels · GP KXFED-1 1d · OB pm:will-it',
    ]);

    t.panels.closeAll();
    t.run('LAY 1');
    t.run('HELP');
    t.lines.length = 0;

    await ws(t, 'WS Fed');
    expect(t.commands()).toEqual(['GP KXFED-1 1d', 'OB pm:will-it']);
    expect(t.panels.columns).toBe(3);
    // The reader's own grid: its column count is remembered, as `LAY`'s is.
    expect(t.workspace.columns).toBe(3);
    expect(t.lines).toEqual(['info: Opened workspace FED: 2 panels.']);

    await ws(t, 'WS DEL fed');
    await ws(t, 'WS DEL fed');
    expect(t.workspace.workspaces).toEqual({});
    expect(t.lines.slice(1)).toEqual([
      'info: Deleted workspace FED.',
      'info: There is no workspace called FED.',
    ]);
  });

  it('says so when there is nothing saved', async () => {
    await ws(t, 'WS');
    expect(t.lines).toEqual(['info: No saved workspaces. WS SAVE <name> keeps the open grid.']);
  });

  it('says it replaced a workspace rather than pretending it is new', async () => {
    await ws(t, 'WS SAVE fed');
    t.run('TOP volume');
    await ws(t, 'WS SAVE FED');

    expect(t.lines.at(-1)).toBe('info: Replaced workspace FED: 3 panels.');
  });

  it('reads a bad name, a reserved one and a missing one as usage errors', () => {
    const run = (line: string) => () => workspaceCommand(parse(line), t.context, COMMAND_INDEX);

    expect(run('WS SAVE')).toThrow(/Missing <name>/);
    expect(run('WS SAVE fed!')).toThrow(/is not a name/);
    expect(run('WS SAVE link')).toThrow(/LINK is a WS action/);
    expect(run('WS DEL')).toThrow(/Missing <name>/);
    expect(run('WS nowhere')).toThrow(/no workspace called NOWHERE/);
    expect(run('WS constructor')).toThrow(/no workspace called CONSTRUCTOR/);
    expect(t.workspace.workspaces).toEqual({});
  });

  it('refuses to save or link an empty grid', () => {
    t.panels.closeAll();
    const run = (line: string) => () => workspaceCommand(parse(line), t.context, COMMAND_INDEX);

    expect(run('WS SAVE fed')).toThrow(/nothing to save/);
    expect(run('WS LINK')).toThrow(/nothing to save/);
  });

  it('copies a link that reads back to the grid on screen', async () => {
    const copied = await ws(t, 'WS LINK');

    expect(copied).toEqual(['https://terminal.test/#ws=3;GP+KXFED-1+1d;OB+pm%3Awill-it']);
    expect(t.lines).toEqual(['info: Copied a link to this grid: 2 panels.']);

    const visitor = terminal();
    expect(openLink(new URL(copied[0]!).hash, visitor.context, COMMAND_INDEX)).toBe(true);
    expect(visitor.commands()).toEqual(t.commands());
    expect(visitor.panels.panels.map((panel) => panel.props)).toEqual(
      t.panels.panels.map((panel) => panel.props),
    );
    expect(visitor.stored()).toBeUndefined();
  });

  it('prints the link when the clipboard refuses', async () => {
    const copied = await ws(t, 'WS LINK', {
      copy: () => Promise.reject(new DOMException('denied', 'NotAllowedError')),
    });

    expect(copied).toEqual([]);
    expect(t.lines).toEqual([
      'warn: The clipboard is not available — the link to this grid, 2 panels.',
      'info: https://terminal.test/#ws=3;GP+KXFED-1+1d;OB+pm%3Awill-it',
    ]);
  });

  it('prints the link when there is no clipboard to ask', async () => {
    await ws(t, 'WS LINK', {
      copy: () => {
        throw new TypeError('navigator.clipboard is undefined');
      },
    });

    expect(t.lines[1]).toBe('info: https://terminal.test/#ws=3;GP+KXFED-1+1d;OB+pm%3Awill-it');
  });

  it('will not make a link too long to open', () => {
    t.panels.closeAll();
    for (let i = 0; i < 12; i++) t.run(`SRCH ${'%'.repeat(150)}${i}`);

    expect(() => workspaceCommand(parse('WS LINK'), t.context, COMMAND_INDEX)).toThrow(
      /too long to put in a link/,
    );
  });

  it('is not a verb or an alias anything else claims', () => {
    // `indexCommands` throws on a collision at import, so reaching this line is
    // most of the test. The rest is that the name resolves to this command.
    for (const name of ['WS', 'WORKSPACE', 'WORKSPACES']) {
      expect(COMMAND_INDEX.get(name)?.verb).toBe('WS');
    }
  });
});
