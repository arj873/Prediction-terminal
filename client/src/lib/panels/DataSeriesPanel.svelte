<!--
  ECO — a published series at any of the eight publishers that chart, plus the
  metadata that makes it readable.

  A number without its units is noise, so the panel leads with units, frequency,
  seasonal adjustment and vintage, and names which publisher and which of its
  arms answered. The terminal never passes a scraped figure off as an official
  one, which is what the SOURCE field is for, and it never passes one
  publisher's figure off as another's, which is what PROVIDER is for.

  `FRED <id>` opens this panel too. It predates the other eleven publishers and
  every habit, example and README line in this terminal says it, so it keeps
  working and reaches the same series through the same route.
-->
<script lang="ts">
  import type { DataObservation, DataSeriesResponse } from '$gen';
  import type { MouseEventParams, Time } from 'lightweight-charts';

  import PanelFrame from './PanelFrame.svelte';
  import { createPanelData } from './data.svelte';
  import { navigable } from '../actions/navigable';
  import { data as dataApi } from '../api/client';
  import Chart from '../chart/Chart.svelte';
  import { toFredData } from '../chart/data';
  import { getRowCursor } from '../context';
  import { day, direction, metric, truncate } from '../format';
  import { dataSourceInfo, formatDataRef, type DataRef } from '../terminal/dataset';

  const {
    id,
    reference,
    start = undefined,
    end = undefined,
  }: { id: string; reference: DataRef; start?: string; end?: string } = $props();

  const printed = $derived(formatDataRef(reference));

  const data = createPanelData({
    // Published data is revised on a release schedule, never intraday.
    load: (signal) => dataApi.series(printed, start, end, signal),
    refreshMs: 15 * 60_000,
  });

  const loaded = $derived(data.data);
  const subtitle = $derived(loaded ? truncate(loaded.series.title, 70) : '');

  /**
   * The last two *published* points. Every publisher here has a missing-value
   * marker — FRED's `.`, the BLS's `-`, an omitted SDMX observation — and each
   * one reaches the client as `null`, which is a hole and not a zero.
   */
  const withValues = $derived(
    loaded ? loaded.observations.filter((o: DataObservation) => o.value !== null) : [],
  );
  const last = $derived(withValues.at(-1));
  const previous = $derived(withValues.at(-2));
  const change = $derived(last && previous ? last.value! - previous.value! : null);

  /**
   * What the crosshair is over, or `null` when it is off the chart — in which
   * case the legend falls back to the last published observation, so the panel
   * reads the same before it has been touched as after the pointer leaves.
   */
  let hovered = $state<{ date: string | null; value: number | null } | null>(null);

  const legend = $derived(hovered ?? { date: last?.date ?? null, value: last?.value ?? null });

  function onCrosshair(param: MouseEventParams<Time>): void {
    if (!param.time) {
      hovered = null;
      return;
    }
    const point = loaded?.observations.find((o: DataObservation) => o.date === String(param.time));
    hovered = { date: point?.date ?? null, value: point?.value ?? null };
  }
</script>

{#snippet field(label: string, value: string)}
  <div class="field">
    <span class="field-label">{label}</span>
    <span class="field-value">{value}</span>
  </div>
{/snippet}

<PanelFrame {id} kind="ECO" title={printed} {subtitle} subject={printed} {data}>
  {@const cursor = getRowCursor()}
  {@const payload = data.data as DataSeriesResponse}
  {@const series = payload.series}

  <div class="headline">
    <span class="headline-value">{metric(last?.value ?? null)}</span>
    <span class="headline-unit">{series.unitsShort || series.units || ''}</span>
    <span class="headline-change {direction(change)}"
      >{change === null
        ? ''
        : `${change > 0 ? '+' : ''}${metric(change)} vs ${day(previous?.date ?? null)}`}</span
    >
    <span class="headline-date">{last ? day(last.date) : ''}</span>
  </div>

  <div class="meta-strip">
    {@render field('UNITS', series.units || '—')}
    {@render field('FREQ', series.frequency || '—')}
    {@render field('ADJ', series.seasonalAdjustment || '—')}
    {@render field('RANGE', `${day(series.observationStart)} → ${day(series.observationEnd)}`)}
    {@render field('UPDATED', series.lastUpdated || '—')}
    <!-- Which publisher, and which of its arms answered. Never left implicit:
         a scraped figure and an official one are not the same evidence. -->
    {@render field('PROVIDER', dataSourceInfo(series.provider).label)}
    {@render field('SOURCE', series.source || '—')}
  </div>

  <div class="chart-wrap">
    <div class="chart-legend">
      <span class="legend-time">{legend.date ? day(legend.date) : '—'}</span>
      <span class="legend-pair">
        <span class="legend-key">VAL</span>
        <span>{metric(legend.value)}</span>
      </span>
    </div>
    {#if payload.observations.length === 0}
      <div class="chart-host">
        <div class="panel-empty">No observations returned.</div>
      </div>
    {:else}
      <!--
        `fitKey` is what is being charted, not its data: a poll must not snap a
        reader's zoom back to the full window, but a different series or a
        different window is a different picture and gets refitted.
      -->
      <Chart
        series={[{ id: 'eco', kind: 'fred-area', data: toFredData(payload.observations) }]}
        fitKey={`${printed}:${start ?? ''}:${end ?? ''}`}
        {onCrosshair}
      />
    {/if}
  </div>

  {#if series.notes}
    <details class="notes">
      <!-- Navigable so the keyboard can open it: the mouse can, so the caret must. -->
      <summary use:navigable={{ cursor }}>NOTES</summary>
      <p>{series.notes}</p>
    </details>
  {/if}
</PanelFrame>
