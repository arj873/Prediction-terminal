/**
 * Reading a statistical period as a calendar date.
 *
 * A chart's x-axis is a date, but almost nothing here publishes one. The ECB
 * dates a quarter `2026-Q3`, the BLS dates a month `M07`, the Federal Reserve
 * dates a monthly average `2026-08` in the same column where its daily release
 * writes `2026-08-12`, and the EIA dates an hourly reading `2026-08-12T14`.
 * Five sources, one question, and getting it wrong is not a visible failure:
 * a period the parser does not recognise looks like a header row and silently
 * disappears from the series.
 *
 * Two rules hold everywhere:
 *
 *  - A period becomes the day it **begins**. FRED already does this — Q1 2026
 *    is `2026-01-01` there — and mixing conventions would put an OECD quarterly
 *    line three months away from the FRED series it is read against.
 *  - An unrecognised period returns `null` and drops one observation, rather
 *    than being guessed at and mis-dating the whole series.
 */

/** `YYYY-MM-DD` for the first day of the period `raw` names, or `null`. */
export function periodToDate(raw: string): string | null {
  const period = raw.trim();
  if (!period) return null;

  // 2026-08-12 — already a date.
  if (/^\d{4}-\d{2}-\d{2}$/.test(period)) return period;

  // 2026-08-12T14 — an hourly reading, dated to its day.
  const hourly = /^(\d{4}-\d{2}-\d{2})[T ]\d{2}/.exec(period);
  if (hourly) return hourly[1]!;

  // 2026-Q3 / 2026Q3 / 2026-q3
  const quarter = /^(\d{4})-?Q([1-4])$/i.exec(period);
  if (quarter) return `${quarter[1]}-${pad(1 + (Number(quarter[2]) - 1) * 3)}-01`;

  // 2026-S1 — half-year. `B` is the same thing in a few OECD flows.
  const semester = /^(\d{4})-?[SB]([1-2])$/i.exec(period);
  if (semester) return `${semester[1]}-${pad(1 + (Number(semester[2]) - 1) * 6)}-01`;

  // 2026-T2 — four-monthly (SDMX "trimester"), rare but present at the OECD.
  const trimester = /^(\d{4})-?T([1-3])$/i.exec(period);
  if (trimester) return `${trimester[1]}-${pad(1 + (Number(trimester[2]) - 1) * 4)}-01`;

  // 2026-M08 and 2026-08 — monthly.
  const month = /^(\d{4})-?M?(\d{2})$/i.exec(period);
  if (month) {
    const m = Number(month[2]);
    if (m >= 1 && m <= 12) return `${month[1]}-${pad(m)}-01`;
  }

  // 2026-W35 — the Monday of that ISO week.
  const week = /^(\d{4})-?W(\d{1,2})$/i.exec(period);
  if (week) return isoWeekStart(Number(week[1]), Number(week[2]));

  // 2026 — annual.
  if (/^\d{4}$/.test(period)) return `${period}-01-01`;

  return null;
}

function pad(n: number): string {
  return String(n).padStart(2, '0');
}

/** The Monday of ISO week `week` in `year`, as `YYYY-MM-DD`. */
export function isoWeekStart(year: number, week: number): string | null {
  if (week < 1 || week > 53) return null;
  // ISO-8601: week 1 is the week containing 4 January.
  const jan4 = new Date(Date.UTC(year, 0, 4));
  const isoDow = jan4.getUTCDay() === 0 ? 7 : jan4.getUTCDay();
  const week1Monday = Date.UTC(year, 0, 4 - (isoDow - 1));
  return new Date(week1Monday + (week - 1) * 7 * 86_400_000).toISOString().slice(0, 10);
}
