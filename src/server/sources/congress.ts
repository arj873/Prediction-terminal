/**
 * Congress.gov — the Library of Congress's legislative API.
 *
 * Kalshi and both Polymarkets list "will bill X pass", "will the government
 * shut down", "who controls the House". Every one of those settles on an action
 * recorded here: a bill's introduction, its committee referral, a floor vote,
 * the President's signature. This is the primary record those markets resolve
 * against.
 *
 * The API needs a key, but degrades rather than failing: `DEMO_KEY` is
 * api.data.gov's shared credential and answers real data at a low per-IP rate.
 * A free key from api.congress.gov raises the limit to 5,000 requests an hour.
 * The terminal says which one it used, because a rate-limit error on a shared
 * key is an operator problem with a five-minute fix, not an outage.
 */

import type {
  Bill,
  BillAction,
  BillDetail,
  BillSearchResponse,
} from '../../shared/types.js';
import { TTL, cache } from '../lib/cache.js';
import { UpstreamError, fetchJson } from '../lib/http.js';

const BASE = process.env.CONGRESS_API_BASE ?? 'https://api.congress.gov/v3';

/** api.data.gov's shared demonstration credential. Rate-limited, but real. */
const DEMO_KEY = 'DEMO_KEY';

const apiKey = (): string => process.env.CONGRESS_API_KEY?.trim() || DEMO_KEY;

export function usingDemoKey(): boolean {
  return apiKey() === DEMO_KEY;
}

/** The bill types Congress.gov recognises, lower-case as its URLs want them. */
export const BILL_TYPES = [
  'hr',
  's',
  'hjres',
  'sjres',
  'hconres',
  'sconres',
  'hres',
  'sres',
] as const;

export type BillType = (typeof BILL_TYPES)[number];

export function assertBillType(raw: string): BillType {
  const type = raw.trim().toLowerCase().replace(/[.\s]/g, '');
  if ((BILL_TYPES as readonly string[]).includes(type)) return type as BillType;
  throw new UpstreamError(`"${raw}" is not a bill type`, {
    code: 'bad_request',
    hint: `Types are ${BILL_TYPES.join(', ')} — e.g. \`CONG BILL 119 hr 1\`.`,
  });
}

/**
 * The Congress sitting on a given date.
 *
 * The 1st Congress convened in 1789 and each runs two years from an odd-numbered
 * year, so this is arithmetic rather than a table — and it stays right in 2027
 * without anyone remembering to update it.
 */
export function currentCongress(now = new Date()): number {
  const year = now.getUTCFullYear();
  // A Congress begins on 3 January of the odd year; before then, the previous
  // one is still sitting.
  const effective = now.getUTCMonth() === 0 && now.getUTCDate() < 3 ? year - 1 : year;
  return Math.floor((effective - 1789) / 2) + 1;
}

/* ------------------------------------------------------------------ fetch */

async function get<T>(path: string, params: Record<string, string | number | undefined>): Promise<T> {
  const url = new URL(`${BASE}${path}`);
  url.searchParams.set('format', 'json');
  url.searchParams.set('api_key', apiKey());
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== '') url.searchParams.set(key, String(value));
  }

  // The key is part of the URL but must not be part of the cache key, or two
  // deployments of the same terminal would never share an entry — and the key
  // would sit in a log line the moment anyone prints the cache.
  const cacheKey = `congress:${url.pathname}?${new URLSearchParams(
    [...url.searchParams].filter(([k]) => k !== 'api_key'),
  )}`;

  return cache.cached(cacheKey, TTL.congress, async () => {
    try {
      return await fetchJson<T>(url.toString(), { timeoutMs: 25_000, retries: 1 });
    } catch (err) {
      throw annotate(err);
    }
  });
}

/** Turn the shared-key rate limit into the one sentence that fixes it. */
function annotate(err: unknown): unknown {
  if (!(err instanceof UpstreamError)) return err;
  if (err.status !== 429 || !usingDemoKey()) return err;
  return new UpstreamError('Congress.gov is rate-limiting the shared demonstration key', {
    code: 'rate_limited',
    status: 429,
    hint:
      'This terminal is using api.data.gov\'s DEMO_KEY, which is throttled per IP. ' +
      'A free key from https://api.congress.gov/sign-up/ raises the limit to 5,000 ' +
      'requests an hour — set CONGRESS_API_KEY.',
  });
}

