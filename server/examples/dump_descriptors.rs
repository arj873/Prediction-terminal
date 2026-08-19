//! Reduce the crawled catalogues to the triples the matcher actually reads.
//!
//! Not part of the product. It exists so `matching`'s regression corpus is made
//! of titles three real exchanges wrote, rather than titles someone invented to
//! pass a test.
//!
//! The grouping mirrors `crossvenue::build_index`: a series is keyed by its
//! series ticker (falling back to the event ticker where a venue publishes
//! none), stands for itself with its busiest event, and offers the matcher that
//! event's sub-title and category as context.
//!
//! `cargo run --release --example dump_descriptors -- <corpus-dir> <out.json>`

use std::collections::HashMap;

use terminal_core::types::VenueEvent;
use terminal_core::venue::venue_ids;

fn volume(event: &VenueEvent) -> f64 {
    event.markets.iter().filter_map(|m| m.volume24h).sum()
}

fn main() -> anyhow::Result<()> {
    let dir = std::env::args().nth(1).expect("corpus dir");
    let out = std::env::args().nth(2).expect("out file");

    let mut all: Vec<serde_json::Value> = Vec::new();

    for venue in venue_ids() {
        let path = format!("{dir}/{}.json", venue.as_str());
        let Ok(raw) = std::fs::read(&path) else {
            eprintln!("{path}: missing, skipped");
            continue;
        };
        let events: Vec<VenueEvent> = serde_json::from_slice(&raw)?;

        let mut order: Vec<String> = Vec::new();
        let mut grouped: HashMap<String, Vec<&VenueEvent>> = HashMap::new();
        for event in &events {
            let key = if event.series_ticker.is_empty() {
                &event.event_ticker
            } else {
                &event.series_ticker
            };
            if key.is_empty() {
                continue;
            }
            grouped.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                Vec::new()
            });
            grouped.get_mut(key).expect("just inserted").push(event);
        }

        let mut count = 0usize;
        for key in &order {
            let group = &grouped[key];
            let lead = group
                .iter()
                .max_by(|a, b| {
                    volume(a)
                        .partial_cmp(&volume(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .expect("non-empty group");
            all.push(serde_json::json!({
                "venue": venue.as_str(),
                "id": key,
                "title": lead.title,
                "context": format!("{} {}", lead.sub_title, lead.category),
            }));
            count += 1;
        }
        eprintln!("{}: {count} series", venue.as_str());
    }

    std::fs::write(&out, serde_json::to_vec_pretty(&all)?)?;
    eprintln!("wrote {} descriptors to {out}", all.len());
    Ok(())
}
