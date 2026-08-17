/**
 * The menu bar: everything the terminal does, reachable without remembering it.
 *
 * It is a teaching surface first and a mouse convenience second, so every entry
 * shows three things at once — what it does, the command line it runs, and the
 * key that runs it without the menu. Clicking one echoes that command into the
 * message log and pushes it onto the history, so the next time it is `↑` away
 * and the time after that it is muscle memory. A menu that only did the thing
 * would leave you needing it forever.
 *
 * Nothing here is a second dispatch path: an entry hands its command line to
 * the same `run()` a typed line goes to, `$` is filled the same way a key
 * binding fills it, and the chord column is read out of the live keymap — so a
 * key rebound with `KEYS` is relabelled here without anyone being told.
 */

import { el } from '../lib/dom.js';
import type { MenuControl } from './command.js';
import { applySubject, formatChord, SUBJECT_HINT, type KeyMode, type Keymap } from './keys.js';
import {
  bindingsFor,
  chordLabel,
  commandOf,
  findMenu,
  verbOf,
  type Menu,
  type MenuEntry,
  type MenuGroup,
  type MenuItem,
} from './menu.js';

export interface MenuBarOptions {
  menus: readonly Menu[];
  keymap: Keymap;
  /** Dispatch a command line, exactly as if it had been typed. */
  run(command: string): void;
  /** Put a line in the log as though it had been typed, and into the history. */
  echo(command: string): void;
  log(message: string, level?: 'info' | 'warn' | 'error'): void;
  /** `$` — the row under the cursor, or the focused panel's own subject. */
  subject(): string | undefined;
  /** Where the keyboard was before the menu took it. */
  mode(): KeyMode;
  /** Give it back. */
  setMode(mode: KeyMode): void;
}

/** One rendered entry, and whether it can be run right now. */
interface RenderedItem {
  entry: MenuItem | MenuGroup;
  node: HTMLButtonElement;
  /** False when the command wants a `$` and there is nothing to fill it with. */
  enabled: boolean;
}

/** One open list: the dropdown itself, or a flyout hanging off a group. */
interface Level {
  list: HTMLElement;
  items: RenderedItem[];
  index: number;
}

/** How long a type-ahead prefix survives between keystrokes. */
const TYPEAHEAD_MS = 900;

/** How long the pointer has to rest on a sibling before it closes a flyout. */
const HOVER_SETTLE_MS = 220;

/**
 * A command line as the reader would type it.
 *
 * `>` is the terminal's way of saying "type this, do not run it" — useful in a
 * key binding, meaningless as a lesson. The entry that carries it is offering
 * the start of a line, so it is shown as one: `GP …`, not `>GP`.
 */
function promptForm(command: string): string {
  return command.startsWith('>') ? `${command.slice(1).trim()} …` : command;
}

export class MenuBar implements MenuControl {
  readonly root: HTMLElement;

  readonly #options: MenuBarOptions;
  readonly #titles: HTMLButtonElement[] = [];
  readonly #dropdown: HTMLElement;
  readonly #hint: HTMLElement;
  readonly #note: HTMLElement;

  #openIndex: number | undefined;
  #levels: Level[] = [];
  #returnMode: KeyMode = 'cmd';
  /** `$` as it stood when the menu opened; nothing can change it while it is up. */
  #subject: string | undefined;
  #typeahead = '';
  #typeaheadAt = 0;
  #hoverTimer: number | undefined;
  /** The menu explains itself once a session, not on every click. */
  #explained = false;

