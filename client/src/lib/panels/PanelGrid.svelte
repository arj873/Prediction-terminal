<!--
  The workspace grid.

  Panels flow into a fixed number of columns, so the grid never has holes and
  opening one never re-sorts the ones already there. The `{#each}` is keyed by
  panel id, which is what makes re-running a command for a panel already on
  screen a prop change rather than a teardown — a chart keeps its instance, and
  its visible range, instead of being rebuilt under the reader.

  Focus follows the click, delegated here rather than bound to each tile: one
  listener, and the tile itself stays a plain non-interactive region.
-->
<script lang="ts">
  import { getTerminalContext } from '../context';
  import { PANEL_COMPONENTS } from './registry';

  const { panels } = getTerminalContext();

  /**
   * Focus follows the click, delegated to the grid.
   *
   * Attached here rather than declared on the element because it is not a
   * control: it adds no way to reach a panel that the keyboard did not already
   * have, it only moves the caret to the tile the mouse landed in.
   */
  function focusFollowsClick(node: HTMLElement) {
    const onMouseDown = (event: MouseEvent) => {
      const target = event.target as HTMLElement | null;
      const id = target?.closest<HTMLElement>('.panel')?.dataset['panelId'];
      if (id) panels.focus(id);
    };
    node.addEventListener('mousedown', onMouseDown);
    return {
      destroy: () => node.removeEventListener('mousedown', onMouseDown),
    };
  }
</script>

<div
  class="workspace"
  class:is-zoomed={panels.zoomedId !== undefined}
  data-columns={panels.columns}
  tabindex="-1"
  use:focusFollowsClick
>
  {#each panels.panels as panel (panel.id)}
    {@const Panel = PANEL_COMPONENTS[panel.kind]}
    <Panel id={panel.id} {...panel.props} />
  {:else}
    <div class="workspace-empty">
      <p>No panels open.</p>
      <p class="dim">Type <span class="mono">HELP</span> for the command list.</p>
    </div>
  {/each}
</div>
