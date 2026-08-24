//! Option pricing and Greeks.
//!
//! Like the implied-price maths, this is the part of the feature whose output
//! looks entirely plausible when it is wrong: a delta computed with the wrong
//! carry, a vega scaled per unit instead of per volatility point, or a theta
//! that forgot to divide by 365 all produce a number of about the right size in
//! about the right place. So it is checked three ways, and each catches things
//! the others cannot:
//!
//!  1. **Against arithmetic.** Put-call parity, intrinsic bounds, and the
//!     identities every option price has to satisfy.
//!  2. **Against finite differences.** Every Greek is re-derived by numerically
//!     differentiating [`black_price`] through the *economic* variables
//!     `(S, r, q, τ, σ)`, rebuilding the forward and discount factor at each
//!     bump. This is what pins the units down — a vega quoted per unit rather
//!     than per point is off by exactly 100 and nothing else notices.
//!  3. **Against Deribit's own published Greeks**, from a captured live
//!     response. An exchange's risk system is the only external oracle
//!     available for this, and it is a good one.
//!
//! The third check is what caught the real bug here. Deribit publishes an
//! `interest_rate` of `0.0` on every instrument while quoting a ten-month
//! forward 3.9% above the index. Trusting the field over the forward left every
//! long-dated delta on the board disagreeing with Deribit's own by that amount.

use serde::Deserialize;
use terminal_core::greeks::{
    black_greeks, black_price, breakeven, equity_expiry_instant, fit_forward, implied_vol,
    max_pain, moneyness, normal_cdf, normal_pdf, years_to_expiry, BlackInputs, PainRung,
    ParityPair, DEFAULT_ASSUMED_RATE, DEFAULT_PARITY_WINDOW,
};
use terminal_core::types::{OptionGreeks, OptionType};

/* ------------------------------------------------------------------ helpers */

/// A textbook set: spot 100, 2% carry-adjusted forward, 25% vol, 6 months.
fn base() -> BlackInputs {
    BlackInputs {
        spot: 100.0,
        forward: 101.0,
        strike: 100.0,
        years: 0.5,
        vol: 0.25,
        discount: (-0.04f64 * 0.5).exp(),
        option_type: OptionType::Call,
    }
}

/// Price as a function of the economic variables, rebuilding the forward and the
/// discount factor from them.
///
/// This is the whole point of the finite-difference checks: bumping `forward`
/// while holding `discount` fixed is a *different* derivative from bumping the
/// rate, and using the first as a check on the second is how a wrong test
/// convinces you that correct code is broken.
#[allow(clippy::too_many_arguments)]
fn value(
    spot: f64,
    rate: f64,
    carry: f64,
    strike: f64,
    years: f64,
    vol: f64,
    option_type: OptionType,
) -> f64 {
    black_price(&BlackInputs {
        spot,
        forward: spot * ((rate - carry) * years).exp(),
        strike,
        years,
        vol,
        discount: (-rate * years).exp(),
        option_type,
    })
    .expect("the textbook inputs price")
}

#[track_caller]
fn close_to(actual: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: expected {expected}, got {actual} (tolerance {tolerance})"
    );
}

/* ------------------------------------------------------------------ pricing */

#[test]
fn satisfies_put_call_parity() {
    let call = black_price(&BlackInputs {
        option_type: OptionType::Call,
        ..base()
    })
    .unwrap();
    let put = black_price(&BlackInputs {
        option_type: OptionType::Put,
        ..base()
    })
    .unwrap();
    // C - P = DF·(F - K), exactly, for any vol.
    let b = base();
    close_to(
        call - put,
        b.discount * (b.forward - b.strike),
        1e-12,
        "parity",
    );
}

#[test]
fn collapses_to_discounted_intrinsic_at_zero_volatility() {
    let b = base();
    let call = black_price(&BlackInputs {
        vol: 0.0,
        strike: 90.0,
        ..b
    })
    .unwrap();
    close_to(
        call,
        b.discount * (b.forward - 90.0),
        1e-12,
        "zero-vol call",
    );
}

#[test]
fn collapses_to_intrinsic_at_expiry_rather_than_returning_none() {
    let b = base();
    let call = black_price(&BlackInputs {
        years: 0.0,
        strike: 90.0,
        ..b
    })
    .expect("an expiring option is still worth its intrinsic");
    close_to(call, b.discount * (b.forward - 90.0), 1e-12, "expiry call");
    // Out of the money, it is worth nothing rather than being unanswerable.
    assert_eq!(
        black_price(&BlackInputs {
            years: 0.0,
            strike: 200.0,
            ..b
        }),
        Some(0.0)
    );
}