  constructor(options: MenuBarOptions) {
    this.#options = options;

    const bar = el('div', { class: 'menubar-titles', role: 'menubar' });
    options.menus.forEach((menu, index) => {
      const title = el('button', {
        class: 'menubar-title',
        type: 'button',
        role: 'menuitem',
        'aria-haspopup': 'true',
        'aria-expanded': 'false',
        text: menu.title,
      });
      title.addEventListener('click', () => {
        if (this.#openIndex === index) this.close();
        else this.#openAt(index);
      });
      // Once one menu is down, sliding along the bar walks the others, the way
      // every menu bar has since 1984.
      title.addEventListener('mouseenter', () => {
        if (this.#openIndex !== undefined && this.#openIndex !== index) this.#openAt(index);
      });
      this.#titles.push(title);
      bar.append(title);
    });

    this.#hint = el('div', { class: 'menu-hint' });
    this.#dropdown = el('div', {
      class: 'menu-dropdown',
      role: 'menu',
      tabindex: '-1',
      hidden: true,
    });
    this.#note = el('div', { class: 'menubar-note' });

    this.root = el('nav', { class: 'menubar', 'aria-label': 'Menu bar' }, [
      bar,
      this.#note,
      this.#dropdown,
    ]);

    // Keeping the pointer from moving focus is what lets the dropdown keep it,
    // and the keyboard keep working, while the mouse walks the entries.
    this.root.addEventListener('mousedown', (event) => event.preventDefault());
    this.root.addEventListener('keydown', (event) => this.#onKeyDown(event));
    this.root.addEventListener('focusout', (event) => {
      const next = event.relatedTarget as Node | null;
      if (next && this.root.contains(next)) return;
      // Something else took the keyboard — the prompt, a panel, another window.
      // Taking it back would fight whoever asked for it.
      this.#close(false);
    });

    document.addEventListener('pointerdown', (event) => {
      if (!this.isOpen) return;
      if (event.target instanceof Node && this.root.contains(event.target)) return;
      this.close();
    });

    this.syncSubject();
  }

  /* ----------------------------------------------------------- MenuControl */

  get isOpen(): boolean {
    return this.#openIndex !== undefined;
  }

  titles(): string[] {
    return this.#options.menus.map((menu) => menu.title);
  }

  open(name: string): boolean {
    const index = findMenu(this.#options.menus, name);
    if (index === -1) return false;
    this.#openAt(index);
    return true;
  }

  toggle(): void {
    if (this.isOpen) this.close();
    else this.#openAt(this.#openIndex ?? 0);
  }

  close(): void {
    this.#close(true);
  }

  cycle(delta: number): void {
    const count = this.#options.menus.length;
    if (count === 0) return;
    const current = this.#openIndex ?? 0;
    this.#openAt((current + delta + count) % count);
  }

  /**
   * Refresh the right-hand note.
   *
   * It carries `$` — the thing every `$` binding and menu entry would act on —
   * because the substitution is the one part of this terminal that is invisible
   * until it goes wrong.
   */
  syncSubject(): void {
    const subject = this.#options.subject();
    const binding = bindingsFor(this.#options.keymap, 'MENU')[0];
    const text = subject
      ? `$ = ${subject}`
      : binding
        ? `${chordLabel(binding)} opens these menus`
        : '';

    if (this.#note.textContent !== text) this.#note.textContent = text;
    this.#note.classList.toggle('has-subject', subject !== undefined);
    if (subject) this.#note.title = `$ stands for ${subject}`;
    else this.#note.removeAttribute('title');
  }

  /* --------------------------------------------------------------- opening */

  #openAt(index: number): void {
    const menu = this.#options.menus[index];
    if (!menu) return;

    if (!this.isOpen) {
      this.#returnMode = this.#options.mode();
      this.#subject = this.#options.subject();
    }

    this.#closeFrom(0);
    this.#openIndex = index;
    this.#titles.forEach((title, i) => {
      title.classList.toggle('is-open', i === index);
      title.setAttribute('aria-expanded', i === index ? 'true' : 'false');
    });

    const level = this.#renderLevel(menu.entries);
    this.#dropdown.append(level.list, this.#hint);
    this.#levels = [level];
    this.#dropdown.hidden = false;
    this.#describe(undefined, menu.hint);

    // Under its own title, unless that would hang it off the right-hand edge.
    const title = this.#titles[index];
    const left = title ? title.offsetLeft : 0;
    const room = this.root.clientWidth - this.#dropdown.offsetWidth - 4;
    this.#dropdown.style.left = `${Math.max(4, Math.min(left, Math.max(4, room)))}px`;

    this.#dropdown.focus({ preventScroll: true });
    this.syncSubject();
  }

  #close(restore: boolean): void {
    if (!this.isOpen) return;

    this.#cancelHover();
    this.#closeFrom(0);
    this.#openIndex = undefined;
    this.#dropdown.hidden = true;
    this.#dropdown.replaceChildren();
    for (const title of this.#titles) {
      title.classList.remove('is-open');
      title.setAttribute('aria-expanded', 'false');
    }
    this.#typeahead = '';

    // Hand the keyboard back to whichever half of the terminal had it.
    if (restore) this.#options.setMode(this.#returnMode);
  }

  /* ------------------------------------------------------------- rendering */

