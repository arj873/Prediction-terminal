//! Deciding whether two brokers are listing the same question.
//!
//! Six exchanges word the same market six ways and name it six more:
//!
//! ```text
//!   Kalshi          KXFEDDECISION      "Fed decision in Oct 2026?"
//!   Polymarket      fomc               "Fed Decision in October?"
//!   Polymarket US   usfed-fomc         "Fed Decision in October"
//!   Gemini          FED                "Fed decision in September?"
//!   predict.fun     fed-decision-in    "Fed Decision in September?"
//!   ForecastEx      FFDEC              "Fed Decision September 16 2026"
//! ```
//!
//! Nothing in any payload connects those. What connects them is the language,
//! and this module is the part of the terminal that reads it — kept pure and
//! free of any network so the judgement can be tested against fixed strings
//! rather than against whatever is listed today.
//!
//! The output is a score *and* the terms it came from. A cross-venue quote that
//! cannot say why it thinks two contracts are the same contract is worth less
//! than no quote at all, because a trader will act on it.
//!
//! The module is `matching` rather than `match` only because the latter is a
//! keyword; it is the Rust reading of `src/shared/match.ts`, rule for rule.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use fancy_regex::Regex as FancyRegex;
use regex::Regex;

use crate::types::MatchConfidence;
use crate::util::fold_diacritics;

/* ---------------------------------------------------------------- tokens */

/// Words that appear in market titles without narrowing anything down.
///
/// Question scaffolding ("will", "be", "the") and the exchanges' own filler
/// ("market", "winner", "outcome"). Dropping them stops two unrelated markets
/// from scoring on "will the … be".
static STOPWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "a", "an", "and", "any", "are", "as", "at", "be", "been", "before", "by", "do", "does",
        "for", "from", "get", "has", "have", "how", "in", "is", "it", "many", "much", "of", "on",
        "or", "the", "their", "there", "this", "to", "up", "was", "what", "when", "which", "who",
        "will", "with", "market", "markets", "outcome", "contract", "event",
        // `city` is in half the place names on the board and distinguishes none of
        // them: Oklahoma City and Panama City differ by the *other* word.
        "city",
        // Clock furniture from Kalshi's dated titles ("on Aug 21, 2026 at 5pm EDT").
        "am", "pm", "et", "edt", "est", "ct", "cdt", "cst", "pt", "pdt", "pst", "utc", "gmt",
    ]
    .into_iter()
    .collect()
});

/// Terms the venues use interchangeably, folded onto one word.
///
/// Every entry is a pair actually observed across the live catalogues, not a
/// general-purpose thesaurus: `fomc` and `fed` name one committee, `btc` and
/// `bitcoin` one asset, `gop` and `republican` one party. Folding anything
/// looser would start matching questions that merely share a subject.
///
/// `maintain` is deliberately not here, though Gemini words the middle FOMC
/// rung "Fed maintains rate" where every other venue writes "No change". It is
/// folded by the *phrase* rewrite below instead, because the bare verb also
/// carries "Republicans maintain Senate majority" and a dozen other markets
/// where turning it into `nochange` would pair a control race with a rate hold.
/// A word that is only unambiguous next to its object belongs in a phrase rule.
static SYNONYMS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    [
        ("fomc", "fed"),
        // `federal` deliberately does NOT fold onto `fed`. It appears in "federal
        // crime", "federal government", "federal court" and a dozen other markets
        // that have nothing to do with the Federal Reserve, and folding it paired a
        // federal-charges market with the Fed's year-end target rate. "Federal
        // Reserve" still reaches `fed` through `reserve`.
        ("reserve", "fed"),
        ("fedfunds", "fed"),
        ("powell", "powell"),
        ("rates", "rate"),
        ("bps", "bp"),
        ("basis", "bp"),
        ("points", "bp"),
        ("cut", "decrease"),
        ("cuts", "decrease"),
        ("lower", "decrease"),
        ("hike", "increase"),
        ("hikes", "increase"),
        ("raise", "increase"),
        ("unchanged", "nochange"),
        ("hold", "nochange"),
        ("btc", "bitcoin"),
        ("xbt", "bitcoin"),
        ("eth", "ethereum"),
        ("sol", "solana"),
        ("doge", "dogecoin"),
        ("hype", "hyperliquid"),
        ("zec", "zcash"),
        // Index and commodity tickers. Kalshi titles by ticker, Polymarket spells the
        // instrument out and puts the ticker in parentheses.
        ("spx", "sp500"),
        ("inx", "sp500"),
        ("ndx", "nasdaq100"),
        ("gc", "gold"),
        ("cl", "wti"),
        // Both venues describe the same quantity with different verbs: Polymarket
        // asks what a price will *hit*, Kalshi how *high* it will get.
        ("hit", "high"),
        ("maximum", "high"),
        ("minimum", "low"),
        ("best", "top"),
        ("sptfy", "spotify"),
        ("gpu", "nvidia"),
        ("rental", "hourly"),
        ("wealth", "networth"),
        ("gop", "republican"),
        ("republicans", "republican"),
        ("dem", "democrat"),
        ("dems", "democrat"),
        ("democrats", "democrat"),
        ("democratic", "democrat"),
        ("potus", "president"),
        ("presidential", "president"),
        // Central bank tickers. Polymarket US names its rate books `boj`, `boe`,
        // `bcb`; Kalshi spells them out. Each side is folded onto one distinctive
        // token, so the bank is matched on its identity rather than on the words
        // "bank" and "decision", which every one of them shares.
        ("boj", "bankofjapan"),
        ("boe", "bankofengland"),
        ("boc", "bankofcanada"),
        ("bcb", "bankofbrazil"),
        ("bcbbrazil", "bankofbrazil"),
        ("boi", "bankofisrael"),
        ("bok", "bankofkorea"),
        ("cbr", "bankofrussia"),
        ("banxico", "bankofmexico"),
        ("ecb", "ecb"),
        ("rbnz", "bankofnewzealand"),
        ("rba", "bankofaustralia"),
        ("snb", "bankofswitzerland"),
        ("pboc", "bankofchina"),
        ("rbi", "bankofindia"),
        ("inflation", "cpi"),
        ("unemployment", "jobless"),
        ("nyc", "newyork"),
        ("ny", "newyork"),
        ("la", "losangeles"),
        ("sf", "sanfrancisco"),
        ("temp", "temperature"),
        ("temperatures", "temperature"),
        ("high", "high"),
        ("highest", "high"),
        ("low", "low"),
        ("lowest", "low"),
        ("academy", "oscar"),
        ("oscars", "oscar"),
        ("awards", "award"),
        ("champion", "champ"),
        ("championship", "champ"),
        ("champions", "champ"),
        ("winner", "win"),
        ("wins", "win"),
        ("winners", "win"),
        ("season", "season"),
        ("midterm", "midterms"),
        ("senate", "senate"),
        ("house", "house"),
        ("governor", "governor"),
        ("gubernatorial", "governor"),
        ("election", "election"),
        ("elections", "election"),
    ]
    .into_iter()
    .collect()
});

/// Month names, folded to a marked number so `Oct` and `October` agree.
///
/// Marked rather than bare, because a month means different things to the two
/// callers: an *event* in October is not the same event as one in January, but a
/// *series* whose next event falls in October is the same series as one whose
/// next event falls in January. The `m` prefix keeps that distinction available
/// instead of collapsing it into an ordinary number.
static MONTHS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    [
        ("jan", "m1"),
        ("january", "m1"),
        ("feb", "m2"),
        ("february", "m2"),
        ("mar", "m3"),
        ("march", "m3"),
        ("apr", "m4"),
        ("april", "m4"),
        ("may", "m5"),
        ("jun", "m6"),
        ("june", "m6"),
        ("jul", "m7"),
        ("july", "m7"),
        ("aug", "m8"),
        ("august", "m8"),
        ("sep", "m9"),
        ("sept", "m9"),
        ("september", "m9"),
        ("oct", "m10"),
        ("october", "m10"),
        ("nov", "m11"),
        ("november", "m11"),
        ("dec", "m12"),
        ("december", "m12"),
    ]
    .into_iter()
    .collect()
});

static MONTH_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^m(?:[1-9]|1[0-2])$").expect("month token pattern"));

fn is_month(token: &str) -> bool {
    MONTH_TOKEN.is_match(token)
}

