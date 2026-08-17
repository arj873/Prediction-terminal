/**
 * A CSV reader for the four upstreams here that answer in CSV.
 *
 * FRED's fredgraph export gets away with `indexOf(',')` because it has exactly
 * two columns and neither is quoted. Nothing else does. The Federal Reserve
 * writes series descriptions with commas *inside* the quotes — "Market yield on
 * U.S. Treasury securities at 1-month constant maturity, quoted on investment
 * basis" — and SDMX writes free-text titles the same way. Splitting those on
 * commas shifts every subsequent column by one, which does not fail loudly: it
 * silently pairs a date with the wrong series' value.
 *
 * So: a real reader. Quoted fields, doubled quotes as an escape, and embedded
 * newlines, which SDMX titles genuinely contain.
 */

/** Parse a CSV document into rows of raw cell strings. */
export function parseCsv(text: string, maxRows = 200_000): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let field = '';
  let quoted = false;
  let sawContent = false;

  const endField = (): void => {
    row.push(field);
    field = '';
  };

  const endRow = (): void => {
    endField();
    // A trailing newline yields one empty cell, which is not a row.
    if (row.length > 1 || row[0] !== '') rows.push(row);
    row = [];
    sawContent = false;
  };

  for (let i = 0; i < text.length; i++) {
    const ch = text[i]!;

    if (quoted) {
      if (ch !== '"') {
        field += ch;
        continue;
      }
      // `""` inside a quoted field is one literal quote.
      if (text[i + 1] === '"') {
        field += '"';
        i++;
        continue;
      }
      quoted = false;
      continue;
    }

    if (ch === '"' && !sawContent) {
      quoted = true;
      sawContent = true;
      continue;
    }
    if (ch === ',') {
      endField();
      sawContent = false;
      continue;
    }
    if (ch === '\r') continue;
    if (ch === '\n') {
      endRow();
      if (rows.length >= maxRows) return rows;
      continue;
    }

    field += ch;
    sawContent = true;
  }

  if (field !== '' || row.length > 0) endRow();
  return rows;
}

/**
 * A header row as a name → column-index map.
 *
 * Names are matched case-insensitively and with surrounding whitespace and a
 * BOM stripped — the Federal Reserve ships `"Unique Identifier: "` with a
 * trailing space, and more than one of these hosts prefixes a UTF-8 BOM.
 * The *first* occurrence wins: OECD's labelled CSV repeats each dimension as a
 * code column then a label column, and the code is the one worth indexing.
 */
export function headerIndex(header: readonly string[]): Map<string, number> {
  const index = new Map<string, number>();
  for (let i = 0; i < header.length; i++) {
    const key = normaliseHeader(header[i] ?? '');
    if (key && !index.has(key)) index.set(key, i);
  }
  return index;
}

export function normaliseHeader(name: string): string {
  return name.replace(/^﻿/, '').trim().toLowerCase();
}

/** Read a cell by header name, or `''` when the column is absent. */
export function cellAt(
  row: readonly string[],
  index: ReadonlyMap<string, number>,
  name: string,
): string {
  const at = index.get(normaliseHeader(name));
  return at === undefined ? '' : (row[at] ?? '').trim();
}

/**
 * The first of `names` that this document actually has a column for.
 *
 * Three SDMX agencies spell the same concept three ways — `TITLE`,
 * `Series title`, `Measure` — and a caller wants whichever is present rather
 * than a chain of ternaries at every field.
 */
export function firstCell(
  row: readonly string[],
  index: ReadonlyMap<string, number>,
  names: readonly string[],
): string {
  for (const name of names) {
    const value = cellAt(row, index, name);
    if (value) return value;
  }
  return '';
}

/**
 * Read a numeric cell.
 *
 * `null` rather than `0` for anything unparseable: every one of these
 * publishers uses an empty cell for "not reported", and a zero would assert
 * that the reading was taken and came out at nought.
 */
export function numberCell(raw: string | undefined): number | null {
  if (raw === undefined) return null;
  const cleaned = raw.trim().replace(/^"|"$/g, '').replace(/,/g, '');
  if (cleaned === '' || cleaned === '.' || cleaned === 'NA' || cleaned === 'ND' || cleaned === 'NC') {
    return null;
  }
  const value = Number(cleaned);
  return Number.isFinite(value) ? value : null;
}