  #renderLevel(entries: readonly MenuEntry[]): Level {
    // The dropdown itself is the menu; this is the box its entries sit in. A
    // flyout is a menu in its own right, and says so when it is attached.
    const list = el('div', { class: 'menu-list', role: 'none' });
    const items: RenderedItem[] = [];
    const level: Level = { list, items, index: -1 };

    for (const entry of entries) {
      if (entry.kind === 'separator') {
        list.append(
          entry.label === undefined
            ? el('div', { class: 'menu-sep' })
            : el('div', { class: 'menu-caption', text: entry.label }),
        );
        continue;
      }

      const enabled = entry.kind === 'group' || this.#resolve(entry.command) !== null;
      const node = this.#renderEntry(entry, enabled);
      const position = items.length;
      items.push({ entry, node, enabled });

      node.addEventListener('mouseenter', () => this.#hover(level, position));
      node.addEventListener('click', () => {
        this.#cancelHover();
        this.#highlight(level, position);
        this.#activate(level);
      });

      if (entry.kind === 'group') list.append(el('div', { class: 'menu-node' }, [node]));
      else list.append(node);
    }

    return level;
  }

  #renderEntry(entry: MenuItem | MenuGroup, enabled: boolean): HTMLButtonElement {
    const node = el('button', {
      class: `menu-item${entry.kind === 'group' ? ' is-group' : ''}${enabled ? '' : ' is-disabled'}`,
      type: 'button',
      role: 'menuitem',
      ...(entry.kind === 'group' ? { 'aria-haspopup': 'true' } : {}),
      ...(enabled ? {} : { 'aria-disabled': 'true' }),
    });

    node.append(el('span', { class: 'menu-label', text: entry.label }));

    if (entry.kind === 'group') {
      node.append(el('span', { class: 'menu-arrow', text: '▸' }));
      return node;
    }

    // A worked example is its own label; printing it twice would only crowd the
    // row it is trying to teach.
    if (entry.command !== entry.label) {
      node.append(el('span', { class: 'menu-command', text: promptForm(entry.command) }));
    }

    const bindings = bindingsFor(this.#options.keymap, entry.command).slice(0, 2);
    node.append(el('span', { class: 'menu-chord', text: bindings.map(chordLabel).join(' · ') }));
    return node;
  }

  /** The hint strip: usage above, what it does and what it will do below. */
  #describe(item: RenderedItem | undefined, fallback?: string): void {
    this.#hint.replaceChildren();

    if (!item) {
      this.#hint.append(el('span', { class: 'menu-hint-text', text: fallback ?? '' }));
      return;
    }

