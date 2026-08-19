<!--
  The menu bar: everything the terminal does, reachable without remembering it.

  It is a teaching surface first and a mouse convenience second, so every entry
  shows three things at once — what it does, the command line it runs, and the
  key that runs it without the menu. Choosing one echoes that command into the
  message log and pushes it onto the history, so the next time it is `↑` away
  and the time after that it is muscle memory. A menu that only did the thing
  would leave you needing it forever.

  Nothing here is a second dispatch path: an entry hands its command line to the
  same `run()` a typed line goes to, `$` is filled the way a key binding fills
  it, and the chord column is read out of the live keymap — so a key rebound
  with `KEYS` is relabelled here without anyone being told.
-->
<script lang="ts">
  import type { LogLevel } from '../context';
  import {
    applySubject,
    formatChord,
    SUBJECT_HINT,
    type KeyMode,
    type Keymap,
  } from '../terminal/keys';
  import {
    bindingsFor,
    chordLabel,
    commandOf,
    findMenu,
    MENUS,
    verbOf,
    type Menu,
    type MenuEntry,
    type MenuGroup,
    type MenuItem,
  } from '../terminal/menu';

  interface Props {
    keymap: Keymap;
    /**
     * Dispatch a command line, exactly as if it had been typed.
     *
     * That "exactly" is what makes the echo work: `run` already puts the line
     * in the log and on the history stack, so a menu choice is indistinguishable
     * from a typed one afterwards — which is the whole lesson.
     */
    run(command: string): void;
    log(message: string, level?: LogLevel): void;
    /** `$` — the row under the cursor, or the focused panel's own subject. */
    subject(): string | undefined;
    /** Where the keyboard was before the menu took it. */
    mode(): KeyMode;
    /** Give it back. */
    setMode(mode: KeyMode): void;
    menus?: readonly Menu[];
  }

  const { keymap, run, log, subject, mode, setMode, menus = MENUS }: Props = $props();

  /** How long a type-ahead prefix survives between keystrokes. */
  const TYPEAHEAD_MS = 900;
  /** How long the pointer has to rest on a sibling before it closes a flyout. */
  const HOVER_SETTLE_MS = 220;

  /** One open list: the dropdown itself, or a flyout hanging off a group. */
  interface Level {
    entries: readonly MenuEntry[];
    /** Index into `choosable`, or -1 for nothing highlighted. */
    index: number;
  }

  let openIndex = $state<number | undefined>(undefined);
  let levels = $state<Level[]>([]);
  let typeahead = '';
  let typeaheadAt = 0;
  let hoverTimer: ReturnType<typeof setTimeout> | undefined;
  let returnMode: KeyMode = 'cmd';
  /** `$` as it stood when the menu opened; nothing can change it while it is up. */
  let frozenSubject: string | undefined;
  /** The menu explains itself once a session, not on every choice. */
  let explained = false;

  let root = $state<HTMLElement | undefined>(undefined);
  let dropdown = $state<HTMLElement | undefined>(undefined);
  let titleNodes: HTMLButtonElement[] = [];

  const isOpen = $derived(openIndex !== undefined);

  /** The entries that can be highlighted — separators are scenery. */
  function choosable(entries: readonly MenuEntry[]): (MenuItem | MenuGroup)[] {
    return entries.filter((entry): entry is MenuItem | MenuGroup => entry.kind !== 'separator');
  }

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

  /** A `>` line is typed, not run, so it never needs a subject filled in. */
  function resolve(command: string): string | null {
    if (command.startsWith('>')) return command;
    return applySubject(command, frozenSubject);
  }

  function enabled(entry: MenuItem | MenuGroup): boolean {
    return entry.kind === 'group' || resolve(entry.command) !== null;
  }

  /* ------------------------------------------------------------ the control */

  export function titles(): string[] {
    return menus.map((menu) => menu.title);
  }

  export function open(name: string): boolean {
    const index = findMenu(menus, name);
    if (index === -1) return false;
    openAt(index);
    return true;
  }

  export function toggle(): void {
    if (isOpen) close();
    else openAt(openIndex ?? 0);
  }

  export function close(): void {
    shut(true);
  }

  export function cycle(delta: number): void {
    if (menus.length === 0) return;
    openAt(((openIndex ?? 0) + delta + menus.length) % menus.length);
  }

  export function opened(): boolean {
    return isOpen;
  }

  /* --------------------------------------------------------------- opening */

  function openAt(index: number): void {
    if (!menus[index]) return;

    if (!isOpen) {
      returnMode = mode();
      frozenSubject = subject();
    }

    cancelHover();
    openIndex = index;
    levels = [{ entries: menus[index].entries, index: -1 }];
    typeahead = '';

    // The dropdown has to exist before it can take the keyboard.
    queueMicrotask(() => dropdown?.focus({ preventScroll: true }));
  }

  function shut(restore: boolean): void {
    if (!isOpen) return;
    cancelHover();
    openIndex = undefined;
    levels = [];
    typeahead = '';
    // Hand the keyboard back to whichever half of the terminal had it.
    if (restore) setMode(returnMode);
  }

  /* ----------------------------------------------------------- interaction */

  function highlight(depth: number, index: number): void {
    if (depth >= levels.length) return;
    // Moving in a list closes whatever was open below it.
    levels = levels
      .slice(0, depth + 1)
      .map((level, i) => (i === depth ? { ...level, index } : level));
  }

  /** Open the flyout under the highlighted group. */
  function enter(depth: number): void {
    const level = levels[depth];
    const entry = level && choosable(level.entries)[level.index];
    if (!entry || entry.kind !== 'group') return;
    if (levels.length > depth + 1) return;

    levels = [...levels, { entries: entry.entries, index: 0 }];
  }

  /**
   * The mouse arriving on an entry.
   *
   * Highlighting is immediate, with one exception: the path from a group entry
   * to its flyout runs diagonally across the entries below it, and a sibling
   * that is merely passed over must not tear down the list the pointer is
   * heading for. So while a flyout is open, a sibling has to be settled on.
   */
  function hover(depth: number, index: number): void {
    cancelHover();

    const settle = (): void => {
      hoverTimer = undefined;
      highlight(depth, index);
      const entry = choosable(levels[depth]?.entries ?? [])[index];
      if (entry?.kind === 'group') enter(depth);
    };

    if (levels.length > depth + 1 && levels[depth]?.index !== index) {
      hoverTimer = setTimeout(settle, HOVER_SETTLE_MS);
      return;
    }

    settle();
  }

  function cancelHover(): void {
    if (hoverTimer !== undefined) clearTimeout(hoverTimer);
    hoverTimer = undefined;
  }

  /** Move the highlight, skipping nothing and wrapping at both ends. */
  function step(delta: number): void {
    const depth = levels.length - 1;
    const level = levels[depth];
    if (!level) return;
    const count = choosable(level.entries).length;
    if (count === 0) return;
    const from = level.index === -1 ? (delta > 0 ? -1 : 0) : level.index;
    highlight(depth, (from + delta + count) % count);
  }

  function stepTo(index: number): void {
    const depth = levels.length - 1;
    const level = levels[depth];
    if (!level) return;
    const count = choosable(level.entries).length;
    if (count === 0) return;
    highlight(depth, Math.min(Math.max(index, 0), count - 1));
  }

  function activate(depth: number): void {
    const level = levels[depth];
    const entry = level && choosable(level.entries)[level.index];
    if (!entry) return;

    if (entry.kind === 'group') {
      enter(depth);
      return;
    }
    if (!enabled(entry)) {
      log(`${entry.command} ${SUBJECT_HINT}`, 'warn');
      return;
    }
    fire(entry);
  }

  /**
   * Run an entry, and say what was run.
   *
   * The echo is the teaching, and it comes free: `run` puts the resolved line in
   * the log in the same shape a typed one takes, and on the history stack where
   * `↑` will find it. What the reader chose once, they can retype — or press.
   */
  function fire(entry: MenuItem): void {
    const resolved = resolve(entry.command);
    if (resolved === null) {
      log(`${entry.command} ${SUBJECT_HINT}`, 'warn');
      return;
    }

    // Close first: restoring the keyboard while the panels are still the ones
    // that were on screen keeps `CLS ALL` from restoring focus to nothing.
    close();
    run(resolved);

    const binding = bindingsFor(keymap, entry.command)[0];

    if (resolved.startsWith('>')) {
      log(
        binding
          ? `${promptForm(entry.command)} — ${chordLabel(binding)} starts the same line.`
          : `${promptForm(entry.command)} — type the rest and press Enter.`,
      );
    } else {
      if (binding) {
        // What the key runs is the entry's own line, `$` and all: the reader is
        // about to press it somewhere else, where `$` will mean something else.
        const what = entry.command === resolved ? 'that' : entry.command;
        log(
          binding.scope === 'panel'
            ? `In NAV mode, ${formatChord(binding.chord)} runs ${what}.`
            : `${formatChord(binding.chord)} runs ${what} from anywhere.`,
        );
      }
    }

    if (!explained) {
      explained = true;
      log('Every menu entry is a command you can type — ↑ recalls the last one.');
    }
  }

  /** `HELP` for the highlighted entry, which is what `?` means everywhere else. */
  function explain(): void {
    const level = levels.at(-1);
    const entry = level && choosable(level.entries)[level.index];
    if (!entry || entry.kind !== 'item') return;

    const verb = commandOf(entry)?.verb ?? verbOf(entry.command);
    if (!verb) return;

    close();
    run(`HELP ${verb}`);
  }

  /* -------------------------------------------------------------- keyboard */

  function onKeyDown(event: KeyboardEvent): void {
    if (!isOpen) return;
    const depth = levels.length - 1;
    const level = levels[depth];
    if (!level) return;

    // The keyboard has taken over; a hover the pointer left half-finished is
    // not about to move the highlight out from under it.
    cancelHover();

    const claim = (): void => {
      event.preventDefault();
      event.stopPropagation();
    };

    switch (event.key) {
      case 'ArrowDown':
        claim();
        step(1);
        return;
      case 'ArrowUp':
        claim();
        step(-1);
        return;
      case 'Home':
        claim();
        stepTo(0);
        return;
      case 'End':
        claim();
        stepTo(choosable(level.entries).length - 1);
        return;
      case 'ArrowRight':
        claim();
        if (choosable(level.entries)[level.index]?.kind === 'group') enter(depth);
        else cycle(1);
        return;
      case 'ArrowLeft':
        claim();
        if (levels.length > 1) levels = levels.slice(0, -1);
        else cycle(-1);
        return;
      case 'Enter':
      case ' ':
        claim();
        activate(depth);
        return;
      case 'Escape':
        claim();
        if (levels.length > 1) levels = levels.slice(0, -1);
        else close();
        return;
      case 'Tab':
        claim();
        cycle(event.shiftKey ? -1 : 1);
        return;
      case '?':
        claim();
        explain();
        return;
      default:
        break;
    }

    // A letter jumps to the entry it names. One that names nothing is left to
    // bubble: the key map will take it to the prompt with the character intact,
    // which is what an unbound letter does everywhere else in this terminal.
    if (event.key.length === 1 && !event.ctrlKey && !event.altKey && !event.metaKey) {
      if (typeAhead(event.key)) claim();
    }
  }

  function typeAhead(char: string): boolean {
    const depth = levels.length - 1;
    const level = levels[depth];
    if (!level) return false;
    const items = choosable(level.entries);
    if (items.length === 0) return false;

    const now = Date.now();
    typeahead = now - typeaheadAt > TYPEAHEAD_MS ? char : typeahead + char;
    typeaheadAt = now;

    const needle = typeahead.toLowerCase();
    // One letter twice running means "the next one that starts with it".
    const start = typeahead.length === 1 ? level.index + 1 : 0;

    for (let i = 0; i < items.length; i++) {
      const index = (start + i) % items.length;
      if (items[index]!.label.toLowerCase().startsWith(needle)) {
        highlight(depth, index);
        return true;
      }
    }

    return false;
  }

  /* ------------------------------------------------------------- the hint */

  /** The strip under the list: usage above, what it does and will do below. */
  const hint = $derived.by(() => {
    const level = levels.at(-1);
    const entry = level && choosable(level.entries)[level.index];

    if (!entry) {
      return {
        usage: '',
        text: openIndex === undefined ? '' : (menus[openIndex]?.hint ?? ''),
        aside: '',
      };
    }

    if (entry.kind === 'group') {
      const count = entry.entries.filter((e) => e.kind !== 'separator').length;
      return { usage: '', text: entry.label, aside: `${count} more · → opens it` };
    }

    const command = commandOf(entry);
    const asides: string[] = [];
    if (!enabled(entry)) asides.push(SUBJECT_HINT);
    else if (entry.command.startsWith('>')) {
      asides.push('types it at the prompt — fill in the rest and press Enter');
    }
    if (command) asides.push(`? for HELP ${command.verb}`);

    return {
      usage: command?.usage ?? entry.command,
      text: entry.note ?? command?.summary ?? '',
      aside: asides.join(' · '),
    };
  });

  /**
   * The right-hand note: `$`, or the key that opens these menus.
   *
   * It carries `$` because the substitution is the one part of this terminal
   * that is invisible until it goes wrong.
   */
  const note = $derived.by(() => {
    const current = isOpen ? frozenSubject : subject();
    if (current) return { text: `$ = ${current}`, subject: current };
    const binding = bindingsFor(keymap, 'MENU')[0];
    return { text: binding ? `${chordLabel(binding)} opens these menus` : '', subject: undefined };
  });

  /**
   * Keep a flyout on screen.
   *
   * Measured rather than guessed: these lists are built from the command table,
   * so their size is whatever the terminal happens to know that week, and the
   * window is whatever the reader has dragged it to. To the right by preference,
   * to the left when the right-hand edge is close, and pinned inside the window
   * when neither side has the room — a menu half off the screen teaches nothing.
   */
  function place(list: HTMLElement): void {
    const anchor = list.parentElement?.getBoundingClientRect();
    if (!anchor) return;

    list.classList.remove('is-flipped');
    list.style.left = '';
    list.style.marginTop = '';

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

  /**
   * Position a list once it is in the DOM.
   *
   * An action rather than an effect: both `place` and `placeDropdown` measure
   * the node and its neighbours, so they can only run after the browser has one
   * to measure, and an action is the hook that guarantees it.
   */
  function positioned(node: HTMLElement, depth: number) {
    const apply = (at: number): void => {
      if (at > 0) place(node);
      else if (openIndex !== undefined) placeDropdown(node, openIndex);
    };

    apply(depth);
    return {
      update(next: number) {
        apply(next);
      },
    };
  }

  /** Under its own title, unless that would hang it off the right-hand edge. */
  function placeDropdown(list: HTMLElement, index: number): void {
    const title = titleNodes[index];
    const left = title ? title.offsetLeft : 0;
    const room = (root?.clientWidth ?? 0) - list.offsetWidth - 4;
    list.style.left = `${Math.max(4, Math.min(left, Math.max(4, room)))}px`;
  }

  function onWindowPointerDown(event: PointerEvent): void {
    if (!isOpen) return;
    if (event.target instanceof Node && root?.contains(event.target)) return;
    close();
  }

  function onFocusOut(event: FocusEvent): void {
    const next = event.relatedTarget as Node | null;
    if (next && root?.contains(next)) return;
    // Something else took the keyboard — the prompt, a panel, another window.
    // Taking it back would fight whoever asked for it.
    shut(false);
  }
</script>

<svelte:window onpointerdown={onWindowPointerDown} />

{#snippet list(depth: number)}
  {@const level = levels[depth]}
  {#if level}
    {@const items = choosable(level.entries)}
    <div
      class="menu-list"
      class:menu-flyout={depth > 0}
      role={depth > 0 ? 'menu' : 'none'}
      use:positioned={depth}
    >
      {#each level.entries as entry, position (position)}
        {#if entry.kind === 'separator'}
          {#if entry.label === undefined}
            <div class="menu-sep"></div>
          {:else}
            <div class="menu-caption">{entry.label}</div>
          {/if}
        {:else}
          {@const index = items.indexOf(entry)}
          {@const on = level.index === index}
          {@const live = enabled(entry)}
          {@const bindings = bindingsFor(keymap, entry.kind === 'item' ? entry.command : '').slice(
            0,
            2,
          )}
          <div class="menu-node">
            <button
              class="menu-item"
              class:is-group={entry.kind === 'group'}
              class:is-disabled={!live}
              class:is-active={on}
              type="button"
              role="menuitem"
              aria-haspopup={entry.kind === 'group' ? 'true' : undefined}
              aria-disabled={live ? undefined : 'true'}
              onmouseenter={() => hover(depth, index)}
              onclick={() => {
                cancelHover();
                highlight(depth, index);
                activate(depth);
              }}
            >
              <span class="menu-label">{entry.label}</span>
              {#if entry.kind === 'group'}
                <span class="menu-arrow">▸</span>
              {:else}
                <!-- A worked example is its own label; printing it twice would
                     only crowd the row it is trying to teach. -->
                {#if entry.command !== entry.label}
                  <span class="menu-command">{promptForm(entry.command)}</span>
                {/if}
                <span class="menu-chord">{bindings.map(chordLabel).join(' · ')}</span>
              {/if}
            </button>
            {#if entry.kind === 'group' && on && levels.length > depth + 1}
              {@render list(depth + 1)}
            {/if}
          </div>
        {/if}
      {/each}
    </div>
  {/if}
{/snippet}

<!--
  The landmark is the `nav`; the handlers live on the group inside it, which is
  the thing that actually has a menubar role to justify them.

  Keeping the pointer from moving focus is what lets the dropdown keep it, and
  the keyboard keep working, while the mouse walks the entries.
-->
<nav class="menubar" aria-label="Menu bar" bind:this={root}>
  <div
    class="menubar-titles"
    role="menubar"
    tabindex="-1"
    onmousedown={(event) => event.preventDefault()}
    onkeydown={onKeyDown}
    onfocusout={onFocusOut}
  >
    {#each menus as menu, index (menu.title)}
      <button
        class="menubar-title"
        class:is-open={openIndex === index}
        type="button"
        role="menuitem"
        aria-haspopup="true"
        aria-expanded={openIndex === index}
        bind:this={titleNodes[index]}
        onclick={() => (openIndex === index ? close() : openAt(index))}
        onmouseenter={() => {
          if (openIndex !== undefined && openIndex !== index) openAt(index);
        }}
      >
        {menu.title}
      </button>
    {/each}

    {#if isOpen}
      <div class="menu-dropdown" role="menu" tabindex="-1" bind:this={dropdown}>
        {@render list(0)}
        <div class="menu-hint">
          {#if hint.usage}
            <span class="menu-hint-usage">{hint.usage}</span>
          {/if}
          <div class="menu-hint-line">
            <span class="menu-hint-text">{hint.text}</span>
            <span class="menu-hint-aside">{hint.aside}</span>
          </div>
        </div>
      </div>
    {/if}
  </div>

  <div
    class="menubar-note"
    class:has-subject={note.subject !== undefined}
    title={note.subject ? `$ stands for ${note.subject}` : undefined}
  >
    {note.text}
  </div>
</nav>
