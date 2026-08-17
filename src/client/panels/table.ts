/**
 * Declarative table panels.
 *
 * Most panels in this terminal are the same shape: fetch a feed, print a
 * summary line, then a ranked table whose rows run a command when clicked.
 * Written out longhand that is about 120 lines each, and it was — eighteen
 * times, which is how the same field came to be rendered two different ways
 * in two panels whose own header comments say they read alike.
 *
 * A panel declares its columns instead. The spec carries everything that
 * actually varied between those eighteen: which columns appear at all (some
 * only when the data has them), how each cell is formatted and toned, what a
 * row does when clicked, and what to show when the feed is empty.
 *
 * What is deliberately *not* here: grouped sections (SRCH nests a table per
 * event), ladders with synthetic rows (OB inserts a spread row between the two
 * sides), and the chart panels. Those are genuinely different renderings, and
 * bending this spec around them would cost more than the duplication saves.
 */

import { cell, el, row, table } from '../lib/dom.js';
import { Panel, type PanelContext } from './panel.js';

/** A run of text in the summary line, with an optional tone class. */
export interface NoteSegment {
  text: string;
  /** `up`, `down`, `dim`, … — a CSS class, not a colour. */
  tone?: string;
}

export type Note = string | NoteSegment[] | null;

/**
 * One column.
 *
 * `cell` returns a string for the common case, or an element when the cell has
 * structure — two stacked lines, an image, its own click target. Returning an
 * element is the escape hatch that keeps bespoke cells out of the spec.
 */
export interface Column<R> {
  /** Header text. Empty for an unlabelled column, e.g. artwork or a row action. */
  header: string;
  cell(row: R, index: number): string | HTMLElement;
  /** Class on the `<td>`; a function when it depends on the value's sign. */
  class?: string | ((row: R) => string);
  /** Tooltip on the `<td>` — usually the untruncated form of the cell text. */
  title?(row: R): string | undefined;
  /**
   * Include this column only when the predicate passes over the whole row set.
   *
   * Netflix publishes view counts globally but not per country, and a column
   * of em-dashes is worse than no column. Evaluated once per render, so the
   * header and the cells can never disagree about whether it is there.
   */
  when?(rows: R[]): boolean;
}

export interface TableSpec<T, R> {
  /** The rows to render, pulled out of the loaded payload. */
  rows(data: T): R[];
  /**
   * The columns. A function when the column *set* depends on the data — the
   * cross-venue compare panel grows two columns per venue it found.
   */
  columns: readonly Column<R>[] | ((data: T) => readonly Column<R>[]);
  /** The summary line above the table. */
  note?(data: T): Note;
  /** Shown instead of the table when there are no rows. */
  empty?(data: T): { message: string; hint?: string };
  /** Command to run when a row is clicked. `null` leaves the row inert. */
  rowCommand?(row: R): string | null;
  /** Row tooltip. Worth setting whenever `rowCommand` is: it says what will happen. */
  rowTitle?(row: R): string | undefined;
  rowClass?(row: R): string | undefined;
  /** Extra class on the `<table>`. */
  tableClass?: string;
}

/* ------------------------------------------------------------ shared cells */

/**
 * A cell of two stacked lines — a title over its artist, a headline over its
 * summary, a figure over its delta.
 *
 * The tooltip defaults to both lines joined, because the visible text is
 * almost always truncated and the untruncated form is what a hover is for.
 */
export function stackedCell(
  primary: string,
  secondary: string | null,
  options: { title?: string; primaryClass?: string; secondaryClass?: string } = {},
): HTMLElement {
  const wrap = el('div', { class: 'stacked-cell' }, [
    el('div', { class: options.primaryClass ?? 'bb-title', text: primary }),
    secondary ? el('div', { class: options.secondaryClass ?? 'bb-artist', text: secondary }) : null,
  ]);
  const title = options.title ?? (secondary ? `${primary} — ${secondary}` : primary);
  wrap.title = title;
  return wrap;
}

/**
 * Make an element run a command when it is activated.
 *
 * The command is recorded on the element as well as closed over, because the
 * keyboard needs to read it back: `$` in a key binding means "the thing the row
 * under the cursor is about", and the row's own command is where that is
 * written down. Anything clickable should go through here so the two never
 * disagree.
 */
export function bindRow(
  node: HTMLElement,
  command: string,
  run: (command: string) => void,
  title?: string,
): void {
  node.classList.add('clickable');
  node.dataset['command'] = command;
  if (title !== undefined) node.title = title;
  node.addEventListener('click', () => run(command));
}

/**
 * A click target inside a clickable row.
 *
 * Stops propagation, so the cell's own command runs instead of the row's —
 * a feed link on a market row, a remove button on a watchlist row.
 */
