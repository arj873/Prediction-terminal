<!--
  OPT — the option chain for one expiry, both legs against a shared strike
  ladder.

  The header is the part that decides whether the Greeks beside it mean
  anything, so it says where the forward came from rather than only what it is.
  A venue forward is the number the venue's own marks are struck against; a
  parity forward was regressed out of the quotes on this very board; an assumed
  one means nothing in the market would say, and every Greek in the table below
  rests on a guess. Three different qualities of answer, and the panel refuses to
  print them identically.
-->
<script lang="ts">
  import type { OptionChain, OptionContract } from '$gen';

  import PanelFrame from './PanelFrame.svelte';
  import { createPanelData } from './data.svelte';
  import { navigable } from '../actions/navigable';
  import { options as optionsApi } from '../api/client';
  import { getRowCursor, getTerminalContext } from '../context';
  import { compact, price } from '../format';
  import { forwardTone, forwardWord, greek, rate, vol } from './options';

  const {
    id,
    symbol,
    expiry = undefined,
  }: { id: string; symbol: string; expiry?: string } = $props();

  const { run } = getTerminalContext();

  const data = createPanelData({
    load: (signal) => optionsApi.chain(symbol, expiry, signal),
    refreshMs: 15_000,
  });

  const loaded = $derived(data.data as OptionChain | undefined);

  /**
   * One row per strike, both legs together.
   *
   * A chain is read across the strike, not down one leg: the question is
   * always "what does this strike cost either way", and two separate tables
   * would make the reader do the join by eye.
   */
  interface Rung {
    strike: number;
    call?: OptionContract;
    put?: OptionContract;
  }

  const rungs = $derived.by((): Rung[] => {
    if (!loaded) return [];
    const byStrike = new Map<number, Rung>();
    for (const call of loaded.calls) {
      byStrike.set(call.strike, {
        ...(byStrike.get(call.strike) ?? { strike: call.strike }),
        call,
      });
    }
    for (const put of loaded.puts) {
      byStrike.set(put.strike, { ...(byStrike.get(put.strike) ?? { strike: put.strike }), put });
    }
    return [...byStrike.values()].sort((a, b) => a.strike - b.strike);
  });

  /** Where the forward sits in the ladder, so the table can mark the crossing. */
  const forward = $derived(loaded?.forward ?? null);

  const subtitle = $derived(
    loaded ? `${loaded.expiry.date} · ${Math.round(loaded.expiry.daysToExpiry)}d` : '',
  );
</script>

