<!--
  ENT — Kalshi's entertainment book.

  Genres are *tags*, not a partition: an Oscars market is both `film` and
  `awards`, and `ENT film` has to show it. So the response carries a count per
  genre and the panel renders the lot as chips — what is trading elsewhere is
  part of the answer to what is trading here.

  The FEED cell on a row is a live link into the data the market settles
  against. Clicking through from a market to its settlement source is the whole
  idea, so it is a second click target on the row: the row opens the ladder, the
  feed opens the data, and neither steals the other's click.
-->
<script lang="ts">
  import type { EntGenreFilter, EntResponse } from '$gen';

  import EmptyState from './EmptyState.svelte';
  import PanelFrame from './PanelFrame.svelte';
  import { createPanelData } from './data.svelte';
  import { navigable } from '../actions/navigable';
  import { ent } from '../api/client';
  import { getRowCursor, getTerminalContext } from '../context';
  import { compact, countdown, group, truncate } from '../format';

  const { id, genre }: { id: string; genre: EntGenreFilter } = $props();

  const { run } = getTerminalContext();

  const data = createPanelData({
    load: (signal) => ent.markets(genre, 60, signal),
    refreshMs: 60_000,
  });

  const GENRE_LABEL: Record<string, string> = {
    all: 'everything',
    music: 'music',
    film: 'film',
    tv: 'television',
    games: 'video games',
    awards: 'awards',
    celeb: 'celebrity',
  };

  /** Prose for a genre tag, falling back to the tag itself for a new one. */
  function label(tag: string): string {
    return GENRE_LABEL[tag] ?? tag;
  }

  const title = $derived(genre.toUpperCase());
  const subtitle = $derived(
    data.data ? `${data.data.events.length} events · ${label(data.data.genre)}` : '',
  );
</script>

<PanelFrame {id} kind="ENT" {title} {subtitle} {data}>
  {@const cursor = getRowCursor()}
  {@const book = data.data as EntResponse}

  <!--
    Genre chips double as the navigation: the counts say what is worth opening,
    and clicking one re-runs `ENT <genre>`. No `command` on the registration —
    these are the panel's own next steps, not a row about a market, and `$` must
    not resolve to the word "film".
  -->
  <div class="result-note">
    <span class="dim">GENRES</span>
    {#each Object.entries(book.counts) as [tag, count] (tag)}
      {#if count}
        <button
          type="button"
          class="ent-chip"
          class:ent-chip-active={tag === book.genre}
          title={`Show ${label(tag)} markets`}
          onclick={() => run(`ENT ${tag}`)}
          use:navigable={{ cursor }}>{tag.toUpperCase()} {group(count)}</button
        >
      {/if}
    {/each}
  </div>

  {#if book.events.length === 0}
    <EmptyState
      message={`Nothing open in ${label(book.genre)}.`}
      hint={`Scanned ${group(book.scanned)} open events, snapshot ${book.snapshotAgeSeconds}s old.`}
    />
  {:else}
    <!--
      Written out rather than declared through the shared column spec: the rows
      carry a second action, which a column of cells has no way to hand to the
      row's own registration.
    -->
    <table class="data-table ent-table">
      <thead>
        <tr>
          <th>EVENT</th>
          <th>MARKET</th>
          <th>GENRE</th>
          <th>MKTS</th>
          <th>VOL 24H</th>
          <th>OI</th>
          <th>CLOSES</th>
          <th>FEED</th>
        </tr>
      </thead>
      <tbody>
        {#each book.events as event (event.eventTicker)}
          {@const feed = event.feed}
          {@const command = `EVT ${event.eventTicker}`}
          <tr
            class="clickable"
            title={`${event.title}\nClick to open the ladder for ${event.eventTicker}`}
            onclick={() => run(command)}
            use:navigable={{
              cursor,
              command,
              action: feed ? () => run(feed.command) : undefined,
            }}
          >
            <td class="mono strong">{event.eventTicker}</td>
            <td title={event.title}>{truncate(event.title, 46)}</td>
            <td class="dim">{event.genres.join(' ')}</td>
            <td class="num dim">{group(event.markets.length)}</td>
            <td class="num">{compact(event.volume24h)}</td>
            <td class="num dim">{compact(event.openInterest)}</td>
            <td class="num dim">{countdown(event.closeTime)}</td>
            <td class={feed ? undefined : 'dim'}>
              {#if feed}
                <button
                  type="button"
                  class="ent-feed row-action"
                  title={`${feed.source} — run \`${feed.command}\``}
                  onclick={(clicked) => {
                    clicked.stopPropagation();
                    run(feed.command);
                  }}>{feed.command.split(' ')[0] ?? ''}</button
                >
              {:else}
                —
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</PanelFrame>
