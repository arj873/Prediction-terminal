/**
 * Option routes.
 *
 * `:symbol` picks the venue the way `/api/spot/:class` picks a price feed, but
 * without making the caller state the asset class: an underlying either has a
 * Deribit board or it does not, and `OPT BTC` should not have to be spelled
 * differently from `OPT AAPL`. Crypto is checked first because the set is small
 * and closed; everything else is routed to the equity providers.
 *
 * Four views over one board — chain, surface, positioning and a single contract
 * — all derived from the same normalised quotes so they cannot disagree. See
 * `sources/optionboard.ts` for what "derived" means at each step.
 */

import { Router } from 'express';
import type { CandleInterval, OptionContract, OptionExpiry } from '../../shared/types.js';
import { UpstreamError } from '../lib/http.js';
import * as deribit from '../sources/deribit.js';
import * as equity from '../sources/equityoptions.js';
import {
  carryFor,
  chainFor,
  enrich,
  expiriesOf,
  positioningFor,
  resolveExpiry,
  surfaceFor,
  type OptionBoard,
} from '../sources/optionboard.js';
import { asyncRoute, intParam, pathParam } from './helpers.js';

export const optionsRouter: Router = Router();

/**
 * Expiries loaded when a view needs the whole term structure.
 *
 * Deribit hands over every expiry in one response, so this only bites on the
 * equity path, where Yahoo serves one expiry per request. Eight covers the part
 * of the curve anyone reads — front week through a few months — without turning
 * one panel into twenty upstream calls.
 */
const TERM_EXPIRIES = 8;

function queryString(value: unknown): string | undefined {
  if (typeof value === 'string' && value.trim() !== '') return value.trim();
  return undefined;
}

/* ------------------------------------------------------------------ loading */

interface Loaded {
  board: OptionBoard;
  expiries: OptionExpiry[];
}

/**
 * Load a board, optionally deep enough to see the whole curve.
 *
 * The two-pass shape on the equity side is deliberate: the first call is the
 * only way to learn which expiries exist, and it is cached, so the second call
 * pays for the extra expiries alone rather than re-fetching the first.
 */
async function loadBoard(symbol: string, wholeCurve: boolean): Promise<Loaded> {
  if (deribit.supports(symbol)) {
    const board = await deribit.getBoard(symbol);
    return { board, expiries: expiriesOf(board) };
  }

  const first = await equity.getBoard(symbol);
  if (!wholeCurve) return { board: first.board, expiries: expiriesOf(first.board) };

  const deep = await equity.getBoard(symbol, first.expiryDates.slice(0, TERM_EXPIRIES));
  return { board: deep.board, expiries: expiriesOf(deep.board) };
}

/* ------------------------------------------------------------------- routes */

optionsRouter.get(
  '/underlyings',
  asyncRoute(async () => ({
    underlyings: [...deribit.listUnderlyings(), ...equity.listUnderlyings()],
    note:
      'Crypto boards are the ones Deribit lists. Equity boards exist for most US ' +
      'listed names with options — the list above is a starting point, not a limit.',
  })),
);

/**
 * One contract, with its pair leg and whatever history the venue publishes.
 *
 * Routed on the shape of the identifier rather than on a class parameter: an
 * OCC symbol (`AAPL260918C00300000`) and a Deribit name (`BTC-25DEC26-104000-C`)
 * are not confusable, so a reader can paste either one straight in.
 */
