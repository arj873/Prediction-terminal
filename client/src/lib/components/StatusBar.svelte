<!--
  The header strip.

  Brand on the left, then the facts a reader glances up for: which input mode
  the keyboard is in, how many panels are open, which venues this build quotes,
  whether the API is answering, and the time in both zones that matter.

  The health dot is polled rather than inferred from panel failures: one panel
  erroring is an upstream problem, but the API being down is a different fact
  and should not have to be guessed from a pattern of red panels.
-->
<script lang="ts">
  import { onDestroy } from 'svelte';

  import { health } from '../api/client';
  import { getTerminalContext } from '../context';
  import { clockEt, clockUtc } from '../format';
  import type { KeyMode } from '../terminal/keys';
  import { VENUES } from '../terminal/venue';

  const HEALTH_MS = 30_000;

  const { mode }: { mode: KeyMode } = $props();
  const { panels } = getTerminalContext();

  let now = $state(new Date());
  let live = $state<boolean | undefined>(undefined);

  const clock = setInterval(() => (now = new Date()), 1_000);

  async function poll() {
    try {
      await health();
      live = true;
    } catch {
      live = false;
    }
  }

  void poll();
  const healthTimer = setInterval(() => void poll(), HEALTH_MS);

  onDestroy(() => {
    clearInterval(clock);
    clearInterval(healthTimer);
  });
</script>

<header class="topbar">
  <div class="brand">
    <span class="brand-mark">PREDICTION</span>
    <span class="brand-sub">TERMINAL</span>
  </div>

  <div class="status-strip">
    <span class="status-mode" class:is-nav={mode === 'nav'}>{mode === 'nav' ? 'NAV' : 'CMD'}</span>
    <span class="status-count dim">{panels.count} panel{panels.count === 1 ? '' : 's'}</span>
    <span class="status-venues dim">{VENUES.map((v) => v.code).join(' · ')}</span>
    <span class="status-link" class:ok={live === true} class:failed={live === false}>
      {live === undefined ? '● …' : live ? '● LIVE' : '● API DOWN'}
    </span>
    <span class="status-clock mono">{clockUtc(now)}</span>
    <span class="status-clock mono dim">{clockEt(now)}</span>
  </div>
</header>