/* ----------------------------------------------------------------- shapes */

interface ApiAction {
  actionDate?: string;
  text?: string;
  chamber?: string;
  type?: string;
}

interface ApiBill {
  congress?: number;
  type?: string;
  number?: string | number;
  title?: string;
  originChamber?: string;
  introducedDate?: string;
  latestAction?: ApiAction;
  policyArea?: { name?: string };
  sponsors?: { fullName?: string; party?: string; state?: string }[];
  cosponsors?: { count?: number };
  laws?: { number?: string; type?: string }[];
  url?: string;
}

function actionOf(action: ApiAction | undefined): BillAction | null {
  if (!action) return null;
  return {
    date: (action.actionDate ?? '').slice(0, 10),
    text: (action.text ?? '').replace(/\s+/g, ' ').trim(),
    chamber: action.chamber ?? '',
  };
}

/** `hr` + `1` + `119` → the citation a reader would say out loud. */
export function billLabel(type: string, number: string | number, congress: number): string {
  const ordinal = ((n: number): string => {
    const rem100 = n % 100;
    if (rem100 >= 11 && rem100 <= 13) return `${n}th`;
    return `${n}${['th', 'st', 'nd', 'rd'][n % 10] ?? 'th'}`;
  })(congress);
  return `${type.toUpperCase()} ${number} (${ordinal})`;
}

function toBill(raw: ApiBill): Bill {
  const congress = raw.congress ?? 0;
  const type = (raw.type ?? '').toLowerCase();
  const number = String(raw.number ?? '');
  const sponsor = raw.sponsors?.[0];
  const latest = actionOf(raw.latestAction);

  return {
    congress,
    type,
    number,
    title: (raw.title ?? '').replace(/\s+/g, ' ').trim(),
    label: billLabel(type, number, congress),
    originChamber: raw.originChamber ?? '',
    introducedDate: (raw.introducedDate ?? '').slice(0, 10),
    latestAction: latest,
    sponsor: sponsor?.fullName ?? '',
    sponsorParty: sponsor?.party ?? '',
    sponsorState: sponsor?.state ?? '',
    cosponsors: raw.cosponsors?.count ?? null,
    policyArea: raw.policyArea?.name ?? '',
    // `laws` is present only once a bill is enacted, which is the cleanest
    // signal available; the action text is a fallback for the gap between the
    // signing and the law number being assigned.
    becameLaw:
      (raw.laws?.length ?? 0) > 0 || /became public law|signed by president/i.test(latest?.text ?? ''),
    url: `https://www.congress.gov/bill/${congress}th-congress/${chamberSlug(type)}/${number}`,
  };
}

/** Congress.gov's web URLs spell the type out. */
function chamberSlug(type: string): string {
  const slugs: Record<string, string> = {
    hr: 'house-bill',
    s: 'senate-bill',
    hjres: 'house-joint-resolution',
    sjres: 'senate-joint-resolution',
    hconres: 'house-concurrent-resolution',
    sconres: 'senate-concurrent-resolution',
    hres: 'house-resolution',
    sres: 'senate-resolution',
  };
  return slugs[type] ?? 'house-bill';
}

/* ---------------------------------------------------------------- public */

/** Bills per request. The API's ceiling, and the unit `offset` steps by. */
const PAGE = 250;

/**
 * Search bills by words, most recently acted on first.
 *
 * Congress.gov's `/bill` collection has **no text parameter** — its own search
 * UI runs on a separate service with no public API — so this reads a window of
 * the Congress's recently-updated bills and matches titles here. The panel says
 * so rather than implying a full-text search of every bill since 1973, because
 * the difference matters: a miss means "not among the recently active bills",
 * not "no such bill".
 *
 * The window is four pages when there is a query and one when there is not.
 * A thousand bills is roughly the last few months of a Congress's activity,
 * which is the span someone searching a live market actually cares about, and
 * four sequential requests is affordable against even the shared DEMO_KEY.
 */
