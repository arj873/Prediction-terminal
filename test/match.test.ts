/**
 * Cross-venue matching tests.
 *
 * This is the part of the terminal whose output looks plausible when it is
 * wrong: pairing two brokers' listings incorrectly still produces a tidy table
 * of two prices, and a trader will read the gap between them as an edge. So the
 * cases here are mostly the near misses — two questions that share almost every
 * word and are not the same question.
 *
 * Every string is one a venue actually publishes.
 */

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import {
  MATCH_FLOOR,
  confidenceOf,
  identityKey,
  normalise,
  numbersIn,
  pairLabels,
  scoreEvent,
  scoreSeries,
  significantNumbers,
  tokenise,
} from '../src/shared/match.js';

/* ------------------------------------------------------------- tokenising */

describe('normalise', () => {
  it('splits a unit welded to its number', () => {
    // Kalshi writes `25bps`, Polymarket writes `25 bps`.
    assert.equal(normalise('Cut 25bps'), 'cut 25 bp');
  });

  it('folds the phrases three venues use for one rung', () => {
    assert.equal(normalise('Fed maintains rate'), 'fed nochange');
    assert.equal(normalise('No change'), 'nochange');
    assert.equal(normalise('Unchanged'), 'nochange');
  });

  it('keeps a comparator, which is what separates two rungs', () => {
    assert.ok(normalise('Cut >25bps').includes('over'));
    assert.ok(normalise('50+ bps decrease').includes('over'));
    assert.ok(normalise('3.75% or below').includes('under'));
  });
});

describe('tokenise', () => {
  it('drops question scaffolding', () => {
    assert.deepEqual(tokenise('Will the Fed cut rates?'), ['fed', 'decrease', 'rate']);
  });

  it('folds month names to numbers so Oct and October agree', () => {
    assert.deepEqual(tokenise('Fed decision in Oct 2026?'), ['fed', 'decision', '10', '2026']);
    assert.deepEqual(tokenise('Fed Decision in October?'), ['fed', 'decision', '10']);
  });

  it('folds the venues\' synonyms onto one word', () => {
    assert.deepEqual(tokenise('FOMC'), ['fed']);
    assert.deepEqual(tokenise('BTC'), ['bitcoin']);
    assert.deepEqual(tokenise('GOP nominee'), ['republican', 'nominee']);
  });
});

describe('identityKey', () => {
  it('flattens three naming conventions onto one string', () => {
    assert.equal(identityKey('KXFEDDECISION'), 'feddecision');
    assert.equal(identityKey('fed-decision'), 'feddecision');
    assert.equal(identityKey('usfed-fomc'), 'fedfomc');
  });

  it('drops a trailing season year, which names the listing not the series', () => {
    assert.equal(identityKey('mlb-2026'), 'mlb');
    assert.equal(identityKey('nfl-2026'), 'nfl');
  });
});

describe('numbersIn / significantNumbers', () => {
  it('reads every number, years included', () => {
    assert.deepEqual(numbersIn('Fed decision in Oct 2026?'), [2026]);
    assert.deepEqual(numbersIn('Cut 25bps'), [25]);
  });

  it('drops the year when comparing series, because a series recurs', () => {
    assert.deepEqual(significantNumbers('Fed decision in Oct 2026?'), []);
    assert.deepEqual(significantNumbers('Big Brother Season 28 · 2nd place'), [28, 2]);
  });
});

/* ---------------------------------------------------------------- scoring */

describe('scoreSeries', () => {
  it('matches the same series across three naming conventions', () => {
    const kalshi = { id: 'KXNOBELPEACE', title: '2026 Nobel Peace Prize winner' };
    const poly = { id: 'nobel-peace-prize-winner-2026', title: 'Nobel Peace Prize Winner 2026' };
    const us = { id: 'nobel-peace', title: 'Nobel Peace Prize Winner' };

    assert.equal(scoreSeries(kalshi, poly).confidence, 'strong');
    assert.equal(scoreSeries(kalshi, us).confidence, 'strong');
  });

  it('ignores the expiry, so one series matches across its events', () => {
    // Both are the FOMC series; they simply have different next meetings.
    const a = { id: 'KXFEDDECISION', title: 'Fed decision in Oct 2026?' };
    const b = { id: 'usfed-fomc', title: 'Fed Decision in December' };
    assert.ok(scoreSeries(a, b).score >= MATCH_FLOOR);
  });

  it('separates two rungs that differ only by a number', () => {
    // The classic false positive: five words out of six in common.
    const second = { id: 'KXBIGBROTHERRANK', title: 'Big Brother Season 28 · 2nd place' };
    const third = { id: 'big-brother-season-28-3rd-place', title: 'Big Brother Season 28 3rd place' };
    assert.equal(scoreSeries(second, third).confidence, 'weak');
    assert.match(scoreSeries(second, third).reason, /numbers differ/);
  });

  it('does not pair two markets that share one common word', () => {
    const a = { id: 'KXNFLCHAMP', title: 'NFL Champion' };
    const b = { id: 'nfl-rookie-of-the-year', title: 'NFL Rookie of the Year' };
    assert.ok(scoreSeries(a, b).score < MATCH_FLOOR);
  });

  it('rewards identifiers that independently agree', () => {
    // Same two titles, worded differently enough that the text alone is not
    // conclusive; the identifiers are what settle it.
    const titles = { a: 'Measles cases in 2026?', b: 'How many measles cases will be reported?' };
    const bare = scoreSeries({ id: 'A', title: titles.a }, { id: 'B', title: titles.b });
    const named = scoreSeries(
      { id: 'KXMEASLES', title: titles.a },
      { id: 'measles', title: titles.b },
    );
    assert.ok(named.score > bare.score, `${named.score} should beat ${bare.score}`);
    assert.match(named.reason, /both listed as "measles"/);
  });

  it('says why, in terms a panel can print', () => {
    const match = scoreSeries(
      { id: 'SENATENC', title: 'North Carolina Senate winner?' },
      { id: 'usse-nc', title: 'North Carolina Senate Election Winner' },
    );
    assert.ok(match.shared.includes('senate'));
    assert.match(match.reason, /shared terms/);
  });
});

