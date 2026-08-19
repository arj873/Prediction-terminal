<!--
  SRCH — event search, across every venue at once.

  Search runs against *events* rather than markets, so the panel groups what it
  finds the way the venues do: a clickable header per event and its most liquid
  contracts nested underneath. That grouping is why this is not a `TablePanel` —
  the column spec describes one flat table, and this is a table per event.

  Two things the note has to say out loud. A venue that did not answer is named,
  because "no results" and "that venue is down" are different answers and only
  one of them means the market does not exist. And a catalogue snapshot that was
  truncated is named too: a partial universe must not be presented as the whole
  one, or an absent event reads as an event nobody lists.
-->
<script lang="ts">
  import type { EventSearchHit, SearchResponse, Venue } from '$gen';

  import MarketTable from './MarketTable.svelte';
  import PanelFrame from './PanelFrame.svelte';
  import PanelNote from './PanelNote.svelte';
  import type { NoteSegment } from './table';
  import { createPanelData } from './data.svelte';
  import { navigable } from '../actions/navigable';
  import { venue } from '../api/client';
  import { getRowCursor, getTerminalContext } from '../context';
  import { compact, truncate } from '../format';
  import { VENUE_IDS, formatRef, venueInfo } from '../terminal/venue';

  const { id, query, venues }: { id: string; query: string; venues: readonly Venue[] } = $props();

  const { run } = getTerminalContext();

  /** One venue's answer, or the reason it did not give one. */
  interface VenueResult {
    venue: Venue;
    response: SearchResponse | null;
    error: string | null;
  }

  const data = createPanelData({
    load: async (signal) => {
      // One venue being down should cost that venue's rows, not the search.
      const settled = await Promise.allSettled(
        venues.map((v) => venue.search(v, query, 12, signal)),
      );

      return venues.map((v, index): VenueResult => {
        const result = settled[index];
        return result?.status === 'fulfilled'
          ? { venue: v, response: result.value, error: null }
          : {
              venue: v,
              response: null,
              error: result?.reason instanceof Error ? result.reason.message : 'unavailable',
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

  /** Interleaved by score, so the best answer is at the top whoever lists it. */
  function rank(results: readonly VenueResult[]): { venue: Venue; hit: EventSearchHit }[] {
    return results
      .flatMap((result) =>
        (result.response?.hits ?? []).map((hit) => ({ venue: result.venue, hit })),
      )
      .sort((a, b) => b.hit.score - a.hit.score || (b.hit.volume24h ?? 0) - (a.hit.volume24h ?? 0));
  }

  function scannedTotal(results: readonly VenueResult[]): number {
    return results.reduce((sum, result) => sum + (result.response?.scanned ?? 0), 0);
  }

  function note(results: readonly VenueResult[]): NoteSegment[] {
    const answered = results.filter((result) => result.response !== null);
    const failures = results.filter((result) => result.error !== null);
    const truncated = answered.filter((result) => result.response!.truncated);
    const hits = answered.reduce((sum, result) => sum + result.response!.hits.length, 0);

    const counts = answered
      .map((result) => `${venueInfo(result.venue).code} ${result.response!.hits.length}`)
      .join(' · ');

    const segments: NoteSegment[] = [
      { text: `${hits} events${counts ? ` · ${counts}` : ''}` },
      { text: ` · ${scannedTotal(results).toLocaleString()} scanned`, tone: 'dim' },
    ];

    // The oldest snapshot in the set, because that is the age of the answer.
    if (answered.length > 0) {
      const age = Math.max(...answered.map((result) => result.response!.snapshotAgeSeconds));
      segments.push({ text: ` · snapshot ${age}s old`, tone: 'dim' });
    }

    if (truncated.length > 0) {
      segments.push({
        text: ` · ${truncated.map((result) => venueInfo(result.venue).code).join(', ')} catalogue truncated — this is not the whole book`,
        tone: 'down',
      });
    }

    // Named, never dropped: a venue that did not answer cannot be read as a
    // venue that lists nothing matching.
    for (const failure of failures) {
      segments.push({ text: ` · ${venueInfo(failure.venue).code} unavailable`, tone: 'down' });
    }

    return segments;
  }
</script>

<PanelFrame {id} kind="SRCH" title={`"${query}"`} {subtitle} {data}>
  {@const cursor = getRowCursor()}
  {@const results = data.data as VenueResult[]}
  {@const hits = rank(results)}

  <PanelNote note={note(results)} />

  {#if hits.length === 0}
    <div class="panel-empty">
      <div>{`Nothing open matches "${query}".`}</div>
      <div class="panel-empty-hint">
        {`Searched ${scannedTotal(results).toLocaleString()} open events across ${results.length} venues. Try fewer or broader words.`}
      </div>
    </div>
  {:else}
    {#each hits.slice(0, 24) as entry, index (index)}
      {@const eventRef = formatRef({ venue: entry.venue, id: entry.hit.event.eventTicker })}
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
          <span class="group-meta">{`${entry.hit.markets.length} contracts`}</span>
          <span class="group-meta">{`24h ${compact(entry.hit.volume24h)}`}</span>
        </div>
        <!-- The liquid few inline; the full ladder is one click away on the header. -->
        <MarketTable markets={entry.hit.markets.slice(0, 4)} />
      </div>
    {/each}
  {/if}
</PanelFrame>