    if (item.entry.kind === 'group') {
      const count = item.entry.entries.filter((entry) => entry.kind !== 'separator').length;
      this.#hint.append(
        el('div', { class: 'menu-hint-line' }, [
          el('span', { class: 'menu-hint-text', text: item.entry.label }),
          el('span', { class: 'menu-hint-aside', text: `${count} more · → opens it` }),
        ]),
      );
      return;
    }

    const entry = item.entry;
    const command = commandOf(entry);
    const typed = entry.command.startsWith('>');
    const asides: string[] = [];

    if (!item.enabled) asides.push(SUBJECT_HINT);
    else if (typed) asides.push('types it at the prompt — fill in the rest and press Enter');
    if (command) asides.push(`? for HELP ${command.verb}`);

    this.#hint.append(
      el('span', { class: 'menu-hint-usage', text: command?.usage ?? entry.command }),
      el('div', { class: 'menu-hint-line' }, [
        el('span', { class: 'menu-hint-text', text: entry.note ?? command?.summary ?? '' }),
        el('span', { class: 'menu-hint-aside', text: asides.join(' · ') }),
      ]),
    );
  }

  /* ----------------------------------------------------------- interaction */

  /**
   * The mouse arriving on an entry.
   *
   * Highlighting is immediate, with one exception: the path from a group entry
   * to its flyout runs diagonally across the entries below it, and a sibling
   * that is merely passed over must not tear down the list the pointer is
   * heading for. So while a flyout is open, a sibling has to be settled on.
   */
  #hover(level: Level, index: number): void {
    this.#cancelHover();
    const depth = this.#levels.indexOf(level);
    if (depth === -1) return;

    const settle = (): void => {
      this.#hoverTimer = undefined;
      this.#highlight(level, index);
      if (level.items[index]?.entry.kind === 'group') this.#enter(level);
    };

    if (this.#levels.length > depth + 1 && level.index !== index) {
      this.#hoverTimer = window.setTimeout(settle, HOVER_SETTLE_MS);
      return;
    }

    settle();
  }

  #cancelHover(): void {
    if (this.#hoverTimer !== undefined) window.clearTimeout(this.#hoverTimer);
    this.#hoverTimer = undefined;
  }

  #highlight(level: Level, index: number): void {
    const depth = this.#levels.indexOf(level);
    if (depth === -1) return;
    // Moving in a list closes whatever was open below it.
    if (depth + 1 < this.#levels.length) this.#closeFrom(depth + 1);

    level.index = index;
    level.items.forEach((item, i) => item.node.classList.toggle('is-active', i === index));
    this.#describe(level.items[index]);
  }

  /** Move the highlight, skipping nothing and wrapping at both ends. */
  #step(delta: number): void {
    const level = this.#levels.at(-1);
    if (!level || level.items.length === 0) return;
    const count = level.items.length;
    const from = level.index === -1 ? (delta > 0 ? -1 : 0) : level.index;
    this.#highlight(level, (from + delta + count) % count);
  }

  #stepTo(index: number): void {
    const level = this.#levels.at(-1);
    if (!level || level.items.length === 0) return;
    this.#highlight(level, Math.min(Math.max(index, 0), level.items.length - 1));
  }

  /** Open the flyout under the highlighted group. */
  #enter(target?: Level): void {
    const level = target ?? this.#levels.at(-1);
    if (!level) return;
    const depth = this.#levels.indexOf(level);
    if (depth === -1) return;

    const item = level.items[level.index];
    if (!item || item.entry.kind !== 'group') return;
    // Already open on this one.
    if (this.#levels.length > depth + 1) return;

    const child = this.#renderLevel(item.entry.entries);
    child.list.classList.add('menu-flyout');
    child.list.setAttribute('role', 'menu');
    item.node.parentElement?.append(child.list);
    this.#levels.push(child);
    this.#place(child.list);
    if (child.items.length > 0) this.#highlight(child, 0);
  }

  /**
   * Keep a flyout on screen.
   *
   * Measured rather than guessed: these lists are built from the command table,
   * so their size is whatever the terminal happens to know that week, and the
   * window is whatever the reader has dragged it to. To the right by
   * preference, to the left when the right-hand edge is close, and pinned
   * inside the window when neither side has the room — a menu half off the
   * screen teaches nothing.
   */
  #place(list: HTMLElement): void {
    const anchor = list.parentElement?.getBoundingClientRect();
    if (!anchor) return;

    const width = list.offsetWidth;
    const fitsRight = anchor.right + width <= window.innerWidth - 4;
    const fitsLeft = anchor.left - width >= 4;

    if (!fitsRight && fitsLeft) list.classList.add('is-flipped');
    else if (!fitsRight) {
      const left = Math.max(4, window.innerWidth - 4 - width);
      list.style.left = `${Math.round(left - anchor.left)}px`;
    }

    const rect = list.getBoundingClientRect();
    const spill = rect.bottom - (window.innerHeight - 8);
    if (spill > 0) list.style.marginTop = `-${Math.ceil(Math.min(spill, rect.top - 8))}px`;
  }

  #closeFrom(depth: number): void {
    for (const level of this.#levels.splice(depth)) level.list.remove();
    const parent = this.#levels.at(-1);
    if (parent) this.#describe(parent.items[parent.index]);
  }

  #activate(target?: Level): void {
    const level = target ?? this.#levels.at(-1);
    const item = level?.items[level.index];
    if (!level || !item) return;

    if (item.entry.kind === 'group') {
      this.#enter(level);
      return;
    }
    if (!item.enabled) {
      this.#options.log(`${item.entry.command} ${SUBJECT_HINT}`, 'warn');
      return;
    }

    this.#fire(item.entry);
  }

  /**
   * Run an entry, and say what was run.
   *
   * The echo is the teaching: it puts the resolved command line in the log in
   * the same shape a typed one takes, and on the history stack where `↑` will
   * find it. What the reader clicked once, they can retype — or press.
   */
  #fire(entry: MenuItem): void {
    const resolved = this.#resolve(entry.command);
    if (resolved === null) {
      this.#options.log(`${entry.command} ${SUBJECT_HINT}`, 'warn');
      return;
    }

    // Close first: restoring the keyboard while the panels are still the ones
    // that were on screen keeps `CLS ALL` from restoring focus to nothing.
    this.close();
    this.#options.run(resolved);

    const binding = bindingsFor(this.#options.keymap, entry.command)[0];

    if (resolved.startsWith('>')) {
      this.#options.log(
        binding
          ? `${promptForm(entry.command)} — ${chordLabel(binding)} starts the same line.`
          : `${promptForm(entry.command)} — type the rest and press Enter.`,
      );
    } else {
      this.#options.echo(resolved);
      if (binding) {
        // What the key runs is the entry's own line, `$` and all: the reader is
        // about to press it somewhere else, where `$` will mean something else.
        const what = entry.command === resolved ? 'that' : entry.command;
        this.#options.log(
          binding.scope === 'panel'
            ? `In NAV mode, ${formatChord(binding.chord)} runs ${what}.`
            : `${formatChord(binding.chord)} runs ${what} from anywhere.`,
        );
      }
    }

    if (!this.#explained) {
      this.#explained = true;
      this.#options.log('Every menu entry is a command you can type — ↑ recalls the last one.');
    }
  }

  /** `HELP` for the highlighted entry, which is what `?` means everywhere else. */
  #explain(): void {
    const level = this.#levels.at(-1);
    const item = level?.items[level.index];
    if (!item || item.entry.kind !== 'item') return;

    const verb = commandOf(item.entry)?.verb ?? verbOf(item.entry.command);
    if (!verb) return;

    this.close();
    this.#options.run(`HELP ${verb}`);
    this.#options.echo(`HELP ${verb}`);
  }

  #resolve(command: string): string | null {
    // A `>` line is typed, not run, so it never needs a subject filled in.
    if (command.startsWith('>')) return command;
    return applySubject(command, this.#subject);
  }

  /* -------------------------------------------------------------- keyboard */

  #onKeyDown(event: KeyboardEvent): void {
    if (!this.isOpen) return;
    const level = this.#levels.at(-1);
    if (!level) return;

    // The keyboard has taken over; a hover the pointer left half-finished is
    // not about to move the highlight out from under it.
    this.#cancelHover();

    const claim = (): void => {
      event.preventDefault();
      event.stopPropagation();
    };

    switch (event.key) {
      case 'ArrowDown':
        claim();
        this.#step(1);
        return;
      case 'ArrowUp':
        claim();
        this.#step(-1);
        return;
      case 'Home':
        claim();
        this.#stepTo(0);
        return;
      case 'End':
        claim();
        this.#stepTo(level.items.length - 1);
        return;
      case 'ArrowRight':
        claim();
        if (level.items[level.index]?.entry.kind === 'group') this.#enter();
        else this.cycle(1);
        return;
      case 'ArrowLeft':
        claim();
        if (this.#levels.length > 1) this.#closeFrom(this.#levels.length - 1);
        else this.cycle(-1);
        return;
      case 'Enter':
      case ' ':
        claim();
        this.#activate();
        return;
      case 'Escape':
        claim();
        if (this.#levels.length > 1) this.#closeFrom(this.#levels.length - 1);
        else this.close();
        return;
      case 'Tab':
        claim();
        this.cycle(event.shiftKey ? -1 : 1);
        return;
      case '?':
        claim();
        this.#explain();
        return;
      default:
        break;
    }

    // A letter jumps to the entry it names. One that names nothing is left to
    // bubble: the key map will take it to the prompt with the character intact,
    // which is what an unbound letter does everywhere else in this terminal.
    if (event.key.length === 1 && !event.ctrlKey && !event.altKey && !event.metaKey) {
      if (this.#typeAhead(event.key)) claim();
    }
  }

  #typeAhead(char: string): boolean {
    const level = this.#levels.at(-1);
    if (!level || level.items.length === 0) return false;

    const now = Date.now();
    this.#typeahead = now - this.#typeaheadAt > TYPEAHEAD_MS ? char : this.#typeahead + char;
    this.#typeaheadAt = now;

    const needle = this.#typeahead.toLowerCase();
    const matches = (index: number): boolean =>
      level.items[index]!.entry.label.toLowerCase().startsWith(needle);

    // One letter twice running means "the next one that starts with it".
    const start = this.#typeahead.length === 1 ? level.index + 1 : 0;
    for (let i = 0; i < level.items.length; i++) {
      const index = (start + i) % level.items.length;
      if (matches(index)) {
        this.#highlight(level, index);
        return true;
      }
    }

    return false;
  }
}