describe('scoreEvent', () => {
  it('keeps two expiries of one series apart', () => {
    const october = { id: 'KXFEDDECISION-26OCT', title: 'Fed decision in Oct 2026?' };
    const january = { id: 'KXFEDDECISION-27JAN', title: 'Fed decision in Jan 2027?' };
    const match = scoreEvent(october, january);
    assert.equal(match.confidence, 'weak');
    assert.match(match.reason, /numbers differ/);
  });

  it('pairs the same expiry worded two ways', () => {
    const match = scoreEvent(
      { id: 'KXFEDDECISION-26OCT', title: 'Fed decision in Oct 2026?' },
      { id: 'usfed-fomc-2026-10-28', title: 'Fed Decision in October' },
    );
    assert.equal(match.confidence, 'strong');
  });

  it('penalises a disagreeing year, which scoreSeries deliberately would not', () => {
    const a = { id: 'a', title: 'Serie A 2026 Champion' };
    const b = { id: 'b', title: 'Serie A 2027 Champion' };
    assert.ok(scoreEvent(a, b).score < scoreSeries(a, b).score);
  });

  it('applies its number penalty once, not twice', () => {
    // scoreEvent builds on the same text reading as scoreSeries; if both
    // applied their own penalty, a rung would be quartered and fall through the
    // floor even where the pairing is right.
    const match = scoreEvent(
      { id: 'a', title: 'Cut >25bps' },
      { id: 'b', title: '50+ bps decrease' },
    );
    assert.ok(match.score >= MATCH_FLOOR, `expected ≥ ${MATCH_FLOOR}, got ${match.score}`);
  });
});

describe('confidenceOf', () => {
  it('grades the bands', () => {
    assert.equal(confidenceOf(0.95), 'strong');
    assert.equal(confidenceOf(0.7), 'likely');
    assert.equal(confidenceOf(0.4), 'weak');
  });
});

/* ------------------------------------------------------------------ ladders */

describe('pairLabels', () => {
  // The real October FOMC ladder, as each venue words it.
  const kalshi = [
    { label: 'Cut >25bps' },
    { label: 'Cut 25bps' },
    { label: 'Fed maintains rate' },
    { label: 'Hike 25bps' },
    { label: 'Hike >25bps' },
  ];
  const poly = [
    { label: '50+ bps decrease' },
    { label: '25 bps decrease' },
    { label: 'No change' },
    { label: '25 bps increase' },
    { label: '50+ bps increase' },
  ];

  it('lines a five-rung ladder up across two venues', () => {
    const paired = pairLabels(kalshi, poly);
    const map = new Map(paired.map((p) => [p.left.label, p.right.label]));

    assert.equal(map.get('Cut 25bps'), '25 bps decrease');
    assert.equal(map.get('Hike 25bps'), '25 bps increase');
    assert.equal(map.get('Fed maintains rate'), 'No change');
    assert.equal(map.get('Cut >25bps'), '50+ bps decrease');
    assert.equal(map.get('Hike >25bps'), '50+ bps increase');
  });

  it('never maps two rungs of one ladder onto the same rung of the other', () => {
    const paired = pairLabels(kalshi, poly);
    const rights = paired.map((p) => p.right.label);
    assert.equal(new Set(rights).size, rights.length);
  });

  it('leaves a rung with no counterpart unpaired rather than forcing one', () => {
    const paired = pairLabels(kalshi, [{ label: 'No change' }]);
    assert.equal(paired.length, 1);
    assert.equal(paired[0]!.left.label, 'Fed maintains rate');
  });

  it('pairs nothing when the ladders share no wording', () => {
    assert.deepEqual(pairLabels(kalshi, [{ label: 'Gavin Newsom' }]), []);
  });
});
