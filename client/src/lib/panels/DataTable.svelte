<!--
  A table built from a column spec.

  Separate from `PanelFrame` so a panel that is *mostly* bespoke can still render
  its table this way rather than by hand — the search and compare panels both do,
  around markup this spec deliberately does not try to describe.
-->
<script lang="ts" generics="R">
  import { navigable } from '../actions/navigable';
  import { getRowCursor, getTerminalContext } from '../context';
  import { activeColumns, columnClass, type Column } from './table';

  interface Props {
    rows: readonly R[];
    columns: readonly Column<R>[];
    /** Command to run when a row is clicked. `null` leaves the row inert. */
    rowCommand?: (row: R) => string | null;
    /** Row tooltip. Worth setting whenever `rowCommand` is: it says what will happen. */
    rowTitle?: (row: R) => string | undefined;
    rowClass?: (row: R) => string | undefined;
    tableClass?: string;
  }

  const {
    rows,
    columns,
    rowCommand = undefined,
    rowTitle = undefined,
    rowClass = undefined,
    tableClass = '',
  }: Props = $props();

  const { run } = getTerminalContext();
  const cursor = getRowCursor();

  const active = $derived(activeColumns(columns, rows));
</script>

<table class="data-table {tableClass}">
  <thead>
    <tr>
      {#each active as column, i (i)}
        <th>{column.header}</th>
      {/each}
    </tr>
  </thead>
  <tbody>
    {#each rows as row, index (index)}
      {@const command = rowCommand?.(row) ?? null}
      <tr
        class={rowClass?.(row)}
        class:clickable={command !== null}
        title={rowTitle?.(row)}
        onclick={command === null ? undefined : () => run(command)}
        use:navigable={{ cursor, command: command ?? undefined, enabled: command !== null }}
      >
        {#each active as column, i (i)}
          <td class={columnClass(column, row)} title={column.title?.(row)}>
            {#if column.render}
              {@render column.render(row, index)}
            {:else}
              {column.cell?.(row, index) ?? ''}
            {/if}
          </td>
        {/each}
      </tr>
    {/each}
  </tbody>
</table>