#[test]
fn is_increasing_in_volatility() {
    let mut previous = f64::NEG_INFINITY;
    for vol in [0.05, 0.1, 0.25, 0.6, 1.2, 3.0] {
        let price = black_price(&BlackInputs { vol, ..base() }).unwrap();
        assert!(price > previous, "price fell as vol rose to {vol}");
        previous = price;
    }
}

#[test]
fn stays_inside_its_no_arbitrage_bounds() {
    let b = base();
    for strike in [50.0, 80.0, 100.0, 130.0, 200.0] {
        for vol in [0.05, 0.4, 2.0] {
            let call = black_price(&BlackInputs {
                strike,
                vol,
                option_type: OptionType::Call,
                ..b
            })
            .unwrap();
            let put = black_price(&BlackInputs {
                strike,
                vol,
                option_type: OptionType::Put,
                ..b
            })
            .unwrap();

            let floor = (b.discount * (b.forward - strike)).max(0.0);
            assert!(
                call >= floor - 1e-9 && call <= b.discount * b.forward + 1e-9,
                "call outside bounds at K={strike} v={vol}"
            );
            let put_floor = (b.discount * (strike - b.forward)).max(0.0);
            assert!(
                put >= put_floor - 1e-9 && put <= b.discount * strike + 1e-9,
                "put outside bounds at K={strike} v={vol}"
            );
        }
    }
}

#[test]
fn rejects_nonsensical_inputs_rather_than_returning_a_number() {
    for bad in [
        BlackInputs {
            forward: 0.0,
            ..base()
        },
        BlackInputs {
            forward: -1.0,
            ..base()
        },
        BlackInputs {
            strike: 0.0,
            ..base()
        },
        BlackInputs {
            forward: f64::NAN,
            ..base()
        },
    ] {
        assert_eq!(black_price(&bad), None, "priced {bad:?}");
    }
}

/* ------------------------------------------------------------------- greeks */

#[test]
fn reproduces_every_greek_by_finite_difference_in_the_stated_units() {
    let spot: f64 = 100.0;
    let rate: f64 = 0.043;
    let carry: f64 = 0.011;

    for strike in [70.0, 90.0, 100.0, 110.0, 140.0] {
        for years in [0.02, 0.25, 1.5] {
            for vol in [0.15, 0.4, 0.9] {
                for option_type in [OptionType::Call, OptionType::Put] {
                    let forward = spot * ((rate - carry) * years).exp();
                    let greeks = black_greeks(&BlackInputs {
                        spot,
                        forward,
                        strike,
                        years,
                        vol,
                        discount: (-rate * years).exp(),
                        option_type,
                    });

                    let d_s = spot * 1e-5;
                    let price = |s: f64, r: f64, q: f64, t: f64, v: f64| {
                        value(s, r, q, strike, t, v, option_type)
                    };

                    // Gamma is checked as the derivative of *delta*, not as the
                    // second derivative of price. A second difference divides by
                    // `h²`, and there is no bump size that works for both an
                    // at-the-money weekly (where gamma is sharply peaked, so a
                    // wide bump truncates) and a deep in-the-money one (where
                    // gamma is ~1e-10, so a narrow bump is pure cancellation
                    // noise). Differentiating delta once is numerically clean at
                    // every strike, and since delta is itself checked against
                    // price just below, the chain gamma → delta → price still
                    // ties every Greek back to the price.
                    let delta_at = |s: f64| {
                        black_greeks(&BlackInputs {
                            spot: s,
                            forward: s * ((rate - carry) * years).exp(),
                            strike,
                            years,
                            vol,
                            discount: (-rate * years).exp(),
                            option_type,
                        })
                        .delta
                        .unwrap()
                    };

                    let fd_delta = (price(spot + d_s, rate, carry, years, vol)
                        - price(spot - d_s, rate, carry, years, vol))
                        / (2.0 * d_s);
                    let fd_gamma = (delta_at(spot + d_s) - delta_at(spot - d_s)) / (2.0 * d_s);
                    // Per volatility *point*, hence the /100.
                    let fd_vega = (price(spot, rate, carry, years, vol + 1e-6)
                        - price(spot, rate, carry, years, vol - 1e-6))
                        / 2e-6
                        / 100.0;
                    // Per calendar *day*, and time to expiry shrinks as the day
                    // passes.
                    let fd_theta = -(price(spot, rate, carry, years + 1e-6, vol)
                        - price(spot, rate, carry, years - 1e-6, vol))
                        / 2e-6
                        / 365.0;
                    // Per *percentage point* of rate.
                    let fd_rho = (price(spot, rate + 1e-7, carry, years, vol)
                        - price(spot, rate - 1e-7, carry, years, vol))
                        / 2e-7
                        / 100.0;

                    let label = format!("{} K={strike} T={years} v={vol}", option_type.as_str());
                    close_to(
                        greeks.delta.unwrap(),
                        fd_delta,
                        (fd_delta.abs() * 1e-4).max(1e-8),
                        &format!("delta {label}"),
                    );
                    close_to(
                        greeks.gamma.unwrap(),
                        fd_gamma,
                        (fd_gamma.abs() * 1e-4).max(1e-12),
                        &format!("gamma {label}"),
                    );
                    close_to(
                        greeks.vega.unwrap(),
                        fd_vega,
                        (fd_vega.abs() * 1e-4).max(1e-10),
                        &format!("vega {label}"),
                    );
                    close_to(
                        greeks.theta.unwrap(),
                        fd_theta,
                        (fd_theta.abs() * 1e-4).max(1e-10),
                        &format!("theta {label}"),
                    );
                    close_to(
                        greeks.rho.unwrap(),
                        fd_rho,
                        (fd_rho.abs() * 1e-4).max(1e-10),
                        &format!("rho {label}"),
                    );
                }
            }
        }
    }
}