optionsRouter.get(
  '/contract/:contract',
  asyncRoute(async (req) => {
    const contract = pathParam(req.params, 'contract').trim().toUpperCase();

    const cryptoSymbol = deribit.symbolOfContract(contract);
    const occ = equity.parseOcc(contract);

    if (cryptoSymbol === null && occ === null) {
      throw new UpstreamError(`"${contract}" is not a recognised option contract`, {
        code: 'bad_request',
        hint:
          'Use an OCC symbol for equities (`AAPL260918C00300000`) or a Deribit ' +
          'instrument name for crypto (`BTC-25DEC26-104000-C`). Clicking a row in ' +
          '`OPT` fills either one in for you.',
      });
    }

    const symbol = cryptoSymbol ?? occ!.symbol;
    const { board, expiries } = await loadBoard(symbol, false);

    const quote = board.quotes.find((q) => q.contract === contract);
    if (!quote) {
      throw new UpstreamError(`${contract} is not on the current ${symbol} board`, {
        code: 'not_found',
        hint:
          `The contract may have expired, or its expiry may not be loaded. ` +
          `Run \`OPT ${symbol}\` and pick the expiry from the strip.`,
      });
    }

    const expiry =
      expiries.find((e) => e.expiry === quote.expiry) ??
      resolveExpiry(expiries, new Date(quote.expiry * 1000).toISOString().slice(0, 10));

    const slice = board.quotes.filter((q) => q.expiry === quote.expiry);
    const carry = carryFor(slice, board.spot, expiry.yearsToExpiry);

    const contractData: OptionContract = enrich(quote, board.spot, carry, expiry.yearsToExpiry);
    const pairQuote = slice.find(
      (q) => q.strike === quote.strike && q.type !== quote.type,
    );

    const interval = intParam(req.query['interval'], 60, 1, 1440) as CandleInterval;
    const now = Math.floor(Date.now() / 1000);
    const defaultSpan = interval === 1 ? 6 * 3600 : interval === 60 ? 14 * 86400 : 180 * 86400;
    const end = intParam(req.query['end'], now, 0, now + 86400);
    const start = intParam(req.query['start'], end - defaultSpan, 0, end);

    // Only Deribit publishes option price history for free. Saying that plainly
    // beats an empty chart that reads as a loading failure.
    const history = cryptoSymbol
      ? await deribit
          .getHistory(contract, interval, start, end)
          .catch(() => ({ candles: [], note: 'Deribit returned no history for this contract.' }))
      : {
          candles: [],
          note:
            'No free source publishes historical prices for US listed option ' +
            'contracts, so this panel shows the live quote only. Crypto contracts ' +
            'get a chart, because Deribit publishes one.',
        };

    return {
      symbol: board.symbol,
      name: board.name,
      assetClass: board.assetClass,
      currency: board.currency,
      spot: board.spot,
      forward: carry.forward,
      forwardSource: carry.source,
      rate: carry.rate,
      carry: carry.carry,
      contractSize: board.contractSize,
      contract: contractData,
      pair: pairQuote ? enrich(pairQuote, board.spot, carry, expiry.yearsToExpiry) : null,
      history: history.candles,
      ...(history.note ? { historyNote: history.note } : {}),
      venue: board.venue,
      source: board.source,
    };
  }),
);

optionsRouter.get(
  '/:symbol/expiries',
  asyncRoute(async (req) => {
    const symbol = pathParam(req.params, 'symbol').trim().toUpperCase();
    const { board, expiries } = await loadBoard(symbol, false);
    return {
      symbol: board.symbol,
      name: board.name,
      assetClass: board.assetClass,
      spot: board.spot,
      expiries,
      venue: board.venue,
      source: board.source,
    };
  }),
);

optionsRouter.get(
  '/:symbol/chain',
  asyncRoute(async (req) => {
    const symbol = pathParam(req.params, 'symbol').trim().toUpperCase();
    const { board, expiries } = await loadBoard(symbol, false);
    const expiry = resolveExpiry(expiries, queryString(req.query['expiry']));
    return chainFor(board, expiry, expiries);
  }),
);

optionsRouter.get(
  '/:symbol/surface',
  asyncRoute(async (req) => {
    const symbol = pathParam(req.params, 'symbol').trim().toUpperCase();
    // The term structure is the point of this view, so it is worth the extra
    // expiries the chain does not need.
    const { board, expiries } = await loadBoard(symbol, true);
    const expiry = resolveExpiry(expiries, queryString(req.query['expiry']));
    return surfaceFor(board, expiry, expiries);
  }),
);

optionsRouter.get(
  '/:symbol/positioning',
  asyncRoute(async (req) => {
    const symbol = pathParam(req.params, 'symbol').trim().toUpperCase();
    const { board, expiries } = await loadBoard(symbol, false);
    const expiry = resolveExpiry(expiries, queryString(req.query['expiry']));
    return positioningFor(board, expiry);
  }),
);
