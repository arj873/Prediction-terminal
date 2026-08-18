/**
 * Reading the recurring question out of a dated slug.
 *
 * Three venues name a contract with a slug that carries its occasion:
 * `usfed-fomc-2026-10-28`, `fed-decision-in-september-762`,
 * `mlb-oak-hou-2026-08-23`. A series here is the *question* — "what will the
 * Fed do at this meeting" — so the occasion has to come off before two
 * instances of it can be recognised as one series, and before either can be
 * paired with the same question at another broker.
 *
 * Getting this wrong is expensive in both directions. Strip too little and the
 * September and October FOMC books become two one-event series, neither of
 * which lines up against Kalshi's `KXFEDDECISION`. Strip too much and every
 * congressional district collapses into one.
 */

const MONTH_SEGMENT =
  /^(jan|january|feb|february|mar|march|apr|april|may|jun|june|jul|july|aug|august|sep|sept|september|oct|october|nov|november|dec|december)$/;

const isYear = (part = ''): boolean => /^(19|20)\d{2}$/.test(part);
const isDayOrMonth = (part = ''): boolean => /^\d{1,2}$/.test(part);

/**
 * Drop the segments of a slug that name an occasion rather than a question.
 *
 * Numeric dates are removed as whole groups, never as loose numbers, because a
 * slug's other numbers carry meaning: `ushr-tx-15-2026-11-03` is the Texas 15th
 * district on 3 November 2026, and stripping every short number would file all
 * 38 Texas districts under one series. A year anchors the group — the two
 * segments after it if they are a month and day (`2026-11-03`), otherwise the
 * two before it (`03-14-2027`), otherwise nothing.
 *
 * A month spelled in words anchors its own, smaller group: the one adjacent
 * segment that is a day number. `bitcoin-up-or-down-on-august-18-2026` is a
 * question asked again tomorrow, and leaving the `18` on gives it a series of
 * its own every day. The day is only taken next to a named month, so the
 * district case above is untouched — it has no month name to anchor on.
 */
export function stripDates(slug: string): string {
  const parts = slug.split('-');
  const drop = new Set<number>();

  parts.forEach((part, i) => {
    if (MONTH_SEGMENT.test(part)) {
      drop.add(i);
      if (isDayOrMonth(parts[i + 1])) drop.add(i + 1);
      else if (isDayOrMonth(parts[i - 1])) drop.add(i - 1);
    }
    if (!isYear(part)) return;

    drop.add(i);
    if (isDayOrMonth(parts[i + 1]) && isDayOrMonth(parts[i + 2])) {
      drop.add(i + 1);
      drop.add(i + 2);
    } else if (isDayOrMonth(parts[i - 1]) && isDayOrMonth(parts[i - 2])) {
      drop.add(i - 1);
      drop.add(i - 2);
    }
  });

  return parts.filter((part, i) => part !== '' && !drop.has(i)).join('-');
}