/// Multi-word phrases the venues use for one idea, folded before tokenising.
///
/// These cannot go in [`SYNONYMS`], which maps single words: Kalshi's `Fed
/// maintains rate`, Polymarket's `No change` and Polymarket US's `Unchanged` are
/// the same rung of the same ladder, and no word-for-word mapping connects them.
///
/// Every `\b` here is safe as the Unicode-aware boundary the `regex` crate
/// compiles it to, unlike the ASCII-only one JavaScript uses: these run *after*
/// the punctuation strip below, which has already reduced the text to
/// `[a-z0-9.%$+<>≥≤ ]`, and the two readings agree on every one of those.
static PHRASES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        // A central bank's name is its identity; the words around it are shared by
        // every other central bank on the board.
        (r"\bbank of japan\b|\bboj\b", " bankofjapan "),
        (r"\bbank of england\b|\bboe\b", " bankofengland "),
        (r"\bbank of canada\b|\bboc\b", " bankofcanada "),
        (r"\b(?:central )?bank of brazil\b|\bbcb\b", " bankofbrazil "),
        (r"\bbank of israel\b|\bboi\b", " bankofisrael "),
        (r"\bbank of korea\b|\bbok\b", " bankofkorea "),
        (r"\bbank of russia\b|\bcbr\b", " bankofrussia "),
        (r"\bbank of mexico\b|\bbanxico\b", " bankofmexico "),
        (
            r"\breserve bank of new zealand\b|\brbnz\b",
            " bankofnewzealand ",
        ),
        (
            r"\breserve bank of australia\b|\brba\b",
            " bankofaustralia ",
        ),
        (r"\beuropean central bank\b|\becb\b", " ecb "),
        // Weather states its direction, and that direction is the whole question. It
        // needs a token of its own: `high` alone also means "how high will BTC get",
        // and one word cannot guard both.
        (
            r"\b(?:highest|high|max|maximum) temperature\b",
            " hightemp ",
        ),
        (r"\b(?:lowest|low|min|minimum) temperature\b", " lowtemp "),
        (r"\bgrand theft auto\b|\bgta\b", " gta "),
        (r"\ball[- ]time high\b", " high "),
        (r"\bnet worth\b|\bhow rich\b", " networth "),
        (r"\bcircuitbreaker\b", " circuit breaker "),
        (r"\bmarketwide\b", " market wide "),
        // `sandp` is what PUNCTUATED_PHRASES leaves behind for `S&P`; folding it here
        // puts it past the letter/digit splitter, where `sp500` can survive intact
        // and meet the same token SYNONYMS folds `spx` and `inx` onto.
        (r"\bsandp\s*500\b|\bsandp\b", " sp500 "),
        (r"\bnasdaq[- ]?100\b", " nasdaq100 "),
        // A city named in full has to meet the same city named short. SYNONYMS maps
        // the abbreviations onto these tokens (`nyc` → `newyork`); without the
        // spelled-out side folded to match, `NYC` and `New York City` shared no term
        // at all — and `york` is a place qualifier, so the lopsided-qualifier penalty
        // then fired on top and drove the pair below the match floor.
        (r"\bnew york city\b|\bnew york\b", " newyork "),
        (r"\blos angeles\b", " losangeles "),
        (r"\bsan francisco\b", " sanfrancisco "),
        (r"\bnew jersey\b", " newjersey "),
        (r"\bnew hampshire\b", " newhampshire "),
        (r"\bnew mexico\b", " newmexico "),
        (r"\bno change\b", "nochange"),
        (r"\bunchanged\b", "nochange"),
        (r"\bmaintains? (?:the )?(?:rate|rates)\b", "nochange"),
        (r"\bbasis points?\b", "bp"),
        (r"\bbps\b", "bp"),
        // A comparator is the difference between two rungs of a ladder, so `>25bps`
        // and `50+ bps` keep the fact that they are open-ended.
        (
            r"[>≥+]|\bor (?:above|more|higher|greater)\b|\bat least\b",
            " over ",
        ),
        (
            r"[<≤]|\bor (?:below|less|lower|fewer)\b|\bat most\b",
            " under ",
        ),
    ]
    .into_iter()
    .map(|(pattern, replacement)| (Regex::new(pattern).expect("phrase pattern"), replacement))
    .collect()
});

/// Phrases whose own punctuation is what identifies them.
///
/// These have to fold before the strip below removes the very characters they
/// match on, and all three had been sitting after it — dead, silently. `s&p 500`
/// arrived as `s p 500`, so an S&P title could never meet the `sp500` that
/// [`SYNONYMS`] folds `spx` and `inx` onto; `up/down` arrived as `up down`, where
/// `up` is a stopword and only `down` survived; and Kalshi's live-strike suffix
/// `· $1,883.54 target` — the noise this rule exists to delete, and which
/// outweighs every content word in a short-interval title — arrived already
/// broken into pieces the pattern could not span.
///
/// These run on the raw title, where a non-Latin script can still sit against a
/// match, so the boundaries are spelled `(?-u:\b)` to keep JavaScript's
/// ASCII-only reading of `\b` rather than the Unicode one `regex` defaults to.
static PUNCTUATED_PHRASES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        // Spelled out rather than folded straight to `sp500`, because the letter/digit
        // split further down would only tear that back into `sp 500`. The all-letter
        // form survives it and is folded with the rest, after the splitter has run.
        (r"(?-u:\b)s\s*&\s*p(?-u:\b)", " sandp "),
        (r"(?-u:\b)up\s*/\s*down(?-u:\b)", " up or down "),
        (r"·?\s*\$[0-9,.]+\s*target(?-u:\b)", " "),
    ]
    .into_iter()
    .map(|(pattern, replacement)| {
        (
            Regex::new(pattern).expect("punctuated phrase pattern"),
            replacement,
        )
    })
    .collect()
});

/// Keep a grouped number whole.
///
/// The strip below turns every comma into a space, so `$63,000` became the two
/// tokens `$63` and `000` — and `000` reads as the number zero. Two strikes
/// thousands apart therefore both reported the single number 0, which
/// [`compare_numbers`] scored as perfect agreement and *rewarded*.
///
/// The lookahead is why this one is a [`FancyRegex`]: it has to see the three
/// digits that make the comma a thousands separator without consuming them, or
/// `$1,250,000` would lose only its first comma.
///
/// The trailing `\b` of the original is written out as "a comma, a non-word
/// character, or the end", because `fancy-regex` will not take `(?-u:\b)` and
/// its Unicode `\b` would refuse `1,000漢` where JavaScript's ASCII-only one
/// accepts it.
static GROUPED_NUMBER: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(r"([0-9]),(?=[0-9]{3}(?:,|[^0-9A-Za-z_]|$))").expect("grouped number pattern")
});

static PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-z0-9.%$+<>≥≤]+").expect("punctuation pattern"));

/// A unit welded to its number is one token to a string comparison and two to a
/// reader: `25bps` has to meet `25 bps` somewhere. Split before folding phrases,
/// or `bps` in `25bps` is still inside a word when `\bbps\b` looks for it.
static DIGIT_THEN_LETTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([0-9])([a-z])").expect("digit/letter pattern"));

static LETTER_THEN_DIGIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([a-z])([0-9])").expect("letter/digit pattern"));

static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("whitespace"));

/// Apply a rewrite, and only pay for it if it fired.
///
/// `replace_all` hands back a `Cow`, and on a pattern that did not match that
/// `Cow` borrows the input. Taking ownership of it unconditionally therefore
/// copies the whole string to produce a value identical to the one already in
/// hand. [`normalise`] runs forty-odd of these passes and a typical title
/// trips one or two, so nearly every copy bought nothing — and [`tokenise`]
/// runs on both sides of every comparison the cross-venue board makes.
fn rewrite(out: &mut String, pattern: &Regex, replacement: &str) {
    if let Cow::Owned(rewritten) = pattern.replace_all(out, replacement) {
        *out = rewritten;
    }
}

/// Lower-case, strip accents and punctuation, collapse whitespace.
pub fn normalise(text: &str) -> String {
    let mut out = fold_diacritics(text).to_lowercase();

    for (pattern, replacement) in PUNCTUATED_PHRASES.iter() {
        rewrite(&mut out, pattern, replacement);
    }

    // Spelled out rather than routed through `rewrite`, because this pass needs
    // the lookahead only `fancy_regex` compiles, and that is a different type.
    if let Cow::Owned(rewritten) = GROUPED_NUMBER.replace_all(&out, "${1}") {
        out = rewritten;
    }

    rewrite(&mut out, &PUNCTUATION, " ");
    rewrite(&mut out, &DIGIT_THEN_LETTER, "${1} ${2}");
    rewrite(&mut out, &LETTER_THEN_DIGIT, "${1} ${2}");

    for (pattern, replacement) in PHRASES.iter() {
        rewrite(&mut out, pattern, replacement);
    }

    rewrite(&mut out, &WHITESPACE, " ");
    let trimmed = out.trim();
    if trimmed.len() != out.len() {
        out = trimmed.to_string();
    }
    out
}

/// A title's meaningful terms.
///
/// Plurals are folded (`rates` → `rate`) and synonyms applied, so the three
/// venues' phrasings converge before they are compared. Months become numbers
/// because `Oct 2026` and `October` are the same expiry to a trader and two
/// unrelated strings to a set intersection.
pub fn tokenise(text: &str) -> Vec<String> {
    let normalised = normalise(text);
    let mut out: Vec<String> = Vec::new();

    for word in normalised.split(' ') {
        if word.is_empty() {
            continue;
        }

        let bare = word.trim_matches(|c| c == '-' || c == '.');
        if bare.is_empty() || STOPWORDS.contains(bare) {
            continue;
        }

        if let Some(month) = MONTHS.get(bare) {
            out.push((*month).to_string());
            continue;
        }

        let singular = if bare.len() > 3 && bare.ends_with('s') && !bare.ends_with("ss") {
            &bare[..bare.len() - 1]
        } else {
            bare
        };

        match SYNONYMS.get(bare).or_else(|| SYNONYMS.get(singular)) {
            Some(folded) => out.push((*folded).to_string()),
            None => out.push(singular.to_string()),
        }
    }

    out
}