#[test]
fn keeps_delta_in_its_sign_correct_range() {
    for strike in [50.0, 100.0, 200.0] {
        let call = black_greeks(&BlackInputs {
            strike,
            option_type: OptionType::Call,
            ..base()
        });
        let put = black_greeks(&BlackInputs {
            strike,
            option_type: OptionType::Put,
            ..base()
        });
        let c = call.delta.unwrap();
        let p = put.delta.unwrap();
        assert!(c > 0.0 && c < 1.0, "call delta out of range at {strike}");
        assert!(p < 0.0 && p > -1.0, "put delta out of range at {strike}");
    }
}

#[test]
fn gives_a_call_and_a_put_at_the_same_strike_identical_gamma_and_vega() {
    let call = black_greeks(&BlackInputs {
        option_type: OptionType::Call,
        ..base()
    });
    let put = black_greeks(&BlackInputs {
        option_type: OptionType::Put,
        ..base()
    });
    close_to(call.gamma.unwrap(), put.gamma.unwrap(), 1e-12, "gamma");
    close_to(call.vega.unwrap(), put.vega.unwrap(), 1e-12, "vega");
}

#[test]
fn drives_a_deep_in_the_money_call_delta_toward_the_discounted_carry_factor() {
    let b = base();
    let greeks = black_greeks(&BlackInputs {
        strike: 1.0,
        vol: 0.05,
        ..b
    });
    // e^(-qT), which is what a spot delta saturates at rather than exactly 1.
    let carry_factor = b.discount * b.forward / b.spot;
    close_to(greeks.delta.unwrap(), carry_factor, 1e-6, "saturated delta");
}

#[test]
fn returns_nones_rather_than_zeroes_when_the_model_cannot_speak() {
    let expired = black_greeks(&BlackInputs {
        years: 0.0,
        ..base()
    });
    assert_eq!(expired, OptionGreeks::default());
    assert_eq!(
        black_greeks(&BlackInputs { vol: 0.0, ..base() }).delta,
        None
    );
}

/* ------------------------------------------- the exchange as an oracle */

#[derive(Deserialize)]
struct DeribitFixture {
    tickers: Vec<DeribitTicker>,
    instruments: Vec<DeribitInstrument>,
}

#[derive(Deserialize)]
struct DeribitTicker {
    timestamp: f64,
    index_price: f64,
    underlying_price: f64,
    mark_price: f64,
    mark_iv: f64,
    greeks: DeribitGreeks,
}

#[derive(Deserialize)]
struct DeribitGreeks {
    delta: f64,
    gamma: f64,
    vega: f64,
    theta: f64,
}

#[derive(Deserialize)]
struct DeribitInstrument {
    instrument_name: String,
    strike: f64,
    expiration_timestamp: f64,
    option_type: String,
}

fn deribit() -> DeribitFixture {
    serde_json::from_str(include_str!("fixtures/deribit-greeks.json"))
        .expect("the Deribit fixture parses")
}

