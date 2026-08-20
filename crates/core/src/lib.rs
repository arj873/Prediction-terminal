//! Shared, pure logic for the Prediction Terminal.
//!
//! This crate is the Rust successor to the old `src/shared/` directory: the wire
//! contract every route answers with, the implied-price maths, the cross-venue
//! matcher, and the venue registry that says how a contract is named at each
//! broker. It performs no I/O and knows nothing about HTTP, so it stays cheap to
//! test and impossible to accidentally couple to a transport.

pub mod implied;
pub mod matching;
pub mod slug;
pub mod types;
pub mod util;
pub mod venue;

pub use types::*;
pub use venue::{Venue, VenueRef};