export async function searchBills(
  query: string,
  congress?: number,
  limit = 40,
): Promise<BillSearchResponse> {
  const target = congress ?? currentCongress();
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const pages = words.length > 0 ? 4 : 1;

  const all: Bill[] = [];
  for (let page = 0; page < pages; page++) {
    const payload = await get<{ bills?: ApiBill[] }>(`/bill/${target}`, {
      limit: PAGE,
      offset: page * PAGE,
      sort: 'updateDate+desc',
    });
    const batch = payload.bills ?? [];
    all.push(...batch.map(toBill));
    // A short page is the end of the collection; asking for the next one would
    // spend a request against the rate limit to be told the same thing.
    if (batch.length < PAGE) break;
  }

  const matched = words.length
    ? all.filter((bill) => {
        const haystack =
          `${bill.label} ${bill.type}${bill.number} ${bill.title} ${bill.sponsor} ${bill.policyArea}`.toLowerCase();
        return words.every((word) => haystack.includes(word));
      })
    : all;

  // The same bill appears on more than one page when Congress.gov re-sorts
  // between requests, which it does whenever a bill is acted on mid-scan.
  const seen = new Set<string>();
  const bills = matched.filter((bill) => {
    const key = `${bill.congress}/${bill.type}/${bill.number}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });

  return {
    query,
    congress: target,
    bills: bills.slice(0, limit),
    source: usingDemoKey() ? 'congress.gov (DEMO_KEY)' : 'congress.gov',
  };
}

/** One bill, with its summary, full action history and committees. */
export async function getBill(
  congress: number,
  rawType: string,
  number: string,
): Promise<BillDetail> {
  const type = assertBillType(rawType);
  const path = `/bill/${congress}/${type}/${encodeURIComponent(number)}`;

  const payload = await get<{ bill?: ApiBill }>(path, {});
  const raw = payload.bill;
  if (!raw) {
    throw new UpstreamError(`Congress.gov has no ${billLabel(type, number, congress)}`, {
      code: 'not_found',
      hint: 'Check the Congress number — the current one is ' + currentCongress() + '.',
    });
  }

  // Summaries, actions and committees are separate sub-resources. Any of them
  // can be legitimately absent — a bill introduced this morning has no summary
  // — so a failure on one must not cost the reader the bill itself.
  const [summaries, actions, committees] = await Promise.all([
    get<{ summaries?: { text?: string; updateDate?: string }[] }>(`${path}/summaries`, {}).catch(
      () => ({ summaries: [] }),
    ),
    get<{ actions?: ApiAction[] }>(`${path}/actions`, { limit: 250 }).catch(() => ({ actions: [] })),
    get<{ committees?: { name?: string; chamber?: string }[] }>(`${path}/committees`, {}).catch(
      () => ({ committees: [] }),
    ),
  ]);

  const newest = (summaries.summaries ?? []).at(-1);

  return {
    ...toBill({ ...raw, congress, type, number }),
    // Congress.gov files summaries as HTML. The terminal renders text, and
    // `el()` refuses raw markup by design, so the tags come off here.
    summary: stripHtml(newest?.text ?? ''),
    actions: (actions.actions ?? [])
      .map(actionOf)
      .filter((a): a is BillAction => a !== null && a.text !== ''),
    committees: (committees.committees ?? [])
      .map((c) => [c.name, c.chamber].filter(Boolean).join(' · '))
      .filter(Boolean),
  };
}

/** Congress.gov's summaries are HTML fragments; the terminal shows text. */
export function stripHtml(html: string): string {
  return html
    .replace(/<\s*br\s*\/?>/gi, '\n')
    .replace(/<\/\s*p\s*>/gi, '\n\n')
    .replace(/<[^>]+>/g, '')
    .replace(/&nbsp;/g, ' ')
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#(\d+);/g, (_, code: string) => String.fromCodePoint(Number(code)))
    .replace(/[ \t]+/g, ' ')
    .replace(/\n{3,}/g, '\n\n')
    .trim()
    .slice(0, 8000);
}