fn deribit_inputs(ticker: &DeribitTicker, instrument: &DeribitInstrument) -> BlackInputs {
    BlackInputs {
        spot: ticker.index_price,
        forward: ticker.underlying_price,
        strike: instrument.strike,
        years: (instrument.expiration_timestamp - ticker.timestamp) / 1000.0 / (365.0 * 86_400.0),
        vol: ticker.mark_iv / 100.0,
        // The correction this test exists to lock in: the discount factor comes
        // from the spot/forward basis, not from Deribit's `interest_rate: 0.0`.
        discount: ticker.index_price / ticker.underlying_price,
        option_type: if instrument.option_type == "put" {
            OptionType::Put
        } else {
            OptionType::Call
        },
    }
}

#[test]
fn has_a_fixture_of_live_contracts_spanning_moneyness_and_maturity() {
    let fixture = deribit();
    assert!(fixture.tickers.len() >= 10);
    assert_eq!(fixture.tickers.len(), fixture.instruments.len());
}

#[test]
fn reproduces_delta_and_price_from_the_forward_implied_discount_factor() {
    let fixture = deribit();
    for (ticker, instrument) in fixture.tickers.iter().zip(&fixture.instruments) {
        let inputs = deribit_inputs(ticker, instrument);
        let greeks = black_greeks(&inputs);
        let price = black_price(&inputs).unwrap();
        let their_price = ticker.mark_price * ticker.index_price;
        let name = &instrument.instrument_name;

        close_to(
            greeks.delta.unwrap(),
            ticker.greeks.delta,
            1e-3,
            &format!("delta {name}"),
        );
        // Deribit rounds its published gamma to five decimals, which on a
        // $63,000 underlying is coarser than gamma itself.
        close_to(
            greeks.gamma.unwrap(),
            ticker.greeks.gamma,
            1e-5,
            &format!("gamma {name}"),
        );
        // Absolute dollars, not relative: `mark_iv` is published to two decimal
        // places, and one hundredth of a vol point is worth a couple of dollars
        // on a long-dated BTC contract. Every price lands inside the bid/ask.
        close_to(price, their_price, 5.0, &format!("price {name}"));
    }
}

#[test]
fn differs_from_deribit_on_vega_and_theta_by_exactly_the_carry_it_discounts_for() {
    // Delta and gamma agree with the exchange to five decimals. Vega and theta
    // do not, and the reason is a convention rather than an error — worth
    // pinning precisely, because "our vega is 3.8% under Deribit's" is the kind
    // of discrepancy someone will eventually try to fix by breaking the model.
    //
    // Deribit quotes its Greeks in *forward* space. Its vega is undiscounted, so
    // ours is exactly `DF` times it; and its theta is the bare time-decay term
    // `-F·φ(d1)·σ/(2√T)`, carrying neither the rate term nor the carry term that
    // a spot-space theta has.
    //
    // Ours stay in spot space deliberately, because that is the space the panel
    // displays: bump the volatility by a point and the price shown moves by the
    // vega shown. Against a discounted price, Deribit's number does not have
    // that property. Both are right about different questions; this test states
    // which one this terminal answers, and confirms we can reproduce theirs.
    let fixture = deribit();
    let mut checked = 0;

    for (ticker, instrument) in fixture.tickers.iter().zip(&fixture.instruments) {
        let inputs = deribit_inputs(ticker, instrument);
        let greeks = black_greeks(&inputs);
        let (Some(vega), Some(_theta)) = (greeks.vega, greeks.theta) else {
            continue;
        };
        // A contract with a vega under 1.0 is hours from expiry and worth
        // pennies. `mark_iv` is published to two decimals, which on such a
        // contract is most of the number — and Deribit's own theta departs from
        // the decay formula there too (by 60% on the one in this fixture, which
        // is a near-expiry adjustment of its own rather than anything this model
        // should reproduce). Everything with a real vega agrees to 7e-4.
        if ticker.greeks.vega.abs() < 1.0 {
            continue;
        }

        let discount = inputs.discount;
        close_to(
            vega / discount,
            ticker.greeks.vega,
            ticker.greeks.vega.abs() * 1e-3,
            &format!("undiscounted vega {}", instrument.instrument_name),
        );

        // The pure decay term, undiscounted, per calendar day — reconstructed
        // from the same inputs rather than from our own theta, so this is a
        // statement about Deribit's convention and not a restatement of ours.
        let sqrt_t = inputs.years.sqrt();
        let d1 = ((inputs.forward / inputs.strike).ln()
            + 0.5 * inputs.vol * inputs.vol * inputs.years)
            / (inputs.vol * sqrt_t);
        let decay = -inputs.forward * normal_pdf(d1) * inputs.vol / (2.0 * sqrt_t) / 365.0;
        close_to(
            decay,
            ticker.greeks.theta,
            ticker.greeks.theta.abs() * 1e-3,
            &format!("undiscounted decay {}", instrument.instrument_name),
        );

        checked += 1;
    }

    assert!(checked >= 12, "only {checked} contracts were comparable");
}

