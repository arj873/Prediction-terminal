//! The matcher against six hundred titles three real exchanges actually wrote.
//!
//! The unit tests in `matching.rs` pin down individual rules on titles chosen to
//! isolate them. This does the opposite: it takes a slice of the live
//! catalogues — House districts several venues list under near-identical
//! wording, Emmy categories that differ by one qualifier, rate ladders that
//! differ by one number — and asserts the properties that have to hold across
//! all of them at once.
//!
//! The fixture is built by the server's `dump_descriptors` example and sampled
//! towards the families the matcher finds hard; see
//! `crates/core/examples/matchbench.rs` for the timing harness that reads the
//! same shape.

use terminal_core::matching::{
    could_reach_floor, prepare, score_event_prepared, score_series_prepared, Prepared,
    SeriesDescriptor, MATCH_FLOOR,
};

struct Row {
    venue: String,
    id: String,
    title: String,
    context: String,
}

fn corpus() -> Vec<Row> {
    let raw = include_str!("fixtures/series.json");
    let parsed: serde_json::Value = serde_json::from_str(raw).expect("fixture json");
    parsed
        .as_array()
        .expect("fixture is an array")
        .iter()
        .map(|row| Row {
            venue: row["venue"].as_str().unwrap_or_default().to_string(),
            id: row["id"].as_str().unwrap_or_default().to_string(),
            title: row["title"].as_str().unwrap_or_default().to_string(),
            context: row["context"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}

fn prepared(rows: &[Row]) -> Vec<Prepared> {
    rows.iter()
        .map(|r| prepare(&SeriesDescriptor::new(&r.id, &r.title).with_context(&r.context)))
        .collect()
}

/// Every cross-venue pair in the fixture, as index pairs.
fn cross_venue_pairs(rows: &[Row]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    for (i, a) in rows.iter().enumerate() {
        for (j, b) in rows.iter().enumerate().skip(i + 1) {
            if a.venue != b.venue {
                pairs.push((i, j));
            }
        }
    }
    pairs
}

/// The property the cross-venue board's prefilter rests on.
///
/// `could_reach_floor` exists so the board can decline millions of pairs
/// without scoring them, and that is only safe if declining is *exactly*
/// equivalent to scoring below the floor and discarding. Argued from the
/// arithmetic it is convincing; checked against real titles it is a fact.
#[test]
fn a_pair_the_prefilter_declines_could_never_have_cleared_the_floor() {
    let rows = corpus();
    let halves = prepared(&rows);
    let pairs = cross_venue_pairs(&rows);
    assert!(
        pairs.len() > 50_000,
        "fixture shrank: {} pairs",
        pairs.len()
    );

    let mut declined = 0usize;
    for (i, j) in &pairs {
        if could_reach_floor(&halves[*i], &halves[*j], MATCH_FLOOR) {
            continue;
        }
        declined += 1;

        // Both scorers, because the bound is stated once for both and the
        // event scorer is the one that can collect the number reward twice.
        let series = score_series_prepared(&halves[*i], &halves[*j]);
        let event = score_event_prepared(&halves[*i], &halves[*j]);
        assert!(
            series.score < MATCH_FLOOR && event.score < MATCH_FLOOR,
            "{} vs {} was declined but scores series={} event={}",
            rows[*i].id,
            rows[*j].id,
            series.score,
            event.score,
        );
    }

    // A filter that declines nothing is admissible and useless; this is the
    // half of the claim the assertions above cannot make.
    assert!(
        declined * 2 > pairs.len(),
        "prefilter declined only {declined} of {} pairs — it is not pulling its weight",
        pairs.len()
    );
}

/// Preparing a listing must not depend on which listing it is compared with.
#[test]
fn a_prepared_half_scores_the_same_as_the_descriptor_it_came_from() {
    use terminal_core::matching::{score_event, score_series};

    let rows = corpus();
    let halves = prepared(&rows);

    // Every 37th pair: enough to catch a divergence, few enough that the slow
    // path — which re-derives both halves per call — stays a test and not a wait.
    for (i, j) in cross_venue_pairs(&rows).into_iter().step_by(37) {
        let da = SeriesDescriptor::new(&rows[i].id, &rows[i].title).with_context(&rows[i].context);
        let db = SeriesDescriptor::new(&rows[j].id, &rows[j].title).with_context(&rows[j].context);

        let direct = score_series(&da, &db);
        let via = score_series_prepared(&halves[i], &halves[j]);
        assert_eq!(
            direct.score.to_bits(),
            via.score.to_bits(),
            "{} vs {}",
            rows[i].id,
            rows[j].id
        );
        assert_eq!(direct.shared, via.shared);
        assert_eq!(direct.reason, via.reason);
        assert_eq!(direct.confidence, via.confidence);

        let direct = score_event(&da, &db);
        let via = score_event_prepared(&halves[i], &halves[j]);
        assert_eq!(
            direct.score.to_bits(),
            via.score.to_bits(),
            "{} vs {}",
            rows[i].id,
            rows[j].id
        );
        assert_eq!(direct.reason, via.reason);
    }
}

/// Scoring is symmetric in everything a caller acts on.
///
/// The reason line is deliberately not: it quotes the shared terms in the order
/// the *first* side stated them, so a panel reads them back the way the row it
/// is drawn under words them.
#[test]
fn swapping_the_two_sides_does_not_change_the_score() {
    let rows = corpus();
    let halves = prepared(&rows);

    for (i, j) in cross_venue_pairs(&rows).into_iter().step_by(11) {
        let forward = score_series_prepared(&halves[i], &halves[j]);
        let backward = score_series_prepared(&halves[j], &halves[i]);
        assert_eq!(
            forward.score.to_bits(),
            backward.score.to_bits(),
            "{} vs {}",
            rows[i].id,
            rows[j].id
        );
        assert_eq!(forward.confidence, backward.confidence);
        assert_eq!(
            could_reach_floor(&halves[i], &halves[j], MATCH_FLOOR),
            could_reach_floor(&halves[j], &halves[i], MATCH_FLOOR),
        );
    }
}
