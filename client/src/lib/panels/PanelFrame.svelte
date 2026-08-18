<!--
  The chrome every panel wears, and the lifecycle it shares.

  A panel component supplies a title, a loader and a body; this supplies the
  tile, the header, the status stamp, the failure rendering and the row cursor.
  Keeping those here is what makes "anything clickable is navigable" free — a
  panel gets a working keyboard caret by rendering rows, without opting in.

  It also registers the panel's handle with the workspace on mount and drops it
  on destroy, which is how `REFRESH`, `ROW NEXT` and `$` reach a live panel
  without anything reading the DOM.
-->
<script lang="ts">
  import { onDestroy, onMount, type Snippet } from 'svelte';

  import { getTerminalContext, setRowCursor } from '../context';
  import { RowCursor } from './cursor.svelte';
  import type { PanelData } from './data.svelte';

  interface Props {
    id: string;
    /** The short verb shown before the title, e.g. `GP`, `DES`. */
    kind: string;
    title: string;
    subtitle?: string;
    /** The panel's own answer for `$`, when it is about one market. */
    subject?: string;
    data: PanelData<unknown>;
    children: Snippet;
  }

  const { id, kind, title, subtitle = '', subject = undefined, data, children }: Props = $props();

  const { panels } = getTerminalContext();
  const cursor = new RowCursor();
  setRowCursor(cursor);

  let root = $state<HTMLElement | undefined>(undefined);
  let body = $state<HTMLElement | undefined>(undefined);

  const index = $derived(panels.indexOf(id));
  const focused = $derived(panels.focusedId === id);
  const zoomed = $derived(panels.zoomedId === id);

  $effect(() => {
    cursor.setScrollHost(body);
  });

  onMount(() => {
    panels.register(id, {
      refresh: () => data.refresh(),
      moveCursor: (step) => cursor.move(step),
      activateCursor: () => cursor.activate(),
      activateRowAction: () => cursor.activateAction(),
      cursorSubject: () => cursor.subject(),
      subject: () => subject,
      element: () => root,
    });
  });

  onDestroy(() => {
    panels.register(id, undefined);
    cursor.clear();
    data.destroy();
  });

  /** Status text: busy, stale, or the time of the last good load. */
  const status = $derived.by(() => {
    if (data.stale) return { text: `STALE · ${data.stale}`, tone: 'error' };
    if (data.loading && data.data === undefined) return { text: '···', tone: 'busy' };
    if (data.error) return { text: 'ERR', tone: 'error' };
    if (data.loading) return { text: '···', tone: 'busy' };
    return { text: data.stamp ?? '', tone: 'ok' };
  });
</script>

<section
  bind:this={root}
  class="panel"
  class:is-focused={focused}
  class:is-zoomed={zoomed}
  aria-label={`Panel ${index}: ${kind} ${title}`.trim()}
  tabindex="-1"
  data-panel-id={id}
  data-index={index}
>
  <div class="panel-header">
    <span class="panel-index">{index}</span>
    <span class="panel-title">{`${kind} ${title}`.trim()}</span>
    <span class="panel-subtitle">{subtitle}</span>
    <span class="panel-spacer"></span>
    <span class="panel-status {status.tone}">{status.text}</span>
    <button
      class="panel-close"
      type="button"
      title="Close panel"
      aria-label="Close panel"
      onclick={(event) => {
        event.stopPropagation();
        panels.close(id);
      }}>×</button
    >
  </div>

  <div bind:this={body} class="panel-body">
    {#if data.error}
      <!--
        Only reached when the panel has never loaded. Once it has, a failure
        leaves the last good render alone and shows itself in the status line.
      -->
      <div class="panel-error">
        <div class="panel-error-title">{data.error.message}</div>
        {#if data.error.hint}
          <div class="panel-error-hint">{data.error.hint}</div>
        {/if}
      </div>
    {:else if data.data === undefined}
      <div class="panel-loading">LOADING…</div>
    {:else}
      {@render children()}
    {/if}
  </div>
</section>