#[test]
fn would_disagree_with_deribit_if_the_interest_rate_field_were_believed() {
    // The regression guard. With `discount: 1` — what `interest_rate: 0.0`
    // implies — the longest-dated contract's delta is out by several percent.
    let fixture = deribit();
    let long_dated = fixture
        .tickers
        .iter()
        .zip(&fixture.instruments)
        .find(|(ticker, instrument)| {
            (instrument.expiration_timestamp - ticker.timestamp) / 1000.0 / (365.0 * 86_400.0) > 0.5
        })
        .expect("fixture should contain a long-dated contract");

    let (ticker, instrument) = long_dated;
    let naive = black_greeks(&BlackInputs {
        discount: 1.0,
        ..deribit_inputs(ticker, instrument)
    });

    assert!(
        (naive.delta.unwrap() - ticker.greeks.delta).abs() > 0.01,
        "an undiscounted delta should visibly disagree with the exchange"
    );
}

/* ------------------------------------------------------- implied volatility */

#[test]
fn round_trips_a_price_back_to_the_volatility_that_made_it() {
    let mut checked = 0;
    for strike in [60.0, 90.0, 100.0, 115.0, 160.0] {
        for years in [0.01, 0.3, 2.0] {
            for vol in [0.08, 0.35, 1.4] {
                for option_type in [OptionType::Call, OptionType::Put] {
                    let inputs = BlackInputs {
                        strike,
                        years,
                        vol,
                        option_type,
                        discount: (-0.04f64 * years).exp(),
                        ..base()
                    };
                    let price = black_price(&inputs).unwrap();
                    if let Some(solved) = implied_vol(Some(price), &inputs) {
                        close_to(
                            solved,
                            vol,
                            1e-6,
                            &format!("{} K={strike} T={years} v={vol}", option_type.as_str()),
                        );
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(checked >= 80, "only {checked} contracts round-tripped");
}

#[test]
fn has_no_answer_for_a_contract_trading_at_intrinsic() {
    let b = base();
    let inputs = BlackInputs { strike: 50.0, ..b };
    let intrinsic = b.discount * (b.forward - 50.0);
    assert_eq!(implied_vol(Some(intrinsic), &inputs), None);
}

#[test]
fn still_recovers_the_volatility_of_a_vanishingly_cheap_contract() {
    // A 160-strike call on a 101 forward at 5% vol is worth about 1.7e-39, and
    // the volatility is still perfectly well defined. This is the case that
    // caught the solver judging convergence on an *absolute* price tolerance:
    // under `|error| < 1e-10`, every vol from 1% to 500% "converged", and the
    // answer was whichever one the iteration happened to try first.
    let inputs = BlackInputs {
        strike: 160.0,
        vol: 0.05,
        option_type: OptionType::Call,
        ..base()
    };
    let price = black_price(&inputs).unwrap();
    assert!(
        price > 0.0 && price < 1e-30,
        "expected a vanishing price, got {price}"
    );

    let solved = implied_vol(Some(price), &inputs).expect("a vanishing price still has a vol");
    close_to(solved, 0.05, 1e-6, "vol of a vanishing contract");
}

#[test]
fn solves_the_wings_where_vega_is_nearly_zero() {
    // A far out-of-the-money contract is exactly where a bare Newton solver
    // diverges, and exactly where a chain still wants a number.
    let inputs = BlackInputs {
        strike: 400.0,
        vol: 0.8,
        option_type: OptionType::Call,
        ..base()
    };
    let price = black_price(&inputs).unwrap();
    let solved = implied_vol(Some(price), &inputs).expect("the wing solves");
    close_to(solved, 0.8, 1e-5, "deep OTM");
}

#[test]
fn returns_none_below_intrinsic_and_above_the_ceiling() {
    let b = base();
    let inputs = BlackInputs { strike: 50.0, ..b };
    assert_eq!(implied_vol(Some(0.01), &inputs), None, "below intrinsic");
    assert_eq!(
        implied_vol(Some(b.discount * b.forward + 1.0), &inputs),
        None,
        "above ceiling"
    );
}

#[test]
fn treats_a_missing_or_non_positive_price_as_unanswerable() {
    assert_eq!(implied_vol(None, &base()), None);
    assert_eq!(implied_vol(Some(0.0), &base()), None);
    assert_eq!(implied_vol(Some(-1.0), &base()), None);
}

#[test]
fn returns_none_rather_than_pinning_to_a_bracket_edge() {
    // A price a hair under the ceiling implies a vol above the bracket.
    let b = base();
    let ceiling = b.discount * b.forward;
    assert_eq!(implied_vol(Some(ceiling * (1.0 - 1e-15)), &b), None);
}

/* ------------------------------------------------------------ forward fit */

fn parity_board(spot: f64, years: f64, rate: f64, carry: f64) -> Vec<ParityPair> {
    let forward = spot * ((rate - carry) * years).exp();
    let discount = (-rate * years).exp();
    let mut pairs = Vec::new();
    for step in -5..=5 {
        let strike = spot + f64::from(step) * 2.0;
        let inputs = BlackInputs {
            spot,
            forward,
            strike,
            years,
            vol: 0.3,
            discount,
            option_type: OptionType::Call,
        };
        pairs.push(ParityPair {
            strike,
            call_price: black_price(&inputs).unwrap(),
            put_price: black_price(&BlackInputs {
                option_type: OptionType::Put,
                ..inputs
            })
            .unwrap(),
        });
    }
    pairs
}

#[test]
fn recovers_the_forward_and_the_discount_factor_exactly() {
    let (spot, years, rate, carry) = (100.0, 0.5, 0.045, 0.015);
    let fit = fit_forward(
        &parity_board(spot, years, rate, carry),
        spot,
        years,
        DEFAULT_PARITY_WINDOW,
        DEFAULT_ASSUMED_RATE,
    )
    .expect("a clean board fits");

    close_to(
        fit.forward,
        spot * ((rate - carry) * years).exp(),
        1e-8,
        "forward",
    );
    close_to(fit.discount, (-rate * years).exp(), 1e-10, "discount");
    close_to(fit.rate, rate, 1e-9, "rate");
    close_to(fit.carry, carry, 1e-9, "carry");
}

#[test]
fn is_unmoved_by_noise_on_individual_strikes_which_one_atm_pair_is_not() {
    let (spot, years, rate, carry) = (100.0, 0.5, 0.045, 0.015);
    let mut pairs = parity_board(spot, years, rate, carry);
    let truth = spot * ((rate - carry) * years).exp();

    // A tick of noise, alternating sign — what a crossed book looks like.
    for (i, pair) in pairs.iter_mut().enumerate() {
        let nudge = if i % 2 == 0 { 0.02 } else { -0.02 };
        pair.call_price += nudge;
        pair.put_price -= nudge;
    }

    let fit = fit_forward(
        &pairs,
        spot,
        years,
        DEFAULT_PARITY_WINDOW,
        DEFAULT_ASSUMED_RATE,
    )
    .expect("noise does not stop a fit");
    // The regression averages the noise out; a single pair would inherit it.
    close_to(fit.forward, truth, 0.05, "forward under noise");
}

#[test]
fn ignores_strikes_outside_the_near_the_money_window() {
    let (spot, years) = (100.0, 0.5);
    let mut pairs = parity_board(spot, years, 0.045, 0.015);
    // A deep in-the-money put carrying an American early-exercise premium.
    pairs.push(ParityPair {
        strike: 200.0,
        call_price: 0.01,
        put_price: 99.0,
    });

    let fit = fit_forward(
        &pairs,
        spot,
        years,
        DEFAULT_PARITY_WINDOW,
        DEFAULT_ASSUMED_RATE,
    )
    .expect("the window keeps the fit clean");
    assert_eq!(fit.used, 11, "the far strike should not be eligible");
}

#[test]
fn declines_to_fit_when_there_is_not_enough_to_fit() {
    let pairs = vec![
        ParityPair {
            strike: 100.0,
            call_price: 5.0,
            put_price: 4.0,
        },
        ParityPair {
            strike: 102.0,
            call_price: 4.0,
            put_price: 5.0,
        },
    ];
    assert_eq!(
        fit_forward(
            &pairs,
            100.0,
            0.5,
            DEFAULT_PARITY_WINDOW,
            DEFAULT_ASSUMED_RATE
        ),
        None
    );
}

#[test]
fn falls_back_to_a_pinned_discount_when_the_slope_is_noise() {
    // A one-day expiry, where the slope is ~0.9999 and a cent of noise on it
    // annualises to an absurd rate. The forward — the intercept — survives.
    let (spot, years) = (100.0, 1.0 / 365.0);
    let mut pairs = parity_board(spot, years, 0.045, 0.0);
    for (i, pair) in pairs.iter_mut().enumerate() {
        let nudge = if i % 2 == 0 { 0.05 } else { -0.05 };
        pair.call_price += nudge;
    }

    let fit = fit_forward(
        &pairs,
        spot,
        years,
        DEFAULT_PARITY_WINDOW,
        DEFAULT_ASSUMED_RATE,
    )
    .expect("parity still knows where the forward is");
    // The pinned branch reports the assumed rate rather than the fitted one.
    close_to(fit.rate, DEFAULT_ASSUMED_RATE, 1e-12, "pinned rate");
    close_to(fit.forward, spot, 0.2, "pinned forward");
}

/* ---------------------------------------------------- moneyness and pain */

#[test]
fn reads_moneyness_as_strike_over_spot_whichever_way_the_contract_pays() {
    close_to(moneyness(110.0, 100.0).unwrap(), 1.1, 1e-12, "high strike");
    close_to(moneyness(90.0, 100.0).unwrap(), 0.9, 1e-12, "low strike");
    assert_eq!(moneyness(110.0, 0.0), None);
}

#[test]
fn puts_a_breakeven_the_right_side_of_the_strike_for_each_leg() {
    close_to(
        breakeven(OptionType::Call, 100.0, Some(5.0)).unwrap(),
        105.0,
        1e-12,
        "call breakeven",
    );
    close_to(
        breakeven(OptionType::Put, 100.0, Some(5.0)).unwrap(),
        95.0,
        1e-12,
        "put breakeven",
    );
    assert_eq!(breakeven(OptionType::Call, 100.0, None), None);
}

#[test]
fn finds_the_settlement_price_that_pays_out_least() {
    let rungs = [
        PainRung {
            strike: 90.0,
            call_open_interest: 100.0,
            put_open_interest: 0.0,
        },
        PainRung {
            strike: 100.0,
            call_open_interest: 10.0,
            put_open_interest: 10.0,
        },
        PainRung {
            strike: 110.0,
            call_open_interest: 0.0,
            put_open_interest: 100.0,
        },
    ];
    let (strike, _) = max_pain(&rungs).expect("a ladder has a max pain");
    // Every call is struck at or below 90 in size and every put at or above 110,
    // so settling in the middle is what costs writers least.
    assert!((90.0..=110.0).contains(&strike));
}

#[test]
fn balances_calls_against_puts() {
    // All the open interest is calls struck at 100; settling at or below 100
    // pays nothing.
    let rungs = [
        PainRung {
            strike: 100.0,
            call_open_interest: 500.0,
            put_open_interest: 0.0,
        },
        PainRung {
            strike: 120.0,
            call_open_interest: 0.0,
            put_open_interest: 0.0,
        },
    ];
    let (strike, payout) = max_pain(&rungs).unwrap();
    close_to(strike, 100.0, 1e-12, "pain strike");
    close_to(payout, 0.0, 1e-12, "pain payout");
}

#[test]
fn has_no_answer_for_an_empty_ladder() {
    assert_eq!(max_pain(&[]), None);
    assert_eq!(
        max_pain(&[PainRung {
            strike: 0.0,
            call_open_interest: 1.0,
            put_open_interest: 1.0,
        }]),
        None
    );
}

/* ------------------------------------------------------------------- time */

#[test]
fn measures_act_365_and_floors_at_zero() {
    close_to(
        years_to_expiry(1_000_000.0 + 365.0 * 86_400.0, 1_000_000.0),
        1.0,
        1e-12,
        "one year",
    );
    close_to(
        years_to_expiry(500.0, 1_000.0),
        0.0,
        1e-12,
        "already expired",
    );
}

#[test]
fn lands_on_1600_new_york_through_both_halves_of_the_year() {
    // January: EST, UTC-5, so 16:00 local is 21:00 UTC.
    assert_eq!(
        equity_expiry_instant("2026-01-16"),
        Some(days_since_epoch(2026, 1, 16) * 86_400 + 21 * 3_600)
    );
    // July: EDT, UTC-4, so 16:00 local is 20:00 UTC.
    assert_eq!(
        equity_expiry_instant("2026-07-17"),
        Some(days_since_epoch(2026, 7, 17) * 86_400 + 20 * 3_600)
    );
}

#[test]
fn handles_the_days_either_side_of_a_dst_switch() {
    // 2026: daylight time starts Sunday 8 March and ends Sunday 1 November.
    let before_spring = equity_expiry_instant("2026-03-06").unwrap();
    let after_spring = equity_expiry_instant("2026-03-09").unwrap();
    assert_eq!(
        before_spring % 86_400,
        21 * 3_600,
        "the Friday before is still EST"
    );
    assert_eq!(after_spring % 86_400, 20 * 3_600, "the Monday after is EDT");

    let before_fall = equity_expiry_instant("2026-10-30").unwrap();
    let after_fall = equity_expiry_instant("2026-11-02").unwrap();
    assert_eq!(before_fall % 86_400, 20 * 3_600, "the Friday before is EDT");
    assert_eq!(after_fall % 86_400, 21 * 3_600, "the Monday after is EST");

    // The switch days themselves: 8 March is already daylight time at 16:00,
    // and 1 November is already back on standard time.
    assert_eq!(
        equity_expiry_instant("2026-03-08").unwrap() % 86_400,
        20 * 3_600
    );
    assert_eq!(
        equity_expiry_instant("2026-11-01").unwrap() % 86_400,
        21 * 3_600
    );
}

#[test]
fn rejects_anything_that_is_not_an_iso_date() {
    for bad in [
        "",
        "2026-13-01",
        "2026-02-30",
        "16 Jan 2026",
        "2026-1-16",
        "not a date",
    ] {
        assert_eq!(equity_expiry_instant(bad), None, "accepted {bad:?}");
    }
}

/// Days since 1970-01-01, computed independently of the implementation so the
/// expiry tests are not checking the module against itself.
fn days_since_epoch(year: i64, month: u32, day: u32) -> i64 {
    let mut days = 0i64;
    for y in 1970..year {
        days += if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
            366
        } else {
            365
        };
    }
    let lengths = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..month {
        days += lengths[(m - 1) as usize];
        if m == 2 && ((year % 4 == 0 && year % 100 != 0) || year % 400 == 0) {
            days += 1;
        }
    }
    days + i64::from(day) - 1
}

/* -------------------------------------------------------------- normal cdf */

#[test]
fn matches_known_values_of_the_standard_normal() {
    close_to(normal_cdf(0.0), 0.5, 1e-15, "N(0)");
    close_to(normal_cdf(1.0), 0.841_344_746_068_543, 1e-14, "N(1)");
    close_to(normal_cdf(-1.0), 0.158_655_253_931_457, 1e-14, "N(-1)");
    close_to(normal_cdf(1.96), 0.975_002_104_851_780, 1e-13, "N(1.96)");
}

#[test]
fn keeps_its_precision_in_the_far_tail_where_a_smiles_wings_live() {
    // The reason this is not the textbook Abramowitz & Stegun approximation.
    // A&S is accurate to 1.5e-7 *absolute*, and N(-8) is 6.2e-16 — so it would
    // return the tail as pure approximation error, and price the wing of every
    // smile from it. Relative accuracy is what matters out here.
    //
    // The references are erfc computed to fifty digits. The bound each is held
    // to is the one actually measured, so the module's own claim about its
    // accuracy is a tested statement rather than a hopeful one: full double
    // precision through the body, degrading to ~1e-8 by N(-12) — which is still
    // eight orders better than A&S manages at N(-5), let alone N(-12).
    for (x, expected, bound) in [
        (-1.0, 0.158_655_253_931_457_05, 1e-15),
        (-2.0, 2.275_013_194_817_921e-2, 1e-15),
        (-3.0, 1.349_898_031_630_094_6e-3, 1e-13),
        (-5.0, 2.866_515_718_791_939e-7, 1e-10),
        (-7.0, 1.279_812_543_885_835e-12, 1e-8),
        (-8.0, 6.220_960_574_271_784e-16, 1e-8),
        (-10.0, 7.619_853_024_160_526e-24, 1e-8),
        (-12.0, 1.776_482_121_628_736_7e-33, 1e-8),
    ] {
        let got = normal_cdf(x);
        let relative = ((got - expected) / expected).abs();
        assert!(
            relative < bound,
            "N({x}): got {got:e}, expected {expected:e}, relative error {relative:e}"
        );
        assert!(got > 0.0, "N({x}) must stay positive");
    }

    // Past 37 standard deviations the tail underflows a double, and saturating
    // is the honest answer rather than a denormal.
    assert_eq!(normal_cdf(-40.0), 0.0);
    assert_eq!(normal_cdf(40.0), 1.0);
}

#[test]
fn is_symmetric() {
    for x in [0.1, 0.7, 1.5, 3.0, 6.5, 9.0] {
        close_to(
            normal_cdf(x) + normal_cdf(-x),
            1.0,
            1e-15,
            &format!("symmetry at {x}"),
        );
    }
}
