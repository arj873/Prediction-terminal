<!--
  A placeholder for a panel kind the registry knows about but has no component
  for yet. Only reachable during the port; it exists so a half-built registry
  still runs rather than throwing at the grid.
-->
<script lang="ts">
  const { id, kind = 'panel' }: { id: string; kind?: string } = $props();
</script>

<section class="panel" data-panel-id={id} tabindex="-1">
  <div class="panel-header">
    <span class="panel-title">{kind}</span>
    <span class="panel-spacer"></span>
    <span class="panel-status error">TODO</span>
  </div>
  <div class="panel-body">
    <div class="panel-empty">
      <div>This panel is not implemented yet.</div>
      <div class="panel-empty-hint">Kind: {kind}</div>
    </div>
  </div>
</section>
