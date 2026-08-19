//! Small numeric and text helpers shared across the crate.

use unicode_normalization::UnicodeNormalization;

/// Round to four decimal places, matching JavaScript's `Math.round`.
///
/// Every venue module leans on this to kill the float dust that `1 - p`
/// leaves behind, so a NO price of `0.31` does not surface as
/// `0.30999999999999994`. The tie rule matters and is why this is not
/// `f64::round`: JavaScript rounds a half *up* (toward positive infinity),
/// Rust rounds it *away from zero*, and the two disagree on every negative
/// half — `Math.round(-0.5)` is `-0` where `(-0.5f64).round()` is `-1`.
pub fn round4(n: f64) -> f64 {
    if !n.is_finite() {
        return n;
    }
    (n * 10_000.0 + 0.5).floor() / 10_000.0
}

/// Round to `places` decimals, with the same tie rule as [`round4`].
pub fn round_to(n: f64, places: u32) -> f64 {
    if !n.is_finite() {
        return n;
    }
    let factor = 10f64.powi(places as i32);
    (n * factor + 0.5).floor() / factor
}

/// Decompose to NFKD and drop combining marks.
///
/// The equivalent of `str.normalize('NFKD').replace(/\p{M}/gu, '')`, which is
/// how both the matcher and the Rotten Tomatoes slug builder flatten accented
/// titles so `Amélie` and `Amelie` compare equal.
///
/// ASCII is answered without decomposing anything. NFKD is a no-op on it by
/// definition, and the overwhelming majority of what the matcher reads is a
/// plain ASCII market title — this runs on both sides of every comparison the
/// cross-venue board makes, so the case worth being quick about is the common
/// one, not the accented one.
pub fn fold_diacritics(input: &str) -> String {
    if input.is_ascii() {
        return input.to_string();
    }

    input
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
}

/// Unicode general categories Mn, Mc and Me — the `\p{M}` class.
fn is_combining_mark(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F   // combining diacritical marks
        | 0x0483..=0x0489
        | 0x0591..=0x05BD
        | 0x0610..=0x061A
        | 0x064B..=0x065F
        | 0x0670
        | 0x06D6..=0x06DC
        | 0x0711
        | 0x0730..=0x074A
        | 0x07A6..=0x07B0
        | 0x0816..=0x0819
        | 0x08E3..=0x0903
        | 0x093A..=0x093C
        | 0x093E..=0x094F
        | 0x0951..=0x0957
        | 0x0962..=0x0963
        | 0x0981..=0x0983
        | 0x09BC
        | 0x09BE..=0x09CD
        | 0x1AB0..=0x1AFF
        | 0x1DC0..=0x1DFF
        | 0x20D0..=0x20F0
        | 0x2CEF..=0x2CF1
        | 0x302A..=0x302F
        | 0x3099..=0x309A
        | 0xFE00..=0xFE0F
        | 0xFE20..=0xFE2F
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round4_kills_float_dust_from_one_minus_p() {
        // The case the helper exists for: 1 - 0.69 in binary floating point.
        assert_eq!(round4(1.0 - 0.69), 0.31);
        assert_eq!(round4(0.123_456_789), 0.1235);
        assert_eq!(round4(0.5), 0.5);
    }

    #[test]
    fn round4_rounds_a_half_up_like_javascript() {
        // Math.round(-0.00005 * 10000) === Math.round(-0.5) === -0, not -1.
        assert_eq!(round4(-0.000_05), 0.0);
        assert_eq!(round4(0.000_05), 0.0001);
        // f64::round would give -0.0002 here; JavaScript gives -0.0001.
        assert_eq!(round4(-0.000_15), -0.0001);
    }

    #[test]
    fn round4_passes_non_finite_values_through() {
        assert!(round4(f64::NAN).is_nan());
        assert_eq!(round4(f64::INFINITY), f64::INFINITY);
    }

    #[test]
    fn fold_diacritics_flattens_accents() {
        assert_eq!(fold_diacritics("Amélie"), "Amelie");
        assert_eq!(fold_diacritics("Beyoncé"), "Beyonce");
        assert_eq!(fold_diacritics("naïve café"), "naive cafe");
    }

    #[test]
    fn fold_diacritics_leaves_plain_ascii_alone() {
        assert_eq!(fold_diacritics("Dune: Part Two"), "Dune: Part Two");
    }
}