export function actionTarget(
  label: string,
  command: string,
  run: (command: string) => void,
  options: { tag?: 'span' | 'button'; class?: string; title?: string } = {},
): HTMLElement {
  const node = el(options.tag ?? 'button', {
    class: options.class ?? 'row-action',
    ...(options.tag === 'span' ? {} : { type: 'button' }),
    text: label,
  });
  if (options.title !== undefined) node.title = options.title;
  node.addEventListener('click', (event) => {
    event.stopPropagation();
    run(command);
  });
  return node;
}

/**
 * The risers / fallers / debuts tally that heads every ranked chart.
 *
 * One pass rather than three, and one definition rather than the two that had
 * drifted apart in the Billboard and stream-chart panels.
 */
export function movementNote(
  entries: readonly { move: number | null; isNew: boolean }[],
): NoteSegment[] {
  let risers = 0;
  let fallers = 0;
  let debuts = 0;
  for (const entry of entries) {
    if (entry.isNew) debuts++;
    const move = entry.move ?? 0;
    if (move > 0) risers++;
    else if (move < 0) fallers++;
  }
  return [
    { text: `${entries.length} entries` },
    { text: `  ▲ ${risers}`, tone: 'up' },
    { text: `  ▼ ${fallers}`, tone: 'down' },
    { text: `  NEW ${debuts}`, tone: 'dim' },
  ];
}

/** Render a summary line, or nothing when there is none. */
export function noteLine(note: Note): HTMLElement | null {
  if (note === null) return null;
  if (typeof note === 'string') return el('div', { class: 'result-note', text: note });
  if (note.length === 0) return null;
  return el(
    'div',
    { class: 'result-note' },
    note.map((segment) =>
      el('span', segment.tone ? { class: segment.tone, text: segment.text } : { text: segment.text }),
    ),
  );
}

/** The two-line empty state used across the terminal. */
export function emptyState(message: string, hint?: string): HTMLElement {
  return el('div', { class: 'panel-empty' }, [
    el('div', { text: message }),
    hint === undefined ? null : el('div', { class: 'panel-empty-hint', text: hint }),
  ]);
}

/**
 * Build a table from a spec.
 *
 * Exported separately from {@link TablePanel} so a panel that is *mostly*
 * bespoke can still render its table this way rather than by hand.
 */
export function renderTable<R>(
  rows: readonly R[],
  columns: readonly Column<R>[],
  options: {
    rowCommand?(row: R): string | null;
    rowTitle?(row: R): string | undefined;
    rowClass?(row: R): string | undefined;
    tableClass?: string;
    run?(command: string): void;
  } = {},
): HTMLTableElement {
  const list = [...rows];
  const active = columns.filter((column) => column.when?.(list) ?? true);

  const built = list.map((item, index) => {
    const cells = active.map((column) => {
      const content = column.cell(item, index);
      const className =
        typeof column.class === 'function' ? column.class(item) : column.class;

      if (typeof content === 'string') {
        return cell(content, className, 'td', column.title?.(item));
      }

      const td = cell('', className, 'td', column.title?.(item));
      td.append(content);
      return td;
    });

    const tr = row(cells, options.rowClass?.(item));

    const command = options.rowCommand?.(item) ?? null;
    if (command !== null && options.run) {
      bindRow(tr, command, options.run, options.rowTitle?.(item));
    }

    return tr;
  });

  return table(
    active.map((column) => column.header),
    built,
    options.tableClass ?? '',
  );
}

/**
 * A panel that renders one table.
 *
 * Subclasses supply `load()`, `title()` and a {@link TableSpec}; everything
 * between the fetch and the DOM is handled here.
 */
export abstract class TablePanel<T, R> extends Panel<T> {
  protected abstract spec(): TableSpec<T, R>;

  protected override render(data: T): void {
    const spec = this.spec();
    const rows = spec.rows(data);

    const note = noteLine(spec.note?.(data) ?? null);
    if (note) this.body.append(note);

    if (rows.length === 0) {
      // A panel that declares no empty state still has to say something — a
      // blank pane under a summary line reads as a rendering failure.
      const empty = spec.empty?.(data) ?? { message: 'Nothing to show.' };
      this.body.append(emptyState(empty.message, empty.hint));
      return;
    }

    const columns =
      typeof spec.columns === 'function' ? spec.columns(data) : spec.columns;

    this.body.append(
      renderTable(rows, columns, {
        ...(spec.rowCommand ? { rowCommand: spec.rowCommand.bind(spec) } : {}),
        ...(spec.rowTitle ? { rowTitle: spec.rowTitle.bind(spec) } : {}),
        ...(spec.rowClass ? { rowClass: spec.rowClass.bind(spec) } : {}),
        ...(spec.tableClass ? { tableClass: spec.tableClass } : {}),
        run: (command) => this.context.run(command),
      }),
    );
  }
}

export type { PanelContext };
