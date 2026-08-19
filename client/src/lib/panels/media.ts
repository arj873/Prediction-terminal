/**
 * The cells the media feeds share.
 *
 * NFLX, SPOT/YT, BO, STEAM, TV and BB all read like a market monitor on purpose
 * — rank is the price, the period move is the change — because that is the frame
 * a trader already has loaded when they are looking at a market that settles on
 * one of these numbers. Reading alike is enforced rather than intended: each is
 * a list of columns over the shared table panel, and the parts they have in
 * common are written down once, here, so they cannot drift apart again.
 */

import { rankMove } from '../format';
import type { Column } from './table';

/** The move column, shared by every ranked feed here. */
export function moveColumn<R extends { move: number | null; isNew: boolean }>(): Column<R> {
  return {
    header: 'MOVE',
    cell: (entry) => rankMove(entry.move, entry.isNew).text,
    class: (entry) => `num ${rankMove(entry.move, entry.isNew).tone}`,
  };
}

/** The tone for a signed figure, where a null is neither up nor down. */
export function signedTone(value: number | null): string {
  return `num ${value === null ? 'dim' : value > 0 ? 'up' : 'down'}`;
}

/** `null` prints as an em dash, for the count columns that are often absent. */
export function countOrDash(value: number | null): string {
  return value === null ? '—' : String(value);
}
