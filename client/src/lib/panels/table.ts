/**
 * Declarative table panels.
 *
 * Most panels in this terminal are the same shape: fetch a feed, print a summary
 * line, then a ranked table whose rows run a command when clicked. Written out
 * longhand that is about 120 lines each, and it was — eighteen times, which is
 * how the same field came to be rendered two different ways in two panels whose
 * own header comments say they read alike.
 *
 * A panel declares its columns instead. The spec carries everything that
 * actually varied between those eighteen: which columns appear at all (some only
 * when the data has them), how each cell is formatted and toned, what a row does
 * when clicked, and what to show when the feed is empty.
 *
 * What is deliberately *not* here: grouped sections (`SRCH` nests a table per
 * event), ladders with synthetic rows (`OB` inserts a spread row between the two
 * sides), and the chart panels. Those are genuinely different renderings, and
 * bending this spec around them would cost more than the duplication saves.
 */

import type { Snippet } from 'svelte';

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
 * `cell` returns the text for the common case. A cell with structure — two
 * stacked lines, an image, its own click target — supplies `render` instead,
 * which is the escape hatch that keeps bespoke markup out of the spec.
 */
export interface Column<R> {
  /** Header text. Empty for an unlabelled column, e.g. artwork or a row action. */
  header: string;
  cell?(row: R, index: number): string;
  /** Structured cell content, when text is not enough. */
  render?: Snippet<[R, number]>;
  /** Class on the `<td>`; a function when it depends on the value's sign. */
  class?: string | ((row: R) => string);
  /** Tooltip on the `<td>` — usually the untruncated form of the cell text. */
  title?(row: R): string | undefined;
  /**
   * Include this column only when the predicate passes over the whole row set.
   *
   * Netflix publishes view counts globally but not per country, and a column of
   * em-dashes is worse than no column. Evaluated once per render, so the header
   * and the cells can never disagree about whether it is there.
   */
  when?(rows: readonly R[]): boolean;
}

/** What to show instead of the table when there are no rows. */
export interface EmptyState {
  message: string;
  hint?: string;
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

/** Normalise a note into segments, so one template renders both forms. */
export function noteSegments(note: Note): NoteSegment[] {
  if (note === null) return [];
  if (typeof note === 'string') return note === '' ? [] : [{ text: note }];
  return note;
}

/** The columns that survive their `when` predicate for this row set. */
export function activeColumns<R>(columns: readonly Column<R>[], rows: readonly R[]): Column<R>[] {
  return columns.filter((column) => column.when?.(rows) ?? true);
}

/** Resolve a column's `<td>` class, which may depend on the row. */
export function columnClass<R>(column: Column<R>, row: R): string | undefined {
  return typeof column.class === 'function' ? column.class(row) : column.class;
}
