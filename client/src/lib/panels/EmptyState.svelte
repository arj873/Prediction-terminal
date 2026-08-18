<!--
  What a panel shows when its feed came back empty.

  Always something: a blank pane under a summary line reads as a rendering
  failure, and "no markets matched" is an answer rather than a fault.
-->
<script lang="ts">
  const { message, hint = undefined }: { message: string; hint?: string } = $props();
</script>

<div class="panel-empty">
  <div>{message}</div>
  {#if hint}
    <div class="panel-empty-hint">{hint}</div>
  {/if}
</div>
