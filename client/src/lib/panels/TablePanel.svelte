<!--
  A panel that is one table.

  Supply a loader, a title and a column spec; everything between the fetch and
  the markup is handled here. Eight panels are exactly this and nothing more.
-->
<script lang="ts" generics="T, R">
  import DataTable from './DataTable.svelte';
  import EmptyState from './EmptyState.svelte';
  import PanelFrame from './PanelFrame.svelte';
  import PanelNote from './PanelNote.svelte';
  import type { PanelData } from './data.svelte';
  import type { Column, EmptyState as Empty, Note } from './table';

  interface Props {
    id: string;
    kind: string;
    title: string;
    subtitle?: string;
    subject?: string;
    data: PanelData<T>;
    /** The rows to render, pulled out of the loaded payload. */
    rows: (data: T) => readonly R[];
    /**
     * The columns. A function when the column *set* depends on the data — the
     * cross-venue compare panel grows two columns per venue it found.
     */
    columns: readonly Column<R>[] | ((data: T) => readonly Column<R>[]);
    /** The summary line above the table. */
    note?: (data: T) => Note;
    /** Shown instead of the table when there are no rows. */
    empty?: (data: T) => Empty;
    rowCommand?: (row: R) => string | null;
    rowTitle?: (row: R) => string | undefined;
    rowClass?: (row: R) => string | undefined;
    tableClass?: string;
  }

  const {
    id,
    kind,
    title,
    subtitle = '',
    subject = undefined,
    data,
    rows,
    columns,
    note = undefined,
    empty = undefined,
    rowCommand = undefined,
    rowTitle = undefined,
    rowClass = undefined,
    tableClass = '',
  }: Props = $props();
</script>

<PanelFrame {id} {kind} {title} {subtitle} {subject} {data}>
  {@const loaded = data.data as T}
  {@const list = rows(loaded)}
  <PanelNote note={note?.(loaded) ?? null} />
  {#if list.length === 0}
    {@const state = empty?.(loaded) ?? { message: 'Nothing to show.' }}
    <EmptyState message={state.message} hint={state.hint} />
  {:else}
    <DataTable
      rows={list}
      columns={typeof columns === 'function' ? columns(loaded) : columns}
      {rowCommand}
      {rowTitle}
      {rowClass}
      {tableClass}
    />
  {/if}
</PanelFrame>