/// A token that states a quantity.
///
/// The currency mark and the percent sign are part of how a strike is written,
/// not part of the number: `$63000` and `3.75%` are quantities, and a matcher
/// that cannot read them cannot tell two rungs of a ladder apart. Requiring bare
/// digits meant every dollar strike and every rate strike was invisible to the
/// numeric comparison — so the Fed ladder, the case this module exists for,
/// was only ever compared as words.
static NUMERIC_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\$?-?[0-9]+(?:\.[0-9]+)?%?$").expect("numeric token pattern"));

fn is_numeric(token: &str) -> bool {
    NUMERIC_TOKEN.is_match(token)
}

/// The quantity a numeric token states, with its notation removed.
fn numeric_value(token: &str) -> f64 {
    js_number(&token.replace(['$', '%'], ""))
}

/// JavaScript's `Number(string)`, for the tokens [`is_numeric`] admits.
///
/// Only reaches a token whose notation has already been stripped, so it does
/// not in practice produce `NaN`. It keeps the fallback because the month
/// encoding shares it.
fn js_number(text: &str) -> f64 {
    text.parse::<f64>().unwrap_or(f64::NAN)
}

/// Whether a token is the *date* an exemplar event happens to carry.
///
/// Only a bare number can be: `$2026` is a price target and `2026%` is not a
/// year either. Reading the mark rather than only the digits is what keeps a
/// four-figure dollar strike out of the year filter.
///
/// Takes the quantity the caller has already read off the token, because the
/// only caller needs it either way and parsing it twice was pure waste.
fn is_calendar_year(token: &str, value: f64) -> bool {
    !token.contains('$') && !token.contains('%') && is_year(value)
}

/// A title's terms with its numbers taken out.
///
/// Dates dominate a title's token count and say nothing about what is being
/// asked: "Highest temperature in Oklahoma City on Aug 16, 2026?" is five parts
/// date and two parts question, so it scored 0.74 against Panama City. Numbers
/// are not discarded — [`weigh_numbers`] reads them separately, where a
/// disagreement can be judged on its own terms instead of being averaged into a
/// bag of words.
pub fn content_tokens(text: &str) -> Vec<String> {
    tokenise(text)
        .into_iter()
        .filter(|token| !is_numeric(token) && !is_month(token))
        .collect()
}

static KX_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^kx").expect("kx prefix"));

/// The `us` a Polymarket US identifier opens with, but only where a name follows.
///
/// The lookahead is why this one is a [`FancyRegex`]: `usfed-fomc` is the `fed`
/// book with a venue prefix, while an identifier that merely starts `us` and
/// then a digit is not.
static US_PREFIX: LazyLock<FancyRegex> =
    LazyLock::new(|| FancyRegex::new(r"^us(?=[a-z])").expect("us prefix"));

static NON_ALPHANUMERIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-z0-9]").expect("non-alphanumeric"));

static TRAILING_YEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(19|20)[0-9]{2}$").expect("trailing year"));

/// A venue identifier reduced to letters and digits.
///
/// Kalshi's `KXFEDDECISION`, Polymarket's `fed-decision` and Polymarket US's
/// `usfed-fomc` are the same name under three conventions; flattening them lets
/// one be tested for containment in another without a word list that could
/// split `FEDDECISION` in the first place.
pub fn identity_key(id: &str) -> String {
    let lowered = id.to_lowercase();
    let without_kx = KX_PREFIX.replace(&lowered, "").into_owned();
    let without_us = US_PREFIX.replace(&without_kx, "").into_owned();
    let flattened = NON_ALPHANUMERIC.replace_all(&without_us, "").into_owned();
    // A trailing season or year is part of the listing, not of the series.
    TRAILING_YEAR.replace(&flattened, "").into_owned()
}

/// Every number in a title, including years and strike levels.
///
/// Read off the *tokenised* form so a month named in words counts as its number:
/// `Oct 2026` and `October` have to agree on the month, and only tokenising
/// makes them comparable.
pub fn numbers_in(text: &str) -> Vec<f64> {
    title_numbers(text).all
}

fn is_year(n: f64) -> bool {
    n.is_finite() && n.fract() == 0.0 && (1900.0..=2100.0).contains(&n)
}

/// The numbers that distinguish one *series* from another, which is every
/// number except the year.
///
/// A series is named by its exemplar event, and that event's title carries a
/// date the series itself does not have: Kalshi's `Fed decision in Oct 2026?`
/// against Polymarket's `Fed Decision in September?` is one series seen at two
/// expiries. Everything else a title counts is part of its identity — `2nd
/// place` and `3rd place` are different questions, and so are `Best Picture` and
/// `Best Animated Feature`. Month *names* survive as words here, so only the
/// year has to be excluded.
///
/// The notation is stripped before the number is read, exactly as
/// [`numbers_in`] does it. Reading the token raw instead — which is what this
/// used to do — made every currency and percentage strike parse to `NaN`, so
/// `$63,000` and `$71,000` reported the same non-number and were rewarded for
/// agreeing on it.
pub fn significant_numbers(text: &str) -> Vec<f64> {
    title_numbers(text).significant
}

/// Both readings of a title's numbers, off one tokenisation.
///
/// [`numbers_in`] and [`significant_numbers`] differ only in whether a year
/// counts, and [`prepare`] wants both. Reading them together also stops each
/// numeric token being parsed twice — once to ask whether it is a year and once
/// to record what it is.
struct TitleNumbers {
    /// Every number, months included. What [`score_event`] weighs.
    all: Vec<f64>,
    /// Every number except the calendar year. What [`score_series`] weighs.
    significant: Vec<f64>,
}

fn title_numbers(text: &str) -> TitleNumbers {
    let mut all: Vec<f64> = Vec::new();
    let mut significant: Vec<f64> = Vec::new();

    for token in tokenise(text) {
        if is_month(&token) {
            all.push(js_number(&token[1..]));
        } else if is_numeric(&token) {
            let value = numeric_value(&token);
            all.push(value);
            if !is_calendar_year(&token, value) {
                significant.push(value);
            }
        }
    }

    TitleNumbers { all, significant }
}

/* ---------------------------------------------------------------- scoring */

/// An insertion-ordered set of tokens, which is what a JavaScript `Set` is.
///
/// The order is part of the output, not an implementation detail: the reason
/// line quotes the first three lopsided qualifiers it finds, and a hash set
/// would hand a different three on every run.
#[derive(Debug, Default, Clone)]
struct TokenSet {
    order: Vec<String>,
    index: HashSet<String>,
}

impl TokenSet {
    fn insert(&mut self, token: &str) {
        if self.index.insert(token.to_string()) {
            self.order.push(token.to_string());
        }
    }

    fn contains(&self, token: &str) -> bool {
        self.index.contains(token)
    }

    fn len(&self) -> usize {
        self.order.len()
    }

    fn iter(&self) -> std::slice::Iter<'_, String> {
        self.order.iter()
    }
}

impl<'a> FromIterator<&'a String> for TokenSet {
    fn from_iter<I: IntoIterator<Item = &'a String>>(tokens: I) -> Self {
        let mut set = TokenSet::default();
        for token in tokens {
            set.insert(token);
        }
        set
    }
}

/// How many terms two sets state in common.
fn shared_count(a: &TokenSet, b: &TokenSet) -> usize {
    a.iter().filter(|term| b.contains(term)).count()
}

/// How much two token sets have in common, allowing for one being terser.
///
/// Dice alone — `2|A∩B| / (|A|+|B|)` — punishes a short title for being short,
/// and the venues are wildly asymmetric about length. Kalshi writes
/// "Bank of Japan rate decision in September" where Polymarket US writes "BoJ
/// Decision"; Kalshi writes "Emmy Winner: Outstanding Lead Actor in a Comedy
/// Series" where Polymarket US writes "Lead Actor, Comedy". Both pairs are
/// *identical questions* that Dice scores in the 0.5s purely on word count, and
/// that is where the terser side is usually the one carrying the meaning.
///
/// The overlap coefficient — `|A∩B| / min(|A|,|B|)` — has the opposite flaw: it
/// reads any subset as a perfect match, so "NFL Champion" would score 1.0
/// against "NFL Champion Rookie of the Year". Blending the two keeps Dice's
/// scepticism about loose subsets while letting a genuinely terser title compete.
///
/// Takes the count rather than the sets because [`could_reach_floor`] has to
/// reproduce this arithmetic exactly — a prefilter that rounds differently from
/// the thing it filters for would silently drop matches — and the only way to
/// guarantee that is for both to be the same code.
fn overlap_of(shared: usize, a: usize, b: usize) -> f64 {
    if a == 0 || b == 0 {
        return 0.0;
    }

    let coefficient = shared as f64 / a.min(b) as f64;
    let dice = (2.0 * shared as f64) / (a + b) as f64;
    0.6 * dice + 0.4 * coefficient
}

/// What a venue offers the matcher about one series.
#[derive(Debug, Clone, Copy)]
pub struct SeriesDescriptor<'a> {
    /// The venue's own identifier: `KXFEDDECISION`, `fomc`, `usfed-fomc`.
    pub id: &'a str,
    /// The series' title, or a representative event's.
    pub title: &'a str,
    /// Anything else worth matching on — a category, a sample strike label.
    pub context: Option<&'a str>,
}

