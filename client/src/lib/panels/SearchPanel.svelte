<!--
  SRCH — event search, across every venue at once.

  Search runs against *events* rather than markets, so the panel groups what it
  finds the way the venues do: a clickable header per event and its most liquid
  contracts nested underneath. That grouping is why this is not a `TablePanel` —
  the column spec describes one flat table, and this is a table per event.

  Three things the panel has to say out loud, and `./search.ts` is where it
  decides how. A venue that did not answer is named *with its reason*, because
  "no results" and "that venue is down" are different answers and only one of
  them means the market does not exist. A search where no venue answered is
  never reported as a search that matched nothing — nothing was searched. And a
  catalogue snapshot that was truncated is named too: a partial universe must
  not be presented as the whole one, or an absent event reads as an event
  nobody lists.
-->
<script lang="ts">
  import type { Venue } from '$gen';

  import MarketTable from './MarketTable.svelte';
  import PanelFrame from './PanelFrame.svelte';
  import PanelNote from './PanelNote.svelte';
  import EmptyState from './EmptyState.svelte';
  import { emptyBody, note, partial, rank, type VenueResult } from './search';
  import { createPanelData } from './data.svelte';
  import { navigable } from '../actions/navigable';
  import { ApiRequestError, venue } from '../api/client';
  import { getRowCursor, getTerminalContext } from '../context';
  import { compact, truncate } from '../format';
  import { VENUE_IDS, formatRef, venueInfo } from '../terminal/venue';

  const { id, query, venues }: { id: string; query: string; venues: readonly Venue[] } = $props();

  const { run } = getTerminalContext();

  const data = createPanelData({
    load: async (signal) => {
      // One venue being down should cost that venue's rows, not the search.
      const settled = await Promise.allSettled(
        venues.map((v) => venue.search(v, query, 12, signal)),
      );

      return venues.map((v, index): VenueResult => {
        const result = settled[index];
        if (result?.status === 'fulfilled') {
          return { venue: v, response: result.value, error: null };
        }
        // Everything the server said, kept. `code` names the class of fault and
        // `hint` is the upstream's own sentence about what to do next; the
        // panel used to reach this point and print the word "unavailable".
        const reason: unknown = result?.reason;
        return {
          venue: v,
          response: null,
          error:
            reason instanceof ApiRequestError
              ? { message: reason.message, code: reason.code, hint: reason.hint }
              : { message: reason instanceof Error ? reason.message : 'did not answer' },
        };
      });
    },
    refreshMs: 60_000,
  });

  const subtitle = $derived(
    venues.length === VENUE_IDS.length
      ? 'all venues'
      : venues.map((v) => venueInfo(v).label).join(' · '),
  );
</script>

<PanelFrame {id} kind="SRCH" title={`"${query}"`} {subtitle} {data}>
  {@const cursor = getRowCursor()}
  {@const results = data.data as VenueResult[]}
  {@const hits = rank(results)}

  <PanelNote note={note(results)} />

  {#if hits.length === 0}
    {@const body = emptyBody(query, results)}
    {#if body.kind === 'outage'}
      <!--
        Rendered as a failure rather than as an empty result, because it is one.
        `load` settles every leg, so the frame's own error block never fires for
        this panel and the outage has to be stated here or not at all.
      -->
      <div class="panel-error">
        <div class="panel-error-title">{body.message}</div>
        {#each body.reasons as reason (reason)}
          <div class="panel-error-hint">{reason}</div>
        {/each}
        {#if body.hint}
          <div class="panel-error-hint">{body.hint}</div>
        {/if}
      </div>
    {:else}
      <EmptyState message={body.message} hint={body.hint} />
    {/if}
  {:else}
    {#each hits.slice(0, 24) as entry, index (index)}
      {@const eventRef = formatRef({ venue: entry.venue, id: entry.hit.event.eventTicker })}
      {@const short = partial(entry.hit)}
      <div class="group">
        <!--
          A row, not a button element: `.group-header` is a flex strip the width
          of the group, and a `<button>` would shrink to its content. The role
          and the key handler are what a real button would have given for free —
          the caret reaches it through `navigable` either way.
        -->
        <div
          class="group-header"
          role="button"
          tabindex="-1"
          title={`Open the full ladder for ${eventRef}`}
          onclick={() => run(`EVT ${eventRef}`)}
          onkeydown={(event) => {
            if (event.key === 'Enter' || event.key === ' ') {
              event.preventDefault();
              run(`EVT ${eventRef}`);
            }
          }}
          use:navigable={{ cursor, command: `EVT ${eventRef}` }}
        >
          <span class="venue-badge venue-{entry.venue}">{venueInfo(entry.venue).code}</span>
          <span class="group-ticker">{truncate(entry.hit.event.eventTicker, 34)}</span>
          <span class="group-title">{truncate(entry.hit.event.title, 62)}</span>
          <!-- Said, not implied: this one did not match everything asked. -->
          {#if short}
            <span class="group-meta dim" title="This event matched only part of the query">
              {short}
            </span>
          {/if}
          <span class="group-meta">{`${entry.hit.markets.length} contracts`}</span>
          <span class="group-meta">{`24h ${compact(entry.hit.volume24h)}`}</span>
        </div>
        <!-- The liquid few inline; the full ladder is one click away on the header. -->
        <MarketTable markets={entry.hit.markets.slice(0, 4)} />
      </div>
    {/each}
  {/if}
</PanelFrame>
