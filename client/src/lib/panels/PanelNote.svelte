<!-- The summary line above a table: a count, a tally, a caveat about the feed. -->
<script lang="ts">
  import { noteSegments, type Note } from './table';

  const { note }: { note: Note } = $props();

  const segments = $derived(noteSegments(note));
</script>

{#if segments.length > 0}
  <div class="result-note">
    {#each segments as segment, i (i)}
      <span class={segment.tone}>{segment.text}</span>
    {/each}
  </div>
{/if}