impl<'a> SeriesDescriptor<'a> {
    pub fn new(id: &'a str, title: &'a str) -> Self {
        Self {
            id,
            title,
            context: None,
        }
    }

    pub fn with_context(mut self, context: &'a str) -> Self {
        self.context = Some(context);
        self
    }
}

#[derive(Debug, Clone)]
pub struct MatchScore {
    /// 0..1.
    pub score: f64,
    pub confidence: MatchConfidence,
    /// Terms both sides carried, in the order the first side stated them.
    pub shared: Vec<String>,
    /// One line naming why, for the panel to print verbatim.
    pub reason: String,
}

/// Words that carve a family of markets into its members.
///
/// These are the opposite of a shared term: when one side says `supporting` and
/// the other does not, the six words they agree on stop mattering, because
/// "Best Actor" and "Best Supporting Actor" are two awards with two winners.
/// Set overlap cannot see that — one word out of six barely moves a Dice
/// coefficient — so presence on exactly one side is scored in its own right.
///
/// Only tokens whose absence is *meaningful* belong here. `high` qualifies
/// because every temperature market on every venue states whether it is the
/// day's high or its low, so a title that omits it is not a title about
/// temperature at all. A word that one venue simply happens to leave implicit
/// would cause a correct pair to be rejected, and does not belong.
///
/// Written in post-[`tokenise`] form: `highest` arrives here as `high`.
const QUALIFIERS: &[&str] = &[
    // Award categories, where the qualifier *is* the category. The Emmys alone
    // run twenty of these, differing by three words and nothing else.
    "supporting",
    "lead",
    "guest",
    "animated",
    "documentary",
    "adapted",
    "original",
    "comedy",
    "drama",
    "variety",
    "anthology",
    "actor",
    "actress",
    // Temperature markets, which always state their direction.
    "hightemp",
    "lowtemp",
];

/// Places and offices, which are the whole question in an election market.
///
/// "South Carolina Senate winner?" and "South Dakota Senate election winner"
/// share three words out of four; so do "Minnesota Senate winner?" and
/// "Minnesota Governor winner". A bag of words cannot see that `carolina` and
/// `dakota` — or `senate` and `governor` — are the entire distinction, and the
/// board is nine-tenths election markets, so it gets these wrong at scale.
///
/// Only the distinctive word of each state is listed: `carolina` tells North
/// from South once `north`/`south` are themselves discriminating.
static PLACES_AND_OFFICES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // Distinctive words of US state names.
        "alabama",
        "alaska",
        "arizona",
        "arkansas",
        "california",
        "colorado",
        "connecticut",
        "delaware",
        "florida",
        "georgia",
        "hawaii",
        "idaho",
        "illinois",
        "indiana",
        "iowa",
        "kansas",
        "kentucky",
        "louisiana",
        "maine",
        "maryland",
        "massachusetts",
        "michigan",
        "minnesota",
        "mississippi",
        "missouri",
        "montana",
        "nebraska",
        "nevada",
        "hampshire",
        "jersey",
        "mexico",
        "york",
        "carolina",
        "dakota",
        "ohio",
        "oklahoma",
        "oregon",
        "pennsylvania",
        "rhode",
        "tennessee",
        "texas",
        "utah",
        "vermont",
        "virginia",
        "washington",
        "wisconsin",
        "wyoming",
        // Compass words, which are what separate one Carolina, Dakota or Virginia
        // from the other.
        "north",
        "south",
        "east",
        "west",
        // The office being contested.
        "senate",
        "house",
        "governor",
        "president",
        "mayor",
        "parliament",
        "congress",
        "attorney",
        "chair",
        "nominee",
        "primary",
    ]
    .into_iter()
    .collect()
});

/// A token that carves a family into members.
///
/// Central bank identities qualify by construction: every rate book on the board
/// says "bank", "rate" and "decision", so the bank's own name is the only part
/// that distinguishes eleven otherwise identical titles.
fn is_qualifier(token: &str) -> bool {
    QUALIFIERS.contains(&token) || PLACES_AND_OFFICES.contains(token) || token.starts_with("bankof")
}

/// Applied once per lopsided qualifier, so two of them compound.
const QUALIFIER_PENALTY: f64 = 0.35;

/// Where a text score stops being worth showing at all.
pub const MATCH_FLOOR: f64 = 0.34;

pub fn confidence_of(score: f64) -> MatchConfidence {
    if score >= 0.8 {
        MatchConfidence::Strong
    } else if score >= 0.6 {
        MatchConfidence::Likely
    } else {
        MatchConfidence::Weak
    }
}

/// Everything about one listing that can be worked out without seeing the other.
///
/// [`similarity`] and the number weighing between them tokenise four times and
/// build two identity keys for every comparison, and every one of those is a
/// function of a single side. At catalogue scale that *is* the cost: the
/// cross-venue board scores millions of pairs, and each anchor's half was being
/// rebuilt once per candidate it was offered. Deriving it once and handing the
/// same value to every comparison turns that work into a lookup.
///
/// Nothing here is a judgement — no field depends on what the other side says.
/// That is the whole invariant, and it is what makes preparing once safe.
#[derive(Debug, Clone)]
pub struct Prepared {
    /// The identifier, kept because a reason line quotes it verbatim.
    id: String,
    /// Content terms of the title *and* its context, deduplicated, in the order
    /// the listing stated them — which is the order a reason line reads back.
    tokens: TokenSet,
    /// Those of `tokens` that are qualifiers, in the same order.
    qualifiers: Vec<String>,
    /// How many of `tokens` are *not* qualifiers.
    substance: usize,
    /// [`significant_numbers`] of the title, which is what [`score_series`] weighs.
    significant: Vec<f64>,
    /// [`numbers_in`] of the title, which is what [`score_event`] weighs.
    numbers: Vec<f64>,
    /// [`identity_key`] of the identifier.
    key: String,
}

/// Derive one listing's half of a comparison.
pub fn prepare(d: &SeriesDescriptor) -> Prepared {
    let joined = format!("{} {}", d.title, d.context.unwrap_or(""));
    let tokens: TokenSet = content_tokens(&joined).iter().collect();
    let title = title_numbers(d.title);

    let qualifiers: Vec<String> = tokens
        .iter()
        .filter(|term| is_qualifier(term))
        .cloned()
        .collect();
    let substance = tokens.len() - qualifiers.len();

    Prepared {
        id: d.id.to_string(),
        tokens,
        qualifiers,
        substance,
        significant: title.significant,
        numbers: title.all,
        key: identity_key(d.id),
    }
}

/// What two identifiers say about each other.
///
/// Corroboration rather than evidence: when two exchanges independently arrive
/// at `fed-decision` and `feddecision`, that is worth more than any single
/// shared word. The length floors keep a two-letter stub from matching the
/// world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyRelation {
    Identical,
    Contains,
    Unrelated,
}

fn key_relation(a: &str, b: &str) -> KeyRelation {
    if a.len() >= 4 && a == b {
        KeyRelation::Identical
    } else if a.len() >= 5 && b.len() >= 5 && (a.contains(b) || b.contains(a)) {
        KeyRelation::Contains
    } else {
        KeyRelation::Unrelated
    }
}

/// How alike two listings read, before any arithmetic on the numbers in them.
///
/// Titles carry the signal, so they carry the weight; the identifiers are a
/// corroborator. What counts as a *disagreeing* number differs between a series
/// and an expiry, so that judgement belongs to the two callers below, and each
/// applies it exactly once.
fn similarity(a: &Prepared, b: &Prepared) -> MatchScore {
    let mut shared: Vec<String> = Vec::new();
    let mut shared_substance = 0usize;
    for term in a.tokens.iter() {
        if b.tokens.contains(term) {
            if !is_qualifier(term) {
                shared_substance += 1;
            }
            shared.push(term.clone());
        }
    }

    let mut score = overlap_of(shared.len(), a.tokens.len(), b.tokens.len());

    let relation = key_relation(&a.key, &b.key);
    let identical = relation == KeyRelation::Identical;
    let contains = relation == KeyRelation::Contains;

    if identical {
        score = score * 0.6 + 0.4;
    } else if contains {
        score = score * 0.75 + 0.25;
    }

    // Agreeing only on a qualifier is not agreement. "Highest temperature in
    // Oklahoma City" and "Highest temperature in Panama City" share nothing but
    // the word that says which direction the thermometer is read in — the place,
    // which is the entire question, is the part they differ on.
    // …but only where there was something else to agree on. A terse label like
    // "Lead Actor, Comedy" is *made* of qualifiers, and has nothing else to offer.
    let both_have_substance = a.substance > 0 && b.substance > 0;
    if both_have_substance && shared_substance == 0 && !identical && !contains {
        score *= 0.4;
    }

    // Two titles that agree on one common word and nothing else are not a match,
    // however that word scored: "NFL Champion" and "NFL Rookie of the Year".
    // Only applied where there was room to share more — a two-word contract label
    // ("No change") has one word to offer, and offering it is not weak evidence.
    let room_to_share = a.tokens.len() + b.tokens.len() >= 5;
    if shared.len() < 2 && room_to_share && !identical && !contains {
        score *= 0.5;
    }

    let mut reason = if identical {
        format!("both listed as \"{}\"", a.key)
    } else if contains {
        format!("identifiers overlap ({} / {})", a.id, b.id)
    } else if !shared.is_empty() {
        format!("shared terms: {}", join_first(&shared, 4, ", "))
    } else {
        "no shared terms".to_string()
    };

    // A qualifier on one side and not the other is the whole question, whatever
    // the rest of the words did. Read off each side's own qualifiers rather than
    // their union: a qualifier missing from the right cannot also be missing
    // from the left, so the two passes cover the union without building it.
    let lopsided: Vec<String> = a
        .qualifiers
        .iter()
        .filter(|q| !b.tokens.contains(q))
        .chain(b.qualifiers.iter().filter(|q| !a.tokens.contains(q)))
        .cloned()
        .collect();
    if !lopsided.is_empty() {
        score *= QUALIFIER_PENALTY.powi(lopsided.len() as i32);
        reason.push_str(&format!(
            "; only one side says {}",
            join_first(&lopsided, 3, "/")
        ));
    }

    score = score.min(1.0);
    MatchScore {
        score,
        confidence: confidence_of(score),
        shared,
        reason,
    }
}

