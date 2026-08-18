/**
 * Which component renders which kind of panel.
 *
 * The command registry decides *what* to open — a kind and its props — and this
 * decides what that looks like. Keeping the two apart is what lets a command
 * handler be a one-liner and a panel be a component with no knowledge of the
 * command that opened it.
 *
 * Every kind resolves to something. A kind still being ported resolves to
 * `MissingPanel`, so a half-finished registry runs rather than throwing at the
 * grid.
 */

import type { Component } from 'svelte';

import MissingPanel from './MissingPanel.svelte';
import type { PanelKind } from '../state/panels.svelte';

/* eslint-disable @typescript-eslint/no-explicit-any */
type AnyPanel = Component<any>;

export const PANEL_COMPONENTS: Record<PanelKind, AnyPanel> = {
  quote: MissingPanel,
  depth: MissingPanel,
  trades: MissingPanel,
  chart: MissingPanel,
  search: MissingPanel,
  event: MissingPanel,
  top: MissingPanel,
  watchlist: MissingPanel,
  spot: MissingPanel,
  'linked-series': MissingPanel,
  compare: MissingPanel,
  news: MissingPanel,
  fred: MissingPanel,
  'fred-search': MissingPanel,
  billboard: MissingPanel,
  'billboard-charts': MissingPanel,
  ent: MissingPanel,
  rt: MissingPanel,
  'rt-search': MissingPanel,
  netflix: MissingPanel,
  'stream-chart': MissingPanel,
  boxoffice: MissingPanel,
  steam: MissingPanel,
  tv: MissingPanel,
  help: MissingPanel,
  keys: MissingPanel,
};
