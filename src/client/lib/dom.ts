/**
 * Minimal DOM helpers.
 *
 * The terminal renders a lot of tabular data from untrusted-ish upstream text
 * (market titles, song titles, FRED notes). Everything here sets `textContent`
 * rather than `innerHTML`, so a stray `<` in a chart title is a character, not
 * markup. No panel builds HTML from a string.
 */

type Attrs = Record<string, string | number | boolean | undefined>;

export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Attrs = {},
  children: (Node | string | null | undefined)[] = [],
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);

  for (const [key, value] of Object.entries(attrs)) {
    if (value === undefined || value === false) continue;
    if (key === 'class') node.className = String(value);
    else if (key === 'text') node.textContent = String(value);
    else if (key === 'html') throw new Error('el(): raw html is not supported by design');
    else if (value === true) node.setAttribute(key, '');
    else node.setAttribute(key, String(value));
  }

  for (const child of children) {
    if (child === null || child === undefined) continue;
    node.append(typeof child === 'string' ? document.createTextNode(child) : child);
  }

  return node;
}

/** A `<td>`/`<th>` with text, an optional class, and an optional title tooltip. */
export function cell(
  text: string,
  className?: string,
  tag: 'td' | 'th' = 'td',
  title?: string,
): HTMLTableCellElement {
  const node = document.createElement(tag);
  node.textContent = text;
  if (className) node.className = className;
  if (title) node.title = title;
  return node;
}

/** Build a `<table>` with a header row and pre-built body rows. */
export function table(headers: string[], rows: HTMLTableRowElement[], className = ''): HTMLTableElement {
  const thead = el('thead', {}, [el('tr', {}, headers.map((h) => cell(h, undefined, 'th')))]);
  const tbody = el('tbody');
  for (const row of rows) tbody.append(row);
  return el('table', { class: `data-table ${className}`.trim() }, [thead, tbody]);
}

export function row(cells: HTMLTableCellElement[], className?: string): HTMLTableRowElement {
  const tr = document.createElement('tr');
  if (className) tr.className = className;
  for (const c of cells) tr.append(c);
  return tr;
}

export function clear(node: Element): void {
  node.replaceChildren();
}

/**
 * `Node.append` that skips nullish children.
 *
 * Panels build lists with conditional entries (`cond ? el(...) : null`); this
 * keeps those readable instead of forcing a `.filter()` at every call site.
 */
export function append(parent: Node, ...children: (Node | string | null | undefined)[]): void {
  for (const child of children) {
    if (child === null || child === undefined) continue;
    parent.appendChild(typeof child === 'string' ? document.createTextNode(child) : child);
  }
}

/** A labelled key/value pair, as used across the description panels. */
export function field(label: string, value: string, valueClass?: string): HTMLElement {
  return el('div', { class: 'field' }, [
    el('span', { class: 'field-label', text: label }),
    el('span', { class: `field-value ${valueClass ?? ''}`.trim(), text: value }),
  ]);
}