/// The most the number weighing can ever multiply a score by.
///
/// [`weigh_numbers`] judges years and everything else separately and multiplies
/// the two verdicts, so a pair that agrees on both collects the reward twice.
/// `score_series` never does — it is handed [`significant_numbers`], which has
/// no years in it — but a single ceiling that holds for both scorers is one
/// fewer thing for [`could_reach_floor`] to be wrong about.
const MAX_NUMBER_REWARD: f64 = 1.12 * 1.12;

/// Whether a pair could possibly clear `floor`, without scoring it.
///
/// Every step of [`similarity`] after the overlap either multiplies the score
/// downward or caps it; only two ever raise it, and both are bounded — the
/// identity-key boost, and [`MAX_NUMBER_REWARD`]. So the best a pair could do is
/// computable from the two prepared halves alone, and a pair whose best is below
/// the floor cannot reach it however the rest of the arithmetic falls out.
///
/// This is exact, not a heuristic: a caller that discards sub-floor pairs gets
/// the same set whether it calls this first or not. It exists because the
/// cross-venue board offers the scorer millions of pairs that share one
/// incidental word, and rejecting those on a token intersection is two orders of
/// magnitude cheaper than rejecting them on a full score.
#[must_use]
pub fn could_reach_floor(a: &Prepared, b: &Prepared, floor: f64) -> bool {
    let base = overlap_of(
        shared_count(&a.tokens, &b.tokens),
        a.tokens.len(),
        b.tokens.len(),
    );
    let best = match key_relation(&a.key, &b.key) {
        KeyRelation::Identical => base * 0.6 + 0.4,
        KeyRelation::Contains => base * 0.75 + 0.25,
        KeyRelation::Unrelated => base,
    };
    best.min(1.0) * MAX_NUMBER_REWARD >= floor
}

fn join_first(items: &[String], limit: usize, separator: &str) -> String {
    items[..items.len().min(limit)].join(separator)
}

/// Reward agreeing numbers, punish disagreeing ones.
///
/// Set overlap cannot see that the one word two titles differ on is the whole
/// question: "Big Brother season 28, 2nd place" and "…3rd place" share five
/// words out of six. Disagreement is halved rather than zeroed, because one side
/// may simply be counting something the other does not mention.
fn weigh_numbers(base: MatchScore, left: &[f64], right: &[f64]) -> MatchScore {
    // Years and everything else answer different questions, and mixing them lets
    // one cover for the other: "Big Brother Season 28, 2nd place" and "…3rd
    // place" agree on the season and differ on the only number that is the
    // question. Judged together, the 28 hides the 2-against-3.
    let years = compare_numbers(&filter_numbers(left, true), &filter_numbers(right, true));
    let rest = compare_numbers(&filter_numbers(left, false), &filter_numbers(right, false));

    let factor = years.factor * rest.factor;
    if factor == 1.0 {
        return base;
    }

    let score = (base.score * factor).min(1.0);
    let notes: Vec<&str> = [years.note.as_str(), rest.note.as_str()]
        .into_iter()
        .filter(|note| !note.is_empty())
        .collect();
    let reason = if notes.is_empty() {
        base.reason
    } else {
        format!("{}; {}", base.reason, notes.join(", "))
    };

    MatchScore {
        score,
        confidence: confidence_of(score),
        shared: base.shared,
        reason,
    }
}

fn filter_numbers(numbers: &[f64], years: bool) -> Vec<f64> {
    numbers
        .iter()
        .copied()
        .filter(|n| is_year(*n) == years)
        .collect()
}

struct NumberVerdict {
    factor: f64,
    note: String,
}

/// Weigh one class of number against its counterpart.
///
/// A side that states none is silent rather than contradictory — Polymarket
/// writes "Fed Decision in October" where Kalshi writes "Fed decision in Oct
/// 2026?", and the missing year is an omission, not a disagreement.
fn compare_numbers(left: &[f64], right: &[f64]) -> NumberVerdict {
    if left.is_empty() || right.is_empty() {
        return NumberVerdict {
            factor: 1.0,
            note: String::new(),
        };
    }

    // A number nobody could read matches nothing — not even another unreadable
    // one. Two unknowns are two unknowns, and treating them as equal is how a
    // pair of currency-marked strikes used to collect the agreement reward.
    let right_set: HashSet<u64> = right
        .iter()
        .filter(|n| n.is_finite())
        .map(quantity)
        .collect();
    let left_set: HashSet<u64> = left
        .iter()
        .filter(|n| n.is_finite())
        .map(quantity)
        .collect();
    let only_left: Vec<f64> = left
        .iter()
        .copied()
        .filter(|n| !n.is_finite() || !right_set.contains(&quantity(n)))
        .collect();
    let only_right: Vec<f64> = right
        .iter()
        .copied()
        .filter(|n| !n.is_finite() || !left_set.contains(&quantity(n)))
        .collect();

    if only_left.is_empty() && only_right.is_empty() {
        return NumberVerdict {
            factor: 1.12,
            note: String::new(),
        };
    }

    let agreed = left.len() - only_left.len();
    let differing = format!(
        "{} vs {}",
        name_numbers(&only_left),
        name_numbers(&only_right)
    );
    // Partial agreement is not agreement, but it is not a flat contradiction
    // either: one side may simply count something the other leaves unsaid.
    NumberVerdict {
        factor: if agreed > 0 { 0.6 } else { 0.4 },
        note: format!("numbers differ ({differing})"),
    }
}

/// The key two quantities are equal under.
///
/// Only ever called on a finite number, so the one normalisation it owes is
/// `-0.0`: a fall of zero and a rise of zero are the same figure, and they do
/// not share a bit pattern.
fn quantity(n: &f64) -> u64 {
    if *n == 0.0 {
        0f64.to_bits()
    } else {
        n.to_bits()
    }
}

fn name_numbers(numbers: &[f64]) -> String {
    if numbers.is_empty() {
        return "—".to_string();
    }
    numbers[..numbers.len().min(2)]
        .iter()
        .map(|n| format_number(*n))
        .collect::<Vec<_>>()
        .join("/")
}

/// How JavaScript prints a number into the reason line.
fn format_number(n: f64) -> String {
    if n.is_nan() {
        "NaN".to_string()
    } else if n == 0.0 {
        "0".to_string()
    } else if n.is_infinite() {
        if n > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        }
    } else {
        format!("{n}")
    }
}

/// Score two venues' series against each other.
///
/// Years are ignored, because a series recurs: `KXFEDDECISION` is the same
/// series whether its next event is October's or January's, and its exemplar
/// event's title says so. Every other number counts — a "2nd place" market and a
/// "3rd place" market are two questions however alike they read.
pub fn score_series(a: &SeriesDescriptor, b: &SeriesDescriptor) -> MatchScore {
    score_series_prepared(&prepare(a), &prepare(b))
}

/// [`score_series`] on two halves already derived.
#[must_use]
pub fn score_series_prepared(a: &Prepared, b: &Prepared) -> MatchScore {
    weigh_numbers(similarity(a, b), &a.significant, &b.significant)
}

/// Score two specific events, or two contracts — one expiry against another.
///
/// Same reading as [`score_series`], except that here the year is decisive too.
/// October's Fed meeting and January's share every word in their titles, so
/// without counting the date the terminal would happily quote one against the
/// other.
pub fn score_event(a: &SeriesDescriptor, b: &SeriesDescriptor) -> MatchScore {
    score_event_prepared(&prepare(a), &prepare(b))
}

/// [`score_event`] on two halves already derived.
#[must_use]
pub fn score_event_prepared(a: &Prepared, b: &Prepared) -> MatchScore {
    weigh_numbers(similarity(a, b), &a.numbers, &b.numbers)
}

/// One rung of a ladder married to one rung of another venue's.
///
/// The two sides are *positions* rather than the labels themselves, because a
/// ladder can list the same wording twice and because the caller needs its own
/// row back, not a copy of the string it passed in. It is also what keeps the
/// leftovers reportable: whichever indices [`pair_labels`] does not return are
/// exactly the rungs one venue lists and the other does not.
#[derive(Debug, Clone)]
pub struct LabelPair {
    /// Index into the `left` slice.
    pub left: usize,
    /// Index into the `right` slice.
    pub right: usize,
    pub matched: MatchScore,
}

