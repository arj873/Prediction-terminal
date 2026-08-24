<!--
  W — the watchlist.

  The list itself lives in the workspace, so it survives a reload; this panel
  only quotes it. Every row has a second action, the `×` that drops the entry,
  declared on the row's registration so `Alt+Backspace` (or `d` in NAV mode)
  reaches it as well as the mouse.

  Written out rather than built from `TablePanel`, because that second action is
  a property of the row and the column spec deliberately describes cells only.
-->
<script lang="ts">
  import type { Market } from '$gen';

  import EmptyState from './EmptyState.svelte';
  import PanelFrame from './PanelFrame.svelte';
  import { createPanelData } from './data.svelte';
  import { navigable } from '../actions/navigable';
  import { venue } from '../api/client';
  import { getRowCursor, getTerminalContext } from '../context';
  import { cents, compact, countdown, direction, signedCents, truncate } from '../format';
  import { formatRef, parseRef, venueInfo } from '../terminal/venue';

  const { id }: { id: string } = $props();

  const { run, workspace } = getTerminalContext();

  const data = createPanelData({
    load: async (signal) => {
      const entries = workspace.watchlist;
      if (entries.length === 0) return [] as Market[];

      // One failing entry (settled, delisted, mistyped) must not blank the list.
      const settled = await Promise.allSettled(
        entries.map((entry) => venue.market(parseRef(entry), signal)),
      );
      return settled
        .filter((result): result is PromiseFulfilledResult<Market> => result.status === 'fulfilled')
        .map((result) => result.value);
    },
    refreshMs: 6_000,
  });

  const refOf = (market: Market): string => formatRef({ venue: market.venue, id: market.ticker });
</script>

<PanelFrame {id} kind="W" title="WATCHLIST" {data}>
  {@const cursor = getRowCursor()}
  {@const markets = data.data as Market[]}

  {#if markets.length === 0}
    <EmptyState
      message="Watchlist is empty."
      hint="Add with `W ADD <ticker>` — prefix `pm:`, `pmus:`, `gem:`, `pf:` or `fx:` for another venue."
    />
  {:else}
    <table class="data-table">
      <thead>
        <tr>
          <th>VEN</th>
          <th>TICKER</th>
          <th>CONTRACT</th>
          <th>BID</th>
          <th>ASK</th>
          <th>CHG</th>
          <th>VOL 24H</th>
          <th>CLOSES</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each markets as market, index (index)}
          {@const ref = refOf(market)}
          <tr
            class="clickable"
            title={`Click to chart ${ref}`}
            onclick={() => run(`GP ${ref}`)}
            use:navigable={{ cursor, command: `GP ${ref}`, action: () => run(`W DEL ${ref}`) }}
          >
            <td class="venue-cell" title={venueInfo(market.venue).label}>
              <span class="venue-badge venue-{market.venue}">{venueInfo(market.venue).code}</span>
            </td>
            <td class="mono strong" title={market.ticker}>{truncate(market.ticker, 26)}</td>
            <td title={market.title}>{truncate(market.yesSubTitle || market.title, 36)}</td>
            <td class="num price-bid">{cents(market.yesBid)}</td>
            <td class="num price-ask">{cents(market.yesAsk)}</td>
            <td class="num {direction(market.change)}">{signedCents(market.change)}</td>
            <td class="num dim">{compact(market.volume24h)}</td>
            <td class="num dim">{countdown(market.closeTime)}</td>
            <td class="action-cell">
              <!--
                Stops the click reaching the row: removing an entry must not also
                open a chart of the entry that was just removed.
              -->
              <button
                class="row-action"
                type="button"
                title="Remove"
                onclick={(event) => {
                  event.stopPropagation();
                  run(`W DEL ${ref}`);
                }}>×</button
              >
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</PanelFrame>
