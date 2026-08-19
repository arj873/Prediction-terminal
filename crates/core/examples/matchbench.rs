//! Time the matcher, and record exactly what it decided.
//!
//! The cross-venue board is the one place where a constant factor is the
//! difference between a feature and a hang, so any change to `matching` has to
//! be answered with a number rather than an argument. This prints that number,
//! and — more importantly — dumps every score it took to get there, so a change
//! meant to be purely a speed-up can be *shown* to have changed nothing:
//!
//! ```text
//! cargo run --release --example matchbench -- corpus.json before.txt
//! …edit…
//! cargo run --release --example matchbench -- corpus.json after.txt
//! diff before.txt after.txt
//! ```
//!
//! The corpus is a JSON array of `{venue, id, title, context}`, as produced by
//! the server's `dump_descriptors` example from the live catalogues. The
//! committed one is `crates/core/tests/fixtures/series.json`; point this at a
//! full crawl to measure at the sizes that actually hurt.

use std::fmt::Write as _;
use std::time::Instant;

use terminal_core::matching::{
    could_reach_floor, prepare, score_event, score_event_prepared, score_series,
    score_series_prepared, Prepared, SeriesDescriptor, MATCH_FLOOR,
};

struct Row {
    venue: String,
    id: String,
    title: String,
    context: String,
}