/// Pair up two venues' contracts within one event.
///
/// Greedy, best-first: the strongest pair is taken, both sides are struck off,
/// and the next strongest is considered. A ladder is a set of near-identical
/// labels ("25 bps decrease", "50+ bps decrease"), so pairing each label with
/// its own best partner independently would happily map two of one venue's rungs
/// onto one of the other's.
pub fn pair_labels<L: AsRef<str>, R: AsRef<str>>(left: &[L], right: &[R]) -> Vec<LabelPair> {
    pair_labels_above(left, right, MATCH_FLOOR)
}

/// [`pair_labels`] with the floor named, for a caller that wants a stricter one.
pub fn pair_labels_above<L: AsRef<str>, R: AsRef<str>>(
    left: &[L],
    right: &[R],
    floor: f64,
) -> Vec<LabelPair> {
    let mut candidates: Vec<LabelPair> = Vec::new();

    for (l, left_label) in left.iter().enumerate() {
        for (r, right_label) in right.iter().enumerate() {
            let l_label = left_label.as_ref();
            let r_label = right_label.as_ref();
            let matched = score_event(
                &SeriesDescriptor::new(l_label, l_label),
                &SeriesDescriptor::new(r_label, r_label),
            );
            if matched.score >= floor {
                candidates.push(LabelPair {
                    left: l,
                    right: r,
                    matched,
                });
            }
        }
    }

    candidates.sort_by(|x, y| {
        y.matched
            .score
            .partial_cmp(&x.matched.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut used_left: HashSet<usize> = HashSet::new();
    let mut used_right: HashSet<usize> = HashSet::new();
    let mut paired: Vec<LabelPair> = Vec::new();

    for candidate in candidates {
        if used_left.contains(&candidate.left) || used_right.contains(&candidate.right) {
            continue;
        }
        used_left.insert(candidate.left);
        used_right.insert(candidate.right);
        paired.push(candidate);
    }

    paired
}

/* ------------------------------------------------------------------ tests */

/// Cross-venue matching tests.
///
/// This is the part of the terminal whose output looks plausible when it is
/// wrong: pairing two brokers' listings incorrectly still produces a tidy table
/// of two prices, and a trader will read the gap between them as an edge. So the
/// cases here are mostly the near misses — two questions that share almost every
/// word and are not the same question.
///
/// Every string is one a venue actually publishes.
#[cfg(test)]
mod tests {
    use super::*;

    fn d<'a>(id: &'a str, title: &'a str) -> SeriesDescriptor<'a> {
        SeriesDescriptor::new(id, title)
    }

    /* --------------------------------------------------------- tokenising */

    #[test]
    fn normalise_splits_a_unit_welded_to_its_number() {
        // Kalshi writes `25bps`, Polymarket writes `25 bps`.
        assert_eq!(normalise("Cut 25bps"), "cut 25 bp");
    }

    #[test]
    fn normalise_folds_the_phrases_three_venues_use_for_one_rung() {
        assert_eq!(normalise("Fed maintains rate"), "fed nochange");
        assert_eq!(normalise("No change"), "nochange");
        assert_eq!(normalise("Unchanged"), "nochange");
    }

    #[test]
    fn normalise_keeps_a_comparator_which_is_what_separates_two_rungs() {
        assert!(normalise("Cut >25bps").contains("over"));
        assert!(normalise("50+ bps decrease").contains("over"));
        assert!(normalise("3.75% or below").contains("under"));
    }

    #[test]
    fn tokenise_drops_question_scaffolding() {
        assert_eq!(
            tokenise("Will the Fed cut rates?"),
            vec!["fed", "decrease", "rate"]
        );
    }

    #[test]
    fn tokenise_folds_month_names_to_numbers_so_oct_and_october_agree() {
        assert_eq!(
            tokenise("Fed decision in Oct 2026?"),
            vec!["fed", "decision", "m10", "2026"]
        );
        assert_eq!(
            tokenise("Fed Decision in October?"),
            vec!["fed", "decision", "m10"]
        );
    }

    #[test]
    fn tokenise_folds_the_venues_synonyms_onto_one_word() {
        assert_eq!(tokenise("FOMC"), vec!["fed"]);
        assert_eq!(tokenise("BTC"), vec!["bitcoin"]);
        assert_eq!(tokenise("GOP nominee"), vec!["republican", "nominee"]);
    }

    #[test]
    fn identity_key_flattens_three_naming_conventions_onto_one_string() {
        assert_eq!(identity_key("KXFEDDECISION"), "feddecision");
        assert_eq!(identity_key("fed-decision"), "feddecision");
        assert_eq!(identity_key("usfed-fomc"), "fedfomc");
    }

    #[test]
    fn identity_key_drops_a_trailing_season_year_which_names_the_listing_not_the_series() {
        assert_eq!(identity_key("mlb-2026"), "mlb");
        assert_eq!(identity_key("nfl-2026"), "nfl");
    }

    #[test]
    fn numbers_in_reads_every_number_months_and_years_included() {
        // The month counts for an event: October's Fed meeting is not January's.
        assert_eq!(numbers_in("Fed decision in Oct 2026?"), vec![10.0, 2026.0]);
        assert_eq!(numbers_in("Cut 25bps"), vec![25.0]);
    }

    #[test]
    fn significant_numbers_drops_the_date_when_comparing_series_because_a_series_recurs() {
        // Neither the month nor the year distinguishes one series from another —
        // both are properties of whichever event happens to be next.
        assert_eq!(
            significant_numbers("Fed decision in Oct 2026?"),
            Vec::<f64>::new()
        );
        assert_eq!(
            significant_numbers("Fed Decision in September?"),
            Vec::<f64>::new()
        );
        assert_eq!(
            significant_numbers("Big Brother Season 28 · 2nd place"),
            vec![28.0, 2.0]
        );
    }

    /* ------------------------------------------------------------ scoring */

    #[test]
    fn score_series_matches_the_same_series_across_three_naming_conventions() {
        let kalshi = d("KXNOBELPEACE", "2026 Nobel Peace Prize winner");
        let poly = d(
            "nobel-peace-prize-winner-2026",
            "Nobel Peace Prize Winner 2026",
        );
        let us = d("nobel-peace", "Nobel Peace Prize Winner");

        assert_eq!(
            score_series(&kalshi, &poly).confidence,
            MatchConfidence::Strong
        );
        assert_eq!(
            score_series(&kalshi, &us).confidence,
            MatchConfidence::Strong
        );
    }

    #[test]
    fn score_series_ignores_the_expiry_so_one_series_matches_across_its_events() {
        // Both are the FOMC series; they simply have different next meetings.
        let a = d("KXFEDDECISION", "Fed decision in Oct 2026?");
        let b = d("usfed-fomc", "Fed Decision in December");
        assert!(score_series(&a, &b).score >= MATCH_FLOOR);
    }

    #[test]
    fn score_series_separates_two_rungs_that_differ_only_by_a_number() {
        // The classic false positive: five words out of six in common.
        let second = d("KXBIGBROTHERRANK", "Big Brother Season 28 · 2nd place");
        let third = d(
            "big-brother-season-28-3rd-place",
            "Big Brother Season 28 3rd place",
        );
        assert_eq!(
            score_series(&second, &third).confidence,
            MatchConfidence::Weak
        );
        assert!(score_series(&second, &third)
            .reason
            .contains("numbers differ"));
    }

    #[test]
    fn score_series_does_not_pair_two_markets_that_share_one_common_word() {
        let a = d("KXNFLCHAMP", "NFL Champion");
        let b = d("nfl-rookie-of-the-year", "NFL Rookie of the Year");
        assert!(score_series(&a, &b).score < MATCH_FLOOR);
    }

    #[test]
    fn score_series_rewards_identifiers_that_independently_agree() {
        // Same two titles, worded differently enough that the text alone is not
        // conclusive; the identifiers are what settle it.
        let title_a = "Measles cases in 2026?";
        let title_b = "How many measles cases will be reported?";
        let bare = score_series(&d("A", title_a), &d("B", title_b));
        let named = score_series(&d("KXMEASLES", title_a), &d("measles", title_b));
        assert!(
            named.score > bare.score,
            "{} should beat {}",
            named.score,
            bare.score
        );
        assert!(named.reason.contains("both listed as \"measles\""));
    }

    #[test]
    fn score_series_says_why_in_terms_a_panel_can_print() {
        let matched = score_series(
            &d("SENATENC", "North Carolina Senate winner?"),
            &d("usse-nc", "North Carolina Senate Election Winner"),
        );
        assert!(matched.shared.iter().any(|t| t == "senate"));
        assert!(matched.reason.contains("shared terms"));
    }

    #[test]
    fn score_event_keeps_two_expiries_of_one_series_apart() {
        let october = d("KXFEDDECISION-26OCT", "Fed decision in Oct 2026?");
        let january = d("KXFEDDECISION-27JAN", "Fed decision in Jan 2027?");
        let matched = score_event(&october, &january);
        assert_eq!(matched.confidence, MatchConfidence::Weak);
        assert!(matched.reason.contains("numbers differ"));
    }

    #[test]
    fn score_event_pairs_the_same_expiry_worded_two_ways() {
        let matched = score_event(
            &d("KXFEDDECISION-26OCT", "Fed decision in Oct 2026?"),
            &d("usfed-fomc-2026-10-28", "Fed Decision in October"),
        );
        assert_eq!(matched.confidence, MatchConfidence::Strong);
    }

    #[test]
    fn score_event_penalises_a_disagreeing_year_which_score_series_deliberately_would_not() {
        let a = d("a", "Serie A 2026 Champion");
        let b = d("b", "Serie A 2027 Champion");
        assert!(score_event(&a, &b).score < score_series(&a, &b).score);
    }

    #[test]
    fn score_event_applies_its_number_penalty_once_not_twice() {
        // score_event builds on the same text reading as score_series; if both
        // applied their own penalty, a rung would be quartered and fall through the
        // floor even where the pairing is right.
        let matched = score_event(&d("a", "Cut >25bps"), &d("b", "50+ bps decrease"));
        assert!(
            matched.score >= MATCH_FLOOR,
            "expected >= {MATCH_FLOOR}, got {}",
            matched.score
        );
    }

    #[test]
    fn confidence_of_grades_the_bands() {
        assert_eq!(confidence_of(0.95), MatchConfidence::Strong);
        assert_eq!(confidence_of(0.7), MatchConfidence::Likely);
        assert_eq!(confidence_of(0.4), MatchConfidence::Weak);
    }

    /* ------------------------------------------------------------ ladders */

    // The real October FOMC ladder, as each venue words it.
    const KALSHI_LADDER: &[&str] = &[
        "Cut >25bps",
        "Cut 25bps",
        "Fed maintains rate",
        "Hike 25bps",
        "Hike >25bps",
    ];
    const POLY_LADDER: &[&str] = &[
        "50+ bps decrease",
        "25 bps decrease",
        "No change",
        "25 bps increase",
        "50+ bps increase",
    ];

    #[test]
    fn pair_labels_lines_a_five_rung_ladder_up_across_two_venues() {
        let paired = pair_labels(KALSHI_LADDER, POLY_LADDER);
        let map: HashMap<&str, &str> = paired
            .iter()
            .map(|p| (KALSHI_LADDER[p.left], POLY_LADDER[p.right]))
            .collect();

        assert_eq!(map.get("Cut 25bps"), Some(&"25 bps decrease"));
        assert_eq!(map.get("Hike 25bps"), Some(&"25 bps increase"));
        assert_eq!(map.get("Fed maintains rate"), Some(&"No change"));
        assert_eq!(map.get("Cut >25bps"), Some(&"50+ bps decrease"));
        assert_eq!(map.get("Hike >25bps"), Some(&"50+ bps increase"));
    }

    #[test]
    fn pair_labels_never_maps_two_rungs_of_one_ladder_onto_the_same_rung_of_the_other() {
        let paired = pair_labels(KALSHI_LADDER, POLY_LADDER);
        let rights: Vec<usize> = paired.iter().map(|p| p.right).collect();
        assert_eq!(rights.iter().collect::<HashSet<_>>().len(), rights.len());
    }

    #[test]
    fn pair_labels_leaves_a_rung_with_no_counterpart_unpaired_rather_than_forcing_one() {
        let paired = pair_labels(KALSHI_LADDER, &["No change"]);
        assert_eq!(paired.len(), 1);
        assert_eq!(KALSHI_LADDER[paired[0].left], "Fed maintains rate");
    }

    #[test]
    fn pair_labels_pairs_nothing_when_the_ladders_share_no_wording() {
        assert!(pair_labels(KALSHI_LADDER, &["Gavin Newsom"]).is_empty());
    }

    /* --------------------------------------------------------- qualifiers */

    #[test]
    fn qualifiers_keeps_an_award_apart_from_its_sub_category() {
        // Six words in common and one that decides the whole question. Dice alone
        // scored this 0.73 — comfortably inside the band the panel calls LIKELY.
        let matched = score_series(
            &d("KXOSCARACTO", "Oscar Winner: Best Actor"),
            &d(
                "oscars-2027-best-supporting-actor-winner",
                "Oscars 2027 Best Supporting Actor Winner",
            ),
        );
        assert_eq!(matched.confidence, MatchConfidence::Weak);
        assert!(matched.reason.contains("only one side says supporting"));
    }

    #[test]
    fn qualifiers_keeps_best_picture_apart_from_best_animated_feature() {
        let matched = score_series(
            &d(
                "KXOSCARANIMATED",
                "Oscar Winner: Best Animated Feature Film",
            ),
            &d("oscars-bestpic", "Oscar Winner: Best Picture"),
        );
        assert!(
            matched.score < MATCH_FLOOR,
            "expected < {MATCH_FLOOR}, got {}",
            matched.score
        );
    }

    #[test]
    fn qualifiers_keeps_a_daily_high_apart_from_a_daily_low() {
        let matched = score_series(
            &d("KXHIGHMIA", "Highest temperature in Miami on Aug 16, 2026?"),
            &d(
                "miami-daily-lowest-temperature",
                "Lowest temperature in Miami on August 16?",
            ),
        );
        assert!(
            matched.score < MATCH_FLOOR,
            "expected < {MATCH_FLOOR}, got {}",
            matched.score
        );
    }

    #[test]
    fn qualifiers_still_pairs_two_venues_that_both_state_the_same_direction() {
        // The qualifier rule must not cost us the matches that already work: both
        // sides say "highest", so nothing is lopsided.
        let matched = score_series(
            &d("KXHIGHMIA", "Highest temperature in Miami on Aug 16, 2026?"),
            &d(
                "miami-daily-weather",
                "Highest temperature in Miami on August 16?",
            ),
        );
        assert_eq!(matched.confidence, MatchConfidence::Strong);
    }

    #[test]
    fn qualifiers_does_not_fire_on_a_word_neither_side_qualifies() {
        let matched = score_series(
            &d("KXMEASLES", "Measles cases in 2026?"),
            &d("measles", "Measles cases in 2026"),
        );
        assert_eq!(matched.confidence, MatchConfidence::Strong);
        assert!(!matched.reason.contains("only one side"));
    }

    /* ----------------------------------------------- sibling award categories */

    // The Emmys run twenty categories that differ by three words. Each of these
    // pairs scored 0.6–0.7 on wording alone, which is the band the panel labels
    // LIKELY — a plausible-looking cross-venue quote on two different awards.
    const EMMY_KALSHI: (&str, &str) = (
        "KXEMMYCACTO",
        "Emmy Winner: Outstanding Lead Actor in a Comedy Series",
    );

    #[test]
    fn sibling_award_categories_rejects_the_same_role_in_a_different_genre() {
        let matched = score_series(
            &d(EMMY_KALSHI.0, EMMY_KALSHI.1),
            &d(
                "emmys-2026-outstanding-lead-actor-in-a-drama-series",
                "Emmys 2026 Outstanding Lead Actor in a Drama Series",
            ),
        );
        assert!(
            matched.score < MATCH_FLOOR,
            "expected < {MATCH_FLOOR}, got {}",
            matched.score
        );
    }

    #[test]
    fn sibling_award_categories_rejects_a_different_role_in_the_same_genre() {
        for title in [
            "Emmys 2026 Outstanding Supporting Actor in a Comedy Series",
            "Emmys 2026 Outstanding Guest Actor in a Comedy Series",
            "Emmys 2026 Outstanding Lead Actress in a Comedy Series",
        ] {
            let matched = score_series(&d(EMMY_KALSHI.0, EMMY_KALSHI.1), &d("x", title));
            assert!(
                matched.score < MATCH_FLOOR,
                "\"{title}\" scored {}",
                matched.score
            );
        }
    }

    #[test]
    fn sibling_award_categories_accepts_the_twin() {
        let matched = score_series(
            &d(EMMY_KALSHI.0, EMMY_KALSHI.1),
            &d(
                "emmys-2026-outstanding-lead-actor-in-a-comedy-series",
                "Emmys 2026 Outstanding Lead Actor in a Comedy Series",
            ),
        );
        assert_eq!(matched.confidence, MatchConfidence::Strong);
    }

    /* ------------------------------------------------------- central banks */

    // Eleven rate books, one shared vocabulary. "Bank of X rate decision in
    // September" against "Bank of Y Decision" shares three words out of four, so
    // wording alone put the ECB with the Bank of Russia. Each bank is folded onto
    // one distinctive token instead, reachable from its ticker or its full name.
    #[test]
    fn central_banks_pairs_a_spelled_out_bank_with_the_ticker_another_venue_uses() {
        for (kalshi, other) in [
            ("Bank of Japan rate decision in September", "BoJ Decision"),
            (
                "Bank of Mexico rate decision in September",
                "Banxico Decision",
            ),
            (
                "European Central Bank rate decision in September",
                "ECB Decision",
            ),
            (
                "Central Bank of Brazil rate decision in September",
                "BCB Decision",
            ),
        ] {
            let matched = score_series(&d("a", kalshi), &d("b", other));
            assert!(
                matched.score >= MATCH_FLOOR,
                "{kalshi} <-> {other} scored {}",
                matched.score
            );
        }
    }

    #[test]
    fn central_banks_keeps_two_different_central_banks_apart() {
        for (a, b) in [
            (
                "Bank of Japan rate decision in September",
                "Bank of England Decision",
            ),
            (
                "European Central Bank rate decision in September",
                "Bank of Russia Decision",
            ),
            (
                "Bank of Canada decision in Oct 2026?",
                "Bank of Korea Decision",
            ),
        ] {
            let matched = score_series(&d("a", a), &d("b", b));
            assert!(
                matched.score < MATCH_FLOOR,
                "{a} <-> {b} scored {}",
                matched.score
            );
        }
    }

    #[test]
    fn central_banks_does_not_fold_federal_onto_the_federal_reserve() {
        // It appears in "federal crime", "federal government" and "federal court";
        // folding it paired a federal-charges market with the Fed's target rate.
        assert!(!tokenise("Who will be charged with a federal crime?")
            .iter()
            .any(|t| t == "fed"));
        // "Federal Reserve" still reaches `fed`, through `reserve`.
        assert!(tokenise("Federal Reserve decision")
            .iter()
            .any(|t| t == "fed"));
    }

    /* ----------------------------------------------------- arbitrary titles */

    #[test]
    fn arbitrary_titles_does_not_read_a_market_title_through_object_prototype() {
        // "F1 Constructors Champion" tokenises through `constructor`, and a lookup
        // in an object literal answers that with a function. The Rust tables cannot
        // inherit, so the assertion is the behaviour that guarded: every such word
        // comes back as itself, never as something a base type supplied.
        assert_eq!(
            tokenise("F1 Constructors Champion"),
            vec!["f", "1", "constructor", "champ"]
        );
        assert_eq!(
            tokenise("Who will be the constructor champion?"),
            vec!["constructor", "champ"]
        );
        assert_eq!(
            tokenise("toString valueOf hasOwnProperty prototype"),
            vec!["tostring", "valueof", "hasownproperty", "prototype"]
        );
    }

    #[test]
    fn arbitrary_titles_scores_a_title_containing_a_prototype_key_without_throwing() {
        let matched = score_series(
            &d("KXF1CONSTRUCTORS", "F1 Constructors Champion"),
            &d("f1-constructors-champion", "F1 Constructors Champion"),
        );
        assert_eq!(matched.confidence, MatchConfidence::Strong);
    }

    #[test]
    fn arbitrary_titles_an_absent_title_behaves_as_empty() {
        // The other half of the prototype guard: a market whose title field is
        // missing reads as having no terms, never as having inherited any.
        assert!(tokenise("").is_empty());
        assert!(content_tokens("").is_empty());
        assert!(numbers_in("").is_empty());
        assert!(significant_numbers("").is_empty());
        assert_eq!(normalise(""), "");
    }

    #[test]
    fn arbitrary_titles_an_absent_title_scores_nothing() {
        let matched = score_series(&d("", ""), &d("", ""));
        assert_eq!(matched.score, 0.0);
        assert_eq!(matched.confidence, MatchConfidence::Weak);
        assert_eq!(matched.reason, "no shared terms");
        assert!(matched.shared.is_empty());
    }

    #[test]
    fn arbitrary_titles_an_absent_identifier_never_counts_as_agreement() {
        // Two empty identifiers are not "both listed as the same thing": the
        // four-character floor on identity_key is what stops an unnamed pair from
        // collecting the corroboration bonus.
        assert_eq!(identity_key(""), "");
        let matched = score_series(&d("", "Fed decision in Oct 2026?"), &d("", "Fed Decision"));
        assert!(!matched.reason.contains("both listed as"));
    }

    /* -------------------------------- numbers written the way a strike is written */

    #[test]
    fn strikes_reads_a_comma_grouped_price_as_one_number() {
        // The punctuation strip turned every comma into a space, so `$63,000`
        // became the two tokens `$63` and `000` — and `000` reads as zero. Both
        // sides of a BTC ladder therefore reported the single number 0.
        assert_eq!(numbers_in("Bitcoin above $63,000"), vec![63000.0]);
        assert_eq!(numbers_in("Will BTC hit $1,250,000?"), vec![1250000.0]);
    }

    #[test]
    fn strikes_does_not_let_two_strikes_agree_by_both_collapsing_to_zero() {
        // This is the failure the module exists to prevent: identical number sets
        // are treated as agreement and *rewarded*, so two strikes $8,000 apart
        // scored `likely` and produced a side-by-side price row a reader would take
        // for an arbitrage.
        let matched = score_event(
            &d("KXBTCD-1", "Bitcoin above $63,000"),
            &d("btc-above-71000", "Bitcoin above $71,000"),
        );
        assert!(
            matched.score < MATCH_FLOOR || matched.confidence == MatchConfidence::Weak,
            "two different strikes should not match; got {:?} at {}",
            matched.confidence,
            matched.score
        );
        assert!(matched.reason.contains("numbers differ"));
    }

    #[test]
    fn strikes_tells_two_priced_strikes_apart_as_series_too() {
        // `significantNumbers` read its tokens raw, so `$63,000` and `$71,000`
        // both parsed to `NaN` — and a JavaScript `Set`, which is what the
        // comparison mirrors, treats `NaN` as equal to itself. Two strikes
        // $8,000 apart therefore reported the *same* number and collected the
        // agreement reward, scoring a confident `strong`. Only event scoring,
        // which strips the currency mark first, ever told them apart.
        assert_eq!(significant_numbers("Bitcoin above $63,000"), vec![63000.0]);
        assert_eq!(
            significant_numbers("Fed decision 3.75% or below"),
            vec![3.75]
        );

        let matched = score_series(
            &d("KXBTCD-1", "Bitcoin above $63,000"),
            &d("btc-above-71000", "Bitcoin above $71,000"),
        );
        assert!(
            matched.reason.contains("numbers differ"),
            "two priced strikes should disagree as series; got {:?} at {} — {}",
            matched.confidence,
            matched.score,
            matched.reason
        );
    }

    #[test]
    fn strikes_never_lets_two_unreadable_numbers_count_as_agreement() {
        // Whatever produces them, two numbers nobody could read are two
        // unknowns, not a match. Rewarding them is how the bug above paid out.
        let verdict = compare_numbers(&[f64::NAN], &[f64::NAN]);
        assert!(verdict.factor < 1.0, "two unknowns must not be rewarded");
        assert!(!compare_numbers(&[f64::NAN], &[63000.0]).note.is_empty());
    }

    #[test]
    fn strikes_reads_a_percentage_strike_which_is_how_a_rate_ladder_is_written() {
        assert_eq!(numbers_in("Fed decision 3.75% or below"), vec![3.75]);
        assert_eq!(numbers_in("Fed decision 4.00% or below"), vec![4.0]);
    }

    #[test]
    fn strikes_tells_two_rungs_of_the_same_rate_ladder_apart() {
        let matched = score_event(
            &d("KXFED-375", "Fed decision 3.75% or below"),
            &d("fed-decision-400", "Fed decision 4.00% or below"),
        );
        assert!(matched.reason.contains("numbers differ"));
    }

    /* -------------------------- phrases whose punctuation is what identifies them */

    #[test]
    fn punctuated_phrases_folds_s_and_p_onto_the_same_token_as_spx() {
        // The fold sat after the punctuation strip, so it never saw the `&` it
        // matched on and an S&P title could not meet the `sp500` that SYNONYMS
        // folds `spx` and `inx` onto.
        assert!(tokenise("S&P 500 above 6500").iter().any(|t| t == "sp500"));
        assert!(tokenise("S&P above 6500").iter().any(|t| t == "sp500"));
        assert_eq!(tokenise("S&P 500 above 6500"), tokenise("SPX above 6500"));
    }

    #[test]
    fn punctuated_phrases_deletes_the_live_strike_from_a_short_interval_title() {
        // Kalshi's `· $1,883.54 target` suffix outweighs every content word in the
        // title it is attached to; the rule that removes it could not span the
        // pieces the strip had already broken it into.
        assert_eq!(
            tokenise("ETH price · $1,883.54 target"),
            tokenise("ETH price")
        );
    }

    /* ------------------- a city named in full and the same city named short */

    #[test]
    fn city_names_matches_nyc_against_new_york_city() {
        // SYNONYMS maps the abbreviation onto `newyork`, but nothing folded the
        // spelled-out side, so the two shared no term at all — and `york` is a
        // place qualifier, so the lopsided-qualifier penalty fired on top.
        assert_eq!(tokenise("NYC"), tokenise("New York City"));

        let matched = score_series(
            &d("KXHIGHNY", "Highest temperature in NYC?"),
            &d(
                "highest-temperature-in-new-york-city",
                "Highest temperature in New York City?",
            ),
        );
        assert!(
            matched.score >= MATCH_FLOOR,
            "NYC and New York City should match; got {:?} at {}",
            matched.confidence,
            matched.score
        );
    }

    #[test]
    fn city_names_still_keeps_genuinely_different_places_apart() {
        let matched = score_series(
            &d("KXHIGHNY", "Highest temperature in New York City?"),
            &d(
                "highest-temperature-in-los-angeles",
                "Highest temperature in Los Angeles?",
            ),
        );
        assert!(
            matched.score < MATCH_FLOOR,
            "two different cities should not match; got {:?} at {}",
            matched.confidence,
            matched.score
        );
    }
}
