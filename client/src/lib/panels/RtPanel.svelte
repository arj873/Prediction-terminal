<!--
  RT — the Rotten Tomatoes scores Kalshi's KXRT series settles against.

  The two meters are the panel: 92 against 41 has to read before the digits do.
  What they must never do is round a missing score down to zero — an unrated
  title is precisely when the ladder has something to price, and a bar at 0%
  would say the reviews came in and they were catastrophic.
-->
<script lang="ts">
  import type { RtScore, RtTitle } from '$gen';

  import PanelFrame from './PanelFrame.svelte';
  import { createPanelData } from './data.svelte';
  import { navigable } from '../actions/navigable';
  import { ent } from '../api/client';
  import { getRowCursor, getTerminalContext } from '../context';
  import { group, truncate } from '../format';

  const { id, query }: { id: string; query: string } = $props();

  const { run } = getTerminalContext();

  const data = createPanelData({
    // Scores move as reviews land, but not minute to minute.
    load: (signal) => ent.rt(query, signal),
    refreshMs: 10 * 60_000,
  });

  const loaded = $derived(data.data);
  const title = $derived(loaded ? truncate(loaded.title, 40) : query.toUpperCase());
  const subtitle = $derived(
    loaded ? [loaded.year, loaded.mediaType].filter(Boolean).join(' · ') : '',
  );

  /**
   * The obvious next commands, one click away — the quote panel's footer idiom.
   * `SRCH` finds the Kalshi ladder trading on this very score.
   */
  const actions = $derived(
    loaded
      ? [
          { label: 'SRCH', command: `SRCH ${loaded.title}` },
          { label: 'SEARCH RT', command: `RT SEARCH ${query}` },
          { label: 'ENT FILM', command: 'ENT film' },
        ]
      : [],
  );
</script>

{#snippet field(label: string, value: string)}
  <div class="field">
    <span class="field-label">{label}</span>
    <span class="field-value">{value}</span>
  </div>
{/snippet}

{#snippet meter(score: RtScore, label: string)}
  {@const value = score.score}
  {@const tone = value === null ? 'flat' : value >= 60 ? 'up' : 'down'}
  <div class="rt-meter">
    <div class="rt-meter-head">
      <span class="rt-meter-label">{label}</span>
      <span class="rt-meter-score {tone}">{value === null ? '--' : `${value}%`}</span>
    </div>
    <div class="rt-meter-track">
      <!--
        Width is the only thing set imperatively; everything else is a class.
        An unrated score draws no bar, and so does a zero — which is why the
        numeral above it, `--` or `0%`, is the authority and not the bar.
      -->
      <div class="rt-meter-fill {tone}" style:width={`${value ?? 0}%`}></div>
    </div>
    <div class="rt-meter-foot">
      <span class="dim">{score.state}</span>
      <span class="dim">{score.reviewCount ? `${group(score.reviewCount)} reviews` : ''}</span>
    </div>
  </div>
{/snippet}

<PanelFrame {id} kind="RT" {title} {subtitle} {data}>
  {@const cursor = getRowCursor()}
  {@const rt = data.data as RtTitle}

  <div class="rt-meters">
    {@render meter(rt.critics, 'TOMATOMETER')}
    {@render meter(rt.audience, 'POPCORNMETER')}
  </div>

  {#if rt.critics.score === null}
    <!--
      A title with no Tomatometer yet is the interesting case, not an error:
      that is precisely when the Kalshi ladder has something to price.
    -->
    <div class="result-note dim">No Tomatometer yet — the score has not been issued.</div>
  {/if}

  <div class="meta-strip">
    {#if rt.year}{@render field('YEAR', rt.year)}{/if}
    {@render field('TYPE', rt.mediaType)}
    {#if rt.critics.averageRating}
      {@render field('AVG CRITIC', `${rt.critics.averageRating}/10`)}
    {/if}
    {#if rt.audience.averageRating}
      {@render field('AVG AUDIENCE', `${rt.audience.averageRating}/5`)}
    {/if}
    {@render field('SLUG', rt.slug)}
  </div>

  {#if rt.synopsis}
    <details class="notes">
      <!-- Navigable so the keyboard can open it: the mouse can, so the caret must. -->
      <summary use:navigable={{ cursor }}>SYNOPSIS</summary>
      <p>{rt.synopsis}</p>
    </details>
  {/if}

  <div class="panel-actions">
    {#each actions as action (action.label)}
      <!--
        No `command` on the registration: these are the panel's own next steps,
        not a row about a market, and `$` must not resolve to a word of a film
        title.
      -->
      <button
        class="action"
        type="button"
        onclick={() => run(action.command)}
        use:navigable={{ cursor }}>{action.label}</button
      >
    {/each}
  </div>
</PanelFrame>