fn load(path: &str) -> Vec<Row> {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("corpus json");
    parsed
        .as_array()
        .expect("corpus is an array")
        .iter()
        .map(|row| Row {
            venue: row["venue"].as_str().unwrap_or_default().to_string(),
            id: row["id"].as_str().unwrap_or_default().to_string(),
            title: row["title"].as_str().unwrap_or_default().to_string(),
            context: row["context"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let corpus = args
        .next()
        .unwrap_or_else(|| "crates/core/tests/fixtures/series.json".to_string());
    let snapshot = args.next();
    // Every left descriptor is scored against this many right ones. The whole
    // cross product of a full crawl is ~8M pairs, which is the bug, not the
    // benchmark; a window samples the same work without taking six minutes.
    let window: usize = args
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(usize::MAX);

    let rows = load(&corpus);
    let venues: Vec<String> = {
        let mut seen: Vec<String> = Vec::new();
        for row in &rows {
            if !seen.contains(&row.venue) {
                seen.push(row.venue.clone());
            }
        }
        seen
    };
    eprintln!("{} descriptors across {} venues", rows.len(), venues.len());

    let mut out = String::new();
    let mut pairs = 0usize;
    let started = Instant::now();

    // Pass one: the public entry points, which derive both halves per call.
    // This is what a one-off comparison costs, and what the snapshot records.
    for (vi, left_venue) in venues.iter().enumerate() {
        for right_venue in venues.iter().skip(vi + 1) {
            let left = lane(&rows, left_venue);
            let right = lane(&rows, right_venue);

            for (i, a) in left.iter().enumerate() {
                let da = descriptor(a);
                for b in window_of(&right, i, window) {
                    let db = descriptor(b);
                    let series = score_series(&da, &db);
                    let event = score_event(&da, &db);
                    pairs += 1;

                    if snapshot.is_some() {
                        let _ = writeln!(
                            out,
                            "{}\t{}\t{}\t{}\t{:.17e}\t{:?}\t{}\t{}\t{:.17e}\t{:?}\t{}",
                            left_venue,
                            a.id,
                            right_venue,
                            b.id,
                            series.score,
                            series.confidence,
                            series.shared.join(","),
                            series.reason,
                            event.score,
                            event.confidence,
                            event.reason,
                        );
                    }
                }
            }
        }
    }
    let one_shot = started.elapsed();

    // Pass two: each half derived once, as the cross-venue index will hold it.
    // Deriving is timed too — it is real work, just work done per series rather
    // than per pair, and a fair comparison has to carry it.
    let started = Instant::now();
    let prepared: Vec<Prepared> = rows.iter().map(|r| prepare(&descriptor(r))).collect();
    let preparing = started.elapsed();

    let started = Instant::now();
    for (vi, left_venue) in venues.iter().enumerate() {
        for right_venue in venues.iter().skip(vi + 1) {
            let left = lane_indices(&rows, left_venue);
            let right = lane_indices(&rows, right_venue);
            for (i, a) in left.iter().enumerate() {
                for b in window_of(&right, i, window) {
                    let _ = score_series_prepared(&prepared[*a], &prepared[*b]);
                }
            }
        }
    }
    let prepared_only = started.elapsed();

    // Pass three: the same, but declining pairs that provably cannot clear the
    // floor before doing any of the work.
    let started = Instant::now();
    let mut kept = 0usize;
    for (vi, left_venue) in venues.iter().enumerate() {
        for right_venue in venues.iter().skip(vi + 1) {
            let left = lane_indices(&rows, left_venue);
            let right = lane_indices(&rows, right_venue);
            for (i, a) in left.iter().enumerate() {
                for b in window_of(&right, i, window) {
                    if could_reach_floor(&prepared[*a], &prepared[*b], MATCH_FLOOR) {
                        let _ = score_series_prepared(&prepared[*a], &prepared[*b]);
                        kept += 1;
                    }
                }
            }
        }
    }
    let filtered = started.elapsed();

    // Untimed: a pair the filter turns away must genuinely be below the floor,
    // for both scorers, or the board would silently lose a real match.
    let mut violations = 0usize;
    for (vi, left_venue) in venues.iter().enumerate() {
        for right_venue in venues.iter().skip(vi + 1) {
            let left = lane_indices(&rows, left_venue);
            let right = lane_indices(&rows, right_venue);
            for (i, a) in left.iter().enumerate() {
                for b in window_of(&right, i, window) {
                    if could_reach_floor(&prepared[*a], &prepared[*b], MATCH_FLOOR) {
                        continue;
                    }
                    if score_series_prepared(&prepared[*a], &prepared[*b]).score >= MATCH_FLOOR
                        || score_event_prepared(&prepared[*a], &prepared[*b]).score >= MATCH_FLOOR
                    {
                        violations += 1;
                    }
                }
            }
        }
    }

    let per = |d: std::time::Duration, n: usize| d.as_secs_f64() * 1e6 / n.max(1) as f64;
    eprintln!("{pairs} pairs, {} descriptors", rows.len());
    eprintln!(
        "score_series (public) {:>8.3}s  {:>8.2} µs/pair",
        one_shot.as_secs_f64(),
        per(one_shot, pairs)
    );
    eprintln!(
        "  prepared            {:>8.3}s  {:>8.2} µs/pair   (+{:.3}s deriving {} halves once)",
        prepared_only.as_secs_f64(),
        per(prepared_only, pairs),
        preparing.as_secs_f64(),
        rows.len(),
    );
    eprintln!(
        "  prepared+prefilter  {:>8.3}s  {:>8.2} µs/pair   ({kept} of {pairs} scored, {:.1}%)",
        filtered.as_secs_f64(),
        per(filtered, pairs),
        100.0 * kept as f64 / pairs.max(1) as f64,
    );
    if violations > 0 {
        eprintln!("PREFILTER UNSOUND: {violations} rejected pairs would have cleared the floor");
        std::process::exit(1);
    }
    eprintln!("prefilter admissible on all {pairs} pairs");

    if let Some(path) = snapshot {
        std::fs::write(&path, out).expect("write snapshot");
        eprintln!("snapshot -> {path}");
    }
}

fn descriptor(row: &Row) -> SeriesDescriptor<'_> {
    SeriesDescriptor::new(&row.id, &row.title).with_context(&row.context)
}

fn lane<'a>(rows: &'a [Row], venue: &str) -> Vec<&'a Row> {
    rows.iter().filter(|r| r.venue == venue).collect()
}

fn lane_indices(rows: &[Row], venue: &str) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, r)| r.venue == venue)
        .map(|(i, _)| i)
        .collect()
}

/// The slice of `right` that anchor `i` is compared against.
///
/// Offsetting by the anchor's own position keeps a windowed run from comparing
/// every anchor against the same opening handful, which would sample one corner
/// of the corpus and call it the whole thing.
fn window_of<T>(right: &[T], i: usize, window: usize) -> impl Iterator<Item = &T> {
    right.iter().skip(i % right.len().max(1)).take(window)
}