{#snippet field(label: string, value: string, tone = 'dim')}
  <div class="field">
    <span class="field-label">{label}</span>
    <span class="field-value {tone}">{value}</span>
  </div>
{/snippet}

<PanelFrame
  {id}
  kind="OPT"
  title={loaded ? `${loaded.symbol} OPTIONS` : `${symbol} OPTIONS`}
  {subtitle}
  subject={symbol}
  {data}
>
  {@const cursor = getRowCursor()}
  {@const chain = data.data as OptionChain}

  <div class="meta-strip">
    {@render field('SPOT', price(chain.spot), 'strong')}
    {@render field('FWD', price(chain.forward))}
    <!-- The field the whole table's trustworthiness hangs on. -->
    {@render field('FWD SRC', forwardWord(chain.forwardSource), forwardTone(chain.forwardSource))}
    {@render field('ATM IV', vol(chain.atmIv))}
    {@render field('RATE', rate(chain.rate))}
    {@render field('CARRY', rate(chain.carry))}
    {@render field('SIZE', `${chain.contractSize}×`)}
    {@render field('VENUE', chain.venue)}
  </div>

  {#if chain.forwardSource === 'assumed'}
    <div class="panel-note warn">
      No forward could be observed for this expiry, so the Greeks below rest on an assumed carry
      rather than on a quote.
    </div>
  {/if}

  <div class="expiry-strip">
    {#each chain.expiries as candidate (candidate.expiry)}
      <button
        class="expiry-chip"
        class:selected={candidate.expiry === chain.expiry.expiry}
        use:navigable={{ cursor }}
        onclick={() => run(`OPT ${chain.symbol} ${candidate.date}`)}
        title={`${candidate.contracts} contracts · ${compact(candidate.openInterest)} open interest`}
      >
        {candidate.date}
        <span class="chip-days">{Math.round(candidate.daysToExpiry)}d</span>
      </button>
    {/each}
  </div>

  {#if rungs.length === 0}
    <div class="panel-empty">No contracts are listed for this expiry.</div>
  {:else}
    <div class="table-wrap">
      <table class="chain">
        <thead>
          <tr>
            <th colspan="6" class="leg-head call-head">CALLS</th>
            <th class="strike-head">STRIKE</th>
            <th colspan="6" class="leg-head put-head">PUTS</th>
          </tr>
          <tr class="sub-head">
            <th>OI</th><th>VOL</th><th>BID</th><th>ASK</th><th>IV</th><th>DELTA</th>
            <th></th>
            <th>DELTA</th><th>IV</th><th>BID</th><th>ASK</th><th>VOL</th><th>OI</th>
          </tr>
        </thead>
        <tbody>
          {#each rungs as rung (rung.strike)}
            {@const crosses =
              forward !== null &&
              rungs[rungs.indexOf(rung) - 1] !== undefined &&
              rungs[rungs.indexOf(rung) - 1]!.strike < forward &&
              rung.strike >= forward}
            <tr class:at-the-money={crosses}>
              <td class="num dim">{compact(rung.call?.openInterest ?? null)}</td>
              <td class="num dim">{compact(rung.call?.volume ?? null)}</td>
              <td class="num">{price(rung.call?.bid ?? null)}</td>
              <td class="num">{price(rung.call?.ask ?? null)}</td>
              <td class="num dim">{vol(rung.call?.iv)}</td>
              <td class="num dim">{greek(rung.call?.greeks.delta, 3)}</td>
              <td class="num strike">
                <button
                  class="strike-button"
                  use:navigable={{ cursor }}
                  onclick={() => rung.call && run(`OPD ${rung.call.contract}`)}
                  title={rung.call ? `Open ${rung.call.contract}` : ''}
                >
                  {price(rung.strike)}
                </button>
              </td>
              <td class="num dim">{greek(rung.put?.greeks.delta, 3)}</td>
              <td class="num dim">{vol(rung.put?.iv)}</td>
              <td class="num">{price(rung.put?.bid ?? null)}</td>
              <td class="num">{price(rung.put?.ask ?? null)}</td>
              <td class="num dim">{compact(rung.put?.volume ?? null)}</td>
              <td class="num dim">{compact(rung.put?.openInterest ?? null)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}

  {#if chain.note}
    <div class="panel-note">{chain.note}</div>
  {/if}
</PanelFrame>

<style>
  .expiry-strip {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    padding: 4px 6px;
    border-bottom: 1px solid var(--rule);
  }

  .expiry-chip {
    display: inline-flex;
    gap: 5px;
    align-items: baseline;
    padding: 1px 6px;
    font: inherit;
    color: var(--dim);
    background: none;
    border: 1px solid var(--rule);
    cursor: pointer;
  }

  .expiry-chip:hover,
  .expiry-chip:focus-visible {
    color: var(--fg);
    border-color: var(--accent);
  }

  .expiry-chip.selected {
    color: var(--bg);
    background: var(--accent);
    border-color: var(--accent);
  }

  .chip-days {
    font-size: 0.85em;
    opacity: 0.7;
  }

  .table-wrap {
    overflow: auto;
  }

  table.chain {
    width: 100%;
    border-collapse: collapse;
  }

  .leg-head {
    padding: 2px 6px;
    color: var(--dim);
    text-align: center;
    letter-spacing: 0.08em;
  }

  .call-head {
    border-right: 1px solid var(--rule);
  }

  .put-head {
    border-left: 1px solid var(--rule);
  }

  .strike-head,
  td.strike {
    text-align: center;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }

  .sub-head th {
    padding: 1px 6px;
    font-weight: normal;
    color: var(--dim);
    text-align: right;
    border-bottom: 1px solid var(--rule);
  }

  td {
    padding: 0 6px;
    white-space: nowrap;
  }

  td.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  /* The rung the forward crosses — where the board is actually traded. */
  tr.at-the-money td {
    border-top: 1px solid var(--accent);
  }

  .strike-button {
    padding: 0;
    font: inherit;
    color: var(--fg);
    background: none;
    border: none;
    cursor: pointer;
  }

  .strike-button:hover,
  .strike-button:focus-visible {
    color: var(--accent);
    text-decoration: underline;
  }
</style>
