<!--
  The ticker tape.

  Decoration with a job: it shows what is busy right now across all three
  brokers, and clicking an item charts it. Two details are load-bearing.

  The three venues are interleaved round-robin rather than concatenated,
  because Kalshi's volumes dwarf the other two and a straight sort would make
  the tape a Kalshi-only feed.

  The item list is rendered twice, the second copy hidden from screen readers.
  That is what lets the CSS marquee translate by exactly -50% and loop with no
  visible seam; one copy would snap back.

  Failures are swallowed. A tape that cannot load is a tape that is not there,
  not an error someone has to dismiss.
-->
<script lang="ts">
  import { onDestroy } from 'svelte';

  import { venue as venueApi } from '../api/client';
  import { getTerminalContext } from '../context';
  import { cents, direction, signedCents } from '../format';
  import { formatRef, VENUES } from '../terminal/venue';

  /** How often the tape re-reads the leaderboards. */
  const REFRESH_MS = 45_000;
  /** Per venue, before interleaving. */
  const PER_VENUE = 12;
  /** Total shown. More than this and the loop gets too long to watch. */
  const MAX_ITEMS = 18;

  interface TapeItem {
    ref: string;
    code: string;
    title: string;
    price: number | null;
    change: number | null;
  }

  const { run } = getTerminalContext();

  let items = $state<TapeItem[]>([]);
  let controller: AbortController | undefined;

  async function load() {
    controller?.abort();
    const own = new AbortController();
    controller = own;

    const boards = await Promise.allSettled(
      VENUES.map((info) => venueApi.top(info.id, 'volume', PER_VENUE, own.signal)),
    );
    if (own.signal.aborted) return;

    const lanes = boards.map((board, index) =>
      board.status === 'fulfilled'
        ? board.value.markets.map((market) => ({
            ref: formatRef({ venue: VENUES[index]!.id, id: market.ticker }),
            code: VENUES[index]!.code,
            title: market.title,
            price: market.mid ?? market.lastPrice,
            change: market.change,
          }))
        : [],
    );

    // Round-robin, so no venue can monopolise the tape.
    const merged: TapeItem[] = [];
    for (let i = 0; merged.length < MAX_ITEMS; i++) {
      const before = merged.length;
      for (const lane of lanes) {
        if (merged.length >= MAX_ITEMS) break;
        const entry = lane[i];
        if (entry) merged.push(entry);
      }
      if (merged.length === before) break;
    }

    items = merged;
  }

  void load();
  const timer = setInterval(() => void load(), REFRESH_MS);

  onDestroy(() => {
    clearInterval(timer);
    controller?.abort();
  });
</script>

{#if items.length > 0}
  <div class="tape">
    <div class="tape-track">
      {#each [false, true] as clone (clone)}
        <div class="tape-run" aria-hidden={clone ? 'true' : undefined}>
          {#each items as item, i (`${clone}-${i}-${item.ref}`)}
            <button
              type="button"
              class="tape-item"
              title={item.title}
              tabindex={clone ? -1 : 0}
              onclick={() => run(`GP ${item.ref}`)}
            >
              <span class="tape-venue dim">{item.code}</span>
              <span class="tape-ref mono">{item.ref}</span>
              <span class="tape-price num">{cents(item.price)}</span>
              <span class="tape-change num {direction(item.change)}"
                >{signedCents(item.change)}</span
              >
            </button>
          {/each}
        </div>
      {/each}
    </div>
  </div>
{/if}
