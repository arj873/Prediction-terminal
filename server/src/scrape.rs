//! A jQuery-shaped traversal layer over [`scraper`].
//!
//! Seven of the sources read HTML rather than JSON, and every one of them was
//! written against cheerio. `scraper` gives us the two things cheerio's
//! underlying parser gives it — a spec-compliant HTML5 tree and CSS selectors —
//! but none of the *traversal* vocabulary the parsers are actually written in:
//! `.next(sel)`, `.closest(sel)`, `.parent()`, `.children(sel)`, `.text()` with
//! its whitespace already collapsed. Those live here, once, instead of being
//! re-derived in each port.
//!
//! Three ideas run through the whole module:
//!
//! - **Nothing panics on missing markup.** A scraper reads a page that is not a
//!   contract, and the surveyed parsers all degrade rather than throw — FRED
//!   would rather lose the "Units:" line than lose the observations. So the
//!   helpers return [`Option`] and let the caller decide what a miss costs. The
//!   two `require_*` entry points are the exceptions, for the cases where the
//!   whole parse is over (no table at all), and they carry
//!   [`UpstreamError::parse_failed`].
//! - **Selectors are compiled once.** [`css!`] wraps a literal in a
//!   `LazyLock<Selector>` so a constant selector is parsed on first use and
//!   never again; [`selector`] is the fallible door for a selector built at
//!   runtime, returning `None` rather than panicking on a typo.
//! - **Text is normalised the way the callers already normalise it.** Every
//!   surveyed parser follows `.text()` with `.replace(/\s+/g, ' ').trim()`, so
//!   [`text_of`] does that itself. [`raw_text_of`] is there for the one case
//!   where it would be wrong: a `<script type="application/json">` payload,
//!   where collapsing runs of spaces would rewrite string values.
//!
//! # Lifetimes
//!
//! [`ElementRef<'a>`] borrows the [`Html`] it came from, and it is `Copy`. The
//! shape that works is: parse once into a local `Html`, keep it alive for the
//! length of the parse function, and pass `ElementRef` values around by value.
//! Every helper here takes `ElementRef` by value and returns references tied to
//! the same `'a`, so no helper ever borrows a *caller's* local.
//!
//! ```no_run
//! # use terminal_server::css;
//! # use terminal_server::scrape::{self, text_of};
//! let doc = scrape::parse_document("<h1>Unemployment Rate</h1>");
//! let title = scrape::select_one(&doc, css!("h1")).map(text_of);
//! assert_eq!(title.as_deref(), Some("Unemployment Rate"));
//! ```

use std::collections::HashMap;
use std::sync::LazyLock;

use scraper::selectable::Selectable;
use scraper::{ElementRef, Html, Node, Selector};

use crate::error::{Result, UpstreamError};

/* ------------------------------------------------------------- selectors */

/// A `&'static Selector` for a literal, compiled on first use.
///
/// The selector text is checked at first evaluation rather than at compile
/// time, so a malformed literal panics — which is what you want for a constant:
/// it is a bug in this repository, not in the page, and every one of them is
/// exercised by the tests that use it.
///
/// ```
/// # use terminal_server::{css, scrape};
/// let doc = scrape::parse_document("<h3 class='c-title'>Golden</h3>");
/// let title = scrape::select_one(&doc, css!("h3.c-title")).unwrap();
/// assert_eq!(scrape::text_of(title), "Golden");
/// ```
#[macro_export]
macro_rules! css {
    ($css:literal) => {{
        static SELECTOR: ::std::sync::LazyLock<::scraper::Selector> =
            ::std::sync::LazyLock::new(|| {
                ::scraper::Selector::parse($css).expect(concat!("invalid CSS selector: ", $css))
            });
        &*SELECTOR
    }};
}

pub use crate::css;

/// Compile a selector built at runtime.
///
/// `None` on a selector the parser rejects — a source that interpolates user
/// input into a selector should not be able to take the process down. Constant
/// selectors belong in [`css!`] instead, which compiles them once.
#[must_use]
pub fn selector(css: &str) -> Option<Selector> {
    match Selector::parse(css) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            tracing::warn!(selector = css, error = %err, "ignoring invalid CSS selector");
            None
        }
    }
}

static TABLE: LazyLock<Selector> = LazyLock::new(|| Selector::parse("table").unwrap());
static TH: LazyLock<Selector> = LazyLock::new(|| Selector::parse("th").unwrap());
static TR: LazyLock<Selector> = LazyLock::new(|| Selector::parse("tr").unwrap());
static TD: LazyLock<Selector> = LazyLock::new(|| Selector::parse("td").unwrap());

/* --------------------------------------------------------------- parsing */

/// Parse a whole page. Equivalent to `cheerio.load(html)`.
#[must_use]
pub fn parse_document(html: &str) -> Html {
    Html::parse_document(html)
}

/// Parse a fragment — markup that is not a document, such as a summary field
/// lifted out of a news feed.
#[must_use]
pub fn parse_fragment(html: &str) -> Html {
    Html::parse_fragment(html)
}

/// The document's root `<html>` element, for when a helper wants an
/// [`ElementRef`] scope rather than the document.
///
/// [`select_one`] and [`select_all`] take `&Html` directly, so this is only
/// needed to hand a document to something that is element-shaped.
#[must_use]
pub fn root(doc: &Html) -> ElementRef<'_> {
    doc.root_element()
}

/* ------------------------------------------------------------- selection */

/// The first descendant matching `selector`, cheerio's `$(sel).first()`.
///
/// `scope` is either `&Html` (the whole document) or an `ElementRef` (that
/// element's descendants, cheerio's `.find(sel).first()`).
pub fn select_one<'a, S>(scope: S, selector: &Selector) -> Option<ElementRef<'a>>
where
    S: Selectable<'a>,
{
    scope.select(selector).next()
}

/// Every descendant matching `selector`, in document order. cheerio's
/// `$(sel).each` / `.find(sel).each`.
pub fn select_all<'a, 'b, S>(scope: S, selector: &'b Selector) -> S::Select<'b>
where
    S: Selectable<'a>,
{
    scope.select(selector)
}

/// [`select_one`], but a miss ends the parse.
///
/// `what` names the thing in the reader's terms ("box office table") and
/// `source_url` is the page it was expected on, giving the message the shape the
/// TypeScript parsers used: *No box office table found on <url>*. No hint is
/// attached — the caller knows what advice fits, and adds it with
/// [`UpstreamError::with_hint`].
pub fn require_one<'a, S>(
    scope: S,
    selector: &Selector,
    what: &str,
    source_url: &str,
) -> Result<ElementRef<'a>>
where
    S: Selectable<'a>,
{
    select_one(scope, selector)
        .ok_or_else(|| UpstreamError::parse_failed(format!("No {what} found on {source_url}")))
}

/// Collapsed text of the first match, `None` when there is no match *or* the
/// match is blank.
///
/// Folding "absent" and "empty" together is deliberate: the surveyed parsers
/// chain candidates with `||`, where an empty string falls through to the next
/// candidate. `text_of_first(..).or_else(|| text_of_first(..))` reads the same
/// way and behaves the same way.
pub fn text_of_first<'a, S>(scope: S, selector: &Selector) -> Option<String>
where
    S: Selectable<'a>,
{
    select_one(scope, selector)
        .map(text_of)
        .filter(|text| !text.is_empty())
}

/// An attribute of the first match — cheerio's `$(sel).attr(name)`, which reads
/// the first element of the set.
pub fn attr_of_first<'a, S>(scope: S, selector: &Selector, name: &str) -> Option<&'a str>
where
    S: Selectable<'a>,
{
    select_one(scope, selector).and_then(|el| attr(el, name))
}

/* ------------------------------------------------------------- traversal */

/// An element's attribute. cheerio's `.attr(name)`.
///
/// The result borrows the document, not the `ElementRef`, so it can outlive the
/// element value it was read from.
#[must_use]
pub fn attr<'a>(el: ElementRef<'a>, name: &str) -> Option<&'a str> {
    el.value().attr(name)
}

/// The element's parent element, `None` at the root. cheerio's `.parent()`,
/// narrowed to elements — the document node is not a useful parent here.
#[must_use]
pub fn parent_element(el: ElementRef<'_>) -> Option<ElementRef<'_>> {
    el.parent().and_then(ElementRef::wrap)
}

/// The nearest ancestor matching `selector`, **starting with `el` itself**.
///
/// jQuery's `.closest()` semantics, which is what FRED's search parser relies
/// on: from the result link, walk out to whichever of `div, li, tr` wraps the
/// result card, then look inside it for the metadata line.
#[must_use]
pub fn closest<'a>(el: ElementRef<'a>, selector: &Selector) -> Option<ElementRef<'a>> {
    std::iter::once(el)
        .chain(el.ancestors().filter_map(ElementRef::wrap))
        .find(|candidate| selector.matches(candidate))
}

/// Every ancestor, nearest first. Does not include `el`.
pub fn ancestors(el: ElementRef<'_>) -> impl Iterator<Item = ElementRef<'_>> {
    el.ancestors().filter_map(ElementRef::wrap)
}

/// The immediately following element sibling, whatever it is. Text and comment
/// nodes between the two are skipped, as in the DOM's `nextElementSibling`.
#[must_use]
pub fn next_element_sibling(el: ElementRef<'_>) -> Option<ElementRef<'_>> {
    el.next_siblings().find_map(ElementRef::wrap)
}

/// cheerio's `.next(selector)`: the immediately following element sibling, and
/// only if *it* matches.
///
/// This is a filter, not a search — the distinction is the whole point. On a
/// Billboard row the artist is the `.c-label` directly after `h3.c-title`; if
/// something else has been slotted in between, the correct answer is `None`
/// rather than the next `.c-label` further down the row, which would be a
/// chart-position number.
#[must_use]
pub fn next_sibling_matching<'a>(
    el: ElementRef<'a>,
    selector: &Selector,
) -> Option<ElementRef<'a>> {
    next_element_sibling(el).filter(|sibling| selector.matches(sibling))
}

/// cheerio's `.nextAll(selector)`: every following element sibling that
/// matches, in document order.
///
/// None of the seven parsers surveyed needs this today — they all reach for
/// `.next()` or for `.parent().find()`. It is here because the two are one
/// character apart in cheerio and worlds apart in behaviour, and a port that
/// wants "the next matching sibling, skipping whatever is in between" should
/// have to name that rather than quietly widen [`next_sibling_matching`].
pub fn next_siblings_matching<'a, 'b>(
    el: ElementRef<'a>,
    selector: &'b Selector,
) -> impl Iterator<Item = ElementRef<'a>> + 'b
where
    'a: 'b,
{
    el.next_siblings()
        .filter_map(ElementRef::wrap)
        .filter(move |sibling| selector.matches(sibling))
}

/// Direct element children, in document order. cheerio's `.children()`.
pub fn child_elements(el: ElementRef<'_>) -> impl Iterator<Item = ElementRef<'_>> {
    el.children().filter_map(ElementRef::wrap)
}

/// cheerio's `.children(selector)`: **direct children only**, filtered.
///
/// Not `.find()`. FRED's label detection takes
/// `children('span, b, strong, th').first()` on a `<p>`, and a descendant search
/// would happily return a `<span>` nested inside the *value* half of the line
/// and then slice the label off the wrong end.
pub fn children_matching<'a, 'b>(
    el: ElementRef<'a>,
    selector: &'b Selector,
) -> impl Iterator<Item = ElementRef<'a>> + 'b
where
    'a: 'b,
{
    el.children()
        .filter_map(ElementRef::wrap)
        .filter(move |child| selector.matches(child))
}

/// cheerio's `.children(selector).first()`.
#[must_use]
pub fn first_child_matching<'a>(el: ElementRef<'a>, selector: &Selector) -> Option<ElementRef<'a>> {
    children_matching(el, selector).next()
}

/* ------------------------------------------------------------------ text */

/// Whitespace for collapsing purposes.
///
/// `char::is_whitespace` plus the byte-order mark, which together cover
/// JavaScript's `\s` — the class every surveyed parser normalises with. The
/// non-breaking space is in `is_whitespace`, which is why FRED's explicit
/// `.replace(/ /g, ' ')` needs no counterpart here.
fn is_ws(ch: char) -> bool {
    ch.is_whitespace() || ch == '\u{feff}'
}

/// Append `text` to `out`, collapsing runs of whitespace to a single space and
/// never emitting a leading one.
fn push_collapsed(out: &mut String, text: &str) {
    for ch in text.chars() {
        if is_ws(ch) {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }
}

fn trim_trailing_space(text: &mut String) {
    while text.ends_with(' ') {
        text.pop();
    }
}

/// `.replace(/\s+/g, ' ').trim()`, the normalisation every surveyed parser
/// applies to text it has just read.
#[must_use]
pub fn collapse_ws(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    push_collapsed(&mut out, text);
    trim_trailing_space(&mut out);
    out
}

/// All text in the element's subtree, whitespace collapsed and trimmed.
///
/// This is cheerio's `.text()` composed with the `.replace(/\s+/g, ' ').trim()`
/// that follows it at every call site in the TypeScript sources, so a port reads
/// `text_of(el)` where the original read `$(el).text().replace(...).trim()`.
///
/// Entities are already decoded: the HTML parser resolves `&amp;` while building
/// the tree, so a Billboard artist reads `Tyler, The Creator & …`. Text that was
/// *double*-encoded upstream arrives here as a literal `&amp;` and needs
/// [`decode_entities`] on top.
///
/// Like cheerio, this includes the contents of `<script>` and `<style>`. That is
/// deliberate — FRED's last-ditch strategy runs a regex over `$('body').text()`
/// and matching the original's input matters more than tidiness — but when the
/// subtree may contain either, [`visible_text_of`] is almost always what you
/// want.
#[must_use]
pub fn text_of(el: ElementRef<'_>) -> String {
    let mut out = String::new();
    for chunk in el.text() {
        push_collapsed(&mut out, chunk);
    }
    trim_trailing_space(&mut out);
    out
}

/// [`text_of`], skipping `<script>` and `<style>` subtrees.
///
/// cheerio's `$('script, style').remove()` followed by `.text()` — what the
/// news-feed sanitiser does before handing copy to a panel.
#[must_use]
pub fn visible_text_of(el: ElementRef<'_>) -> String {
    let mut out = String::new();
    collect_text(el, true, &mut out);
    trim_trailing_space(&mut out);
    out
}

/// Text of the element's *direct* text-node children only.
///
/// The label/value lines FRED renders put the value in a bare text node beside
/// the `<span>` that holds the label, so `own_text(p)` is the value on its own
/// without having to subtract the label from the whole.
#[must_use]
pub fn own_text(el: ElementRef<'_>) -> String {
    let mut out = String::new();
    for child in el.children() {
        if let Node::Text(text) = child.value() {
            push_collapsed(&mut out, text);
        }
    }
    trim_trailing_space(&mut out);
    out
}

/// Subtree text with every byte of whitespace exactly as it was served.
///
/// For payloads rather than prose: Rotten Tomatoes parks its scorecard in a
/// `<script id="media-scorecard-json">`, and collapsing whitespace inside a JSON
/// document rewrites any string value that contains two spaces or a newline.
/// Trim it and parse it; do not normalise it.
#[must_use]
pub fn raw_text_of(el: ElementRef<'_>) -> String {
    el.text().collect()
}

/// The remainder of `whole` after a leading `label`, with the separator eaten.
///
/// FRED reads a labelled line by taking the whole line's text and slicing the
/// label's text off the front — `whole.slice(label.length).replace(/^[:\s]+/, '')`
/// in the original. Ported literally that is a bug waiting to happen, since JS
/// `.length` counts UTF-16 units and Rust slicing counts bytes; a label with an
/// accent or an em dash would panic. This does it on character boundaries, and
/// returns `whole` untouched when the label is not actually a prefix.
#[must_use]
pub fn text_after_label<'t>(whole: &'t str, label: &str) -> &'t str {
    let rest = whole.strip_prefix(label).unwrap_or(whole);
    rest.trim_start_matches(|ch: char| ch == ':' || is_ws(ch))
}

/* -------------------------------------------------------------- entities */

/// Decode HTML character references: `AT&amp;T` → `AT&T`, `&#39;` → `'`,
/// `&nbsp;` → U+00A0.
///
/// Text pulled out of a parsed tree is already decoded once, so this is for the
/// *wire* strings that arrive escaped — news headlines and summaries — and for
/// copy that was escaped twice upstream, where the tree's own decode leaves a
/// literal `&amp;` behind.
///
/// Only complete references ending in `;` are decoded. The legacy
/// semicolon-less form (`&amp` on its own) is left alone, which matches how
/// browsers treat it in attribute values and costs nothing on real feeds.
#[must_use]
pub fn decode_entities(text: &str) -> String {
    html_escape::decode_html_entities(text).into_owned()
}

/// Markup in, plain text out. cheerio used purely as a sanitiser.
///
/// A news summary is cut from an article body and can bring tags with it, and
/// the whole feed arrives HTML-escaped. Every panel renders through
/// `textContent`, so the markup has to come off and the escaping has to come
/// undone before the string leaves the server:
///
/// 1. parse the fragment,
/// 2. take its text, minus `<script>` and `<style>`,
/// 3. decode entities again — the parse resolved one layer, this catches copy
///    that was escaped twice,
/// 4. collapse whitespace and trim.
///
/// A string with neither `<` nor `&` skips the parse entirely and is only
/// collapsed, which is the common case on a headline.
///
/// One deliberate departure from cheerio's `.text()`: a `<br>` or a block
/// boundary becomes a space here. `<div>Fed holds<br>rates steady</div>` is
/// `Fed holdsrates steady` under a plain concatenation, and this is display
/// copy — the two halves were never adjacent on the page. [`text_of`] keeps the
/// concatenating behaviour, because the parsers that key off it are reading
/// structure rather than prose.
#[must_use]
pub fn strip_markup(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    if !raw.contains('<') && !raw.contains('&') {
        return collapse_ws(raw);
    }

    let fragment = Html::parse_fragment(raw);
    let mut text = String::new();
    collect_display_text(fragment.root_element(), &mut text);
    collapse_ws(&decode_entities(&text))
}

/// Whether an element separates the copy on either side of it.
///
/// The usual `textContent` versus `innerText` line: inline tags (`<b>`, `<a>`,
/// `<em>`) hold a sentence together and must not have spaces injected around
/// them, everything structural breaks it.
fn is_block_boundary(name: &str) -> bool {
    matches!(
        name,
        "br" | "hr"
            | "p"
            | "div"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "aside"
            | "main"
            | "nav"
            | "blockquote"
            | "pre"
            | "figure"
            | "figcaption"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
    )
}

/// Text as it reads on a page: script and style dropped, block boundaries
/// spaced.
fn collect_display_text(el: ElementRef<'_>, out: &mut String) {
    for child in el.children() {
        match child.value() {
            Node::Text(text) => out.push_str(text),
            Node::Element(element) => {
                if matches!(element.name(), "script" | "style") {
                    continue;
                }
                let Some(child_el) = ElementRef::wrap(child) else {
                    continue;
                };
                let breaks = is_block_boundary(element.name());
                if breaks {
                    out.push(' ');
                }
                collect_display_text(child_el, out);
                if breaks {
                    out.push(' ');
                }
            }
            _ => {}
        }
    }
}

/// Append the subtree's text to `out`, optionally skipping script and style.
fn collect_text(el: ElementRef<'_>, skip_scripts: bool, out: &mut String) {
    for node in el.descendants() {
        let Node::Text(text) = node.value() else {
            continue;
        };
        if skip_scripts
            && node.ancestors().any(|ancestor| {
                ancestor
                    .value()
                    .as_element()
                    .is_some_and(|el| matches!(el.name(), "script" | "style"))
            })
        {
            continue;
        }
        out.push_str(text);
    }
}

/* ---------------------------------------------------------------- tables */

/// Normalise a header cell to a column key.
///
/// Three of the scrapers resolve table columns by header text rather than by
/// index, so that an upstream inserting a column does not silently shift
/// "theaters" into "days in release". This is the shared normalisation, and the
/// two substitutions in it are the load-bearing part:
///
/// - `%` → `pct`, because Mojo ships `YD` (yesterday's rank) beside `%± YD`
///   (the day-over-day change). Dropping the symbol maps both to `yd` and reads
///   a rank where a percentage belongs.
/// - `+` → `plus`, because kworb pairs every value column with its delta —
///   `Streams` beside `Streams+`, `7Day` beside `7Day+`. Dropping the sign
///   collapses both onto one key and whichever came first answers for both.
///
/// `±` is dropped rather than named: it appears only as part of `%±`, where the
/// `%` already carries the distinction. Everything else non-alphanumeric goes,
/// after lowercasing.
///
/// The function is idempotent — `header_key("%± YD")` and `header_key("pctyd")`
/// are both `pctyd` — which is what lets [`Table::column_index`] normalise its
/// argument and accept either spelling.
#[must_use]
pub fn header_key(text: &str) -> String {
    let mut key = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '%' => key.push_str("pct"),
            '+' => key.push_str("plus"),
            '±' => {}
            _ if ch.is_ascii_alphanumeric() => key.push(ch.to_ascii_lowercase()),
            // Non-ASCII letters and digits are dropped rather than transliterated,
            // exactly as `[^a-z0-9]` did.
            _ => {}
        }
    }
    key
}

/// One `<tr>`, materialised.
#[derive(Debug)]
struct RawRow<'a> {
    element: ElementRef<'a>,
    cells: Vec<String>,
}

/// A header-keyed view of one `<table>`.
///
/// Built eagerly: every `<th>` becomes a column key and every `<tr>` with at
/// least one `<td>` becomes a row of collapsed cell text. The header row is not
/// a row (it has no `<td>`), which is the same thing the TypeScript parsers got
/// from `if (cells.length === 0) return`.
///
/// ```
/// # use terminal_server::scrape::{self, Table};
/// let doc = scrape::parse_document(
///     "<table><tr><th>YD</th><th>%± YD</th></tr><tr><td>3</td><td>-18.3%</td></tr></table>",
/// );
/// let table = Table::first_in(&doc).unwrap();
/// assert_eq!(table.column_index("yd"), Some(0));
/// assert_eq!(table.column_index("pctyd"), Some(1));
/// assert_eq!(table.rows().next().unwrap().get("pctyd"), Some("-18.3%"));
/// ```
#[derive(Debug)]
pub struct Table<'a> {
    element: ElementRef<'a>,
    headers: Vec<String>,
    columns: HashMap<String, usize>,
    rows: Vec<RawRow<'a>>,
}

impl<'a> Table<'a> {
    /// Read a table from its `<table>` element.
    #[must_use]
    pub fn from_element(table: ElementRef<'a>) -> Self {
        let mut headers = Vec::new();
        let mut columns: HashMap<String, usize> = HashMap::new();

        for (index, th) in table.select(&TH).enumerate() {
            let key = header_key(&text_of(th));
            if !key.is_empty() {
                // First spelling wins, as in `if (!columns.has(key))`.
                columns.entry(key.clone()).or_insert(index);
            }
            headers.push(key);
        }

        let rows = table
            .select(&TR)
            .filter_map(|tr| {
                let cells: Vec<String> = tr.select(&TD).map(text_of).collect();
                (!cells.is_empty()).then_some(RawRow { element: tr, cells })
            })
            .collect();

        Self {
            element: table,
            headers,
            columns,
            rows,
        }
    }

    /// The first `<table>` in scope — `$('table').first()`.
    #[must_use]
    pub fn first_in<S>(scope: S) -> Option<Self>
    where
        S: Selectable<'a>,
    {
        select_one(scope, &TABLE).map(Self::from_element)
    }

    /// [`Table::first_in`], where a page with no table ends the parse.
    ///
    /// `what` names it for the reader ("box office table", "chart table"); the
    /// caller adds the hint that fits.
    pub fn require_first_in<S>(scope: S, what: &str, source_url: &str) -> Result<Self>
    where
        S: Selectable<'a>,
    {
        require_one(scope, &TABLE, what, source_url).map(Self::from_element)
    }

    /// Name the columns whose header cell is blank, by position.
    ///
    /// kworb leaves the rank and movement headers empty on most of its tables
    /// and steamcharts leaves the rank header empty on its leaderboard; both
    /// conventions are stable, so those columns are addressed positionally:
    ///
    /// ```
    /// # use terminal_server::scrape::{self, Table};
    /// # let doc = scrape::parse_document(
    /// #     "<table><tr><th></th><th></th><th>Streams</th></tr>\
    /// #      <tr><td>1</td><td>+3</td><td>1,466,839</td></tr></table>");
    /// let table = Table::first_in(&doc).unwrap().with_positional_headers(&["pos", "move"]);
    /// assert_eq!(table.column_index("move"), Some(1));
    /// ```
    ///
    /// Only blank headers are named — a column whose header says something is
    /// keyed by what it says. A name given as `""` skips that position, and a
    /// name that collides with a real header elsewhere in the table wins, which
    /// is the order the original `.each` produced.
    #[must_use]
    pub fn with_positional_headers(mut self, names: &[&str]) -> Self {
        for (index, name) in names.iter().enumerate() {
            if name.is_empty() {
                continue;
            }
            if self
                .headers
                .get(index)
                .is_some_and(|header| header.is_empty())
            {
                self.columns.insert(header_key(name), index);
            }
        }
        self
    }

    /// The `<table>` element itself, for anything this view does not model.
    #[must_use]
    pub fn element(&self) -> ElementRef<'a> {
        self.element
    }

    /// Column index for a header key. Accepts either the normalised key
    /// (`"pctyd"`) or the header as printed (`"%± YD"`).
    #[must_use]
    pub fn column_index(&self, key: &str) -> Option<usize> {
        if let Some(index) = self.columns.get(key) {
            return Some(*index);
        }
        self.columns.get(&header_key(key)).copied()
    }

    /// Whether the table has such a column. kworb's all-time table is told from
    /// its daily tables by the presence of `yesterday`.
    #[must_use]
    pub fn has_column(&self, key: &str) -> bool {
        self.column_index(key).is_some()
    }

    /// Normalised header keys in document order; a blank header stays blank
    /// unless [`Table::with_positional_headers`] named it.
    #[must_use]
    pub fn headers(&self) -> &[String] {
        &self.headers
    }

    /// The data rows: every `<tr>` carrying at least one `<td>`.
    pub fn rows<'t>(&'t self) -> impl ExactSizeIterator<Item = Row<'a, 't>> + 't {
        self.rows.iter().map(|raw| Row { table: self, raw })
    }

    /// One data row by position.
    #[must_use]
    pub fn row(&self, index: usize) -> Option<Row<'a, '_>> {
        self.rows.get(index).map(|raw| Row { table: self, raw })
    }

    /// How many data rows there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the table has no data rows — a header and nothing under it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// One data row of a [`Table`], carrying its cells and the column map they are
/// read through.
#[derive(Debug, Clone, Copy)]
pub struct Row<'a, 't> {
    table: &'t Table<'a>,
    raw: &'t RawRow<'a>,
}

impl<'a, 't> Row<'a, 't> {
    /// The `<tr>` element, for what the cell text does not carry — steamcharts
    /// takes the app id out of the row's own `/app/730` link.
    #[must_use]
    pub fn element(&self) -> ElementRef<'a> {
        self.raw.element
    }

    /// Collapsed text of every `<td>`, in document order.
    #[must_use]
    pub fn cells(&self) -> &'t [String] {
        &self.raw.cells
    }

    /// One cell by position.
    #[must_use]
    pub fn cell(&self, index: usize) -> Option<&'t str> {
        self.raw.cells.get(index).map(String::as_str)
    }

    /// One cell by header key.
    ///
    /// `Some("")` for a column that exists and is empty; `None` only when the
    /// table has no such column, or the row is short of it. Callers that treat
    /// an empty cell as absent should say so — `.filter(|s| !s.is_empty())`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&'t str> {
        self.table.column_index(key).and_then(|i| self.cell(i))
    }

    /// The first of several header keys this table actually has — the
    /// `at(cells, 'release', 'title')` idiom, for a column an upstream has
    /// renamed between chart types.
    ///
    /// Resolution is by column, not by content: the first key that names a
    /// column present in the row wins even if that cell is empty.
    #[must_use]
    pub fn first_of(&self, keys: &[&str]) -> Option<&'t str> {
        keys.iter().find_map(|key| self.get(key))
    }

    /// Number of `<td>` cells in this row.
    #[must_use]
    pub fn len(&self) -> usize {
        self.raw.cells.len()
    }

    /// Always false — a row with no cells is not a row. Present because clippy
    /// asks for it alongside [`Row::len`].
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.raw.cells.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Billboard chart row, trimmed to the structure the parser anchors on.
    const BILLBOARD_ROW: &str = r#"
        <div class="o-chart-results-list-row-container">
          <ul class="o-chart-results-list-row">
            <li class="o-chart-results-list__item">
              <span class="c-label a-font-primary-bold-l">1</span>
            </li>
            <li class="o-chart-results-list__item">
              <img class="c-lazy-image__img" src="data:image/gif;base64,PLACEHOLDER"
                   data-lazy-src="https://charts-static.billboard.com/img/golden.jpg" />
            </li>
            <li class="o-chart-results-list__item">
              <h3 id="title-of-a-story" class="c-title">Golden</h3>
              <span class="c-label">HUNTR/X: EJAE, Audrey Nuna &amp; REI AMI</span>
            </li>
            <li class="o-chart-results-list__item">
              <span class="c-span">LW</span>
              <span class="c-label">2</span>
            </li>
            <li class="o-chart-results-list__item">
              <span class="c-span">PEAK</span>
              <span class="c-label">1</span>
            </li>
          </ul>
        </div>
    "#;

    /// The same row on a chart where the artist is not the next sibling.
    const BILLBOARD_ROW_UNLINKED: &str = r#"
        <div class="o-chart-results-list-row-container">
          <li class="o-chart-results-list__item">
            <h3 class="c-title">Ordinary</h3>
            <div class="c-tagline">New entry</div>
            <span class="c-label">Alex Warren</span>
          </li>
        </div>
    "#;

    /// FRED's metadata block: a label element and the value beside it.
    const FRED_META: &str = r#"
        <div id="series-meta">
          <p><span class="series-meta-label">Units:</span>&nbsp;Percent,
             Seasonally   Adjusted</p>
          <p><b>Frequency:</b> Monthly</p>
          <div class="fg-source">
            <span class="series-meta-label">Source</span>
            <span class="series-meta-value">U.S. Bureau of Labor Statistics</span>
          </div>
        </div>
    "#;

    /// A FRED search result card.
    const FRED_SEARCH: &str = r#"
        <div class="search-results">
          <div class="series-pager-item">
            <a href="/series/UNRATE">Unemployment Rate</a>
            <p class="series-meta">Percent, Monthly, Seasonally Adjusted</p>
          </div>
          <div class="site-nav"><a href="/series/">All series</a></div>
        </div>
    "#;

    /// A Rotten Tomatoes search hit: a custom element whose data is in its
    /// attributes, because the element's own rendering never reaches the wire.
    const RT_SEARCH_ROW: &str = r#"
        <search-page-media-row data-qa="data-row" release-year="2026"
                               tomatometer-score="87" audience-score="91">
          <a href="https://www.rottentomatoes.com/m/dune_part_three" data-qa="thumbnail-link">
            <img alt="Dune: Part Three" src="https://resizing.flixster.com/poster.jpg" />
          </a>
          <a href="https://www.rottentomatoes.com/m/dune_part_three" data-qa="info-name">
            Dune: Part Three
          </a>
        </search-page-media-row>
    "#;

    /// Box Office Mojo's daily table — note `YD` and `%± YD` side by side.
    const MOJO_TABLE: &str = r#"
        <table>
          <tr>
            <th>TD</th><th>YD</th><th class="a-text-left">Release</th><th>Daily</th>
            <th>%± YD</th><th>%± LW</th><th>Theaters</th><th>Avg</th>
            <th>To Date</th><th>Days</th><th class="a-text-left">Distributor</th>
          </tr>
          <tr>
            <td>1</td><td>1</td><td><a href="/release/rl123/">Weapons</a></td>
            <td>$4,150,000</td><td>-18.3%</td><td>+12.4%</td><td>3,650</td>
            <td>$1,136</td><td>$102,000,000</td><td>8</td><td>Warner Bros.</td>
          </tr>
          <tr>
            <td>2</td><td>-</td><td>Freakier Friday</td><td>$2,010,000</td>
            <td>-</td><td>-</td><td>3,100</td><td>$648</td><td>$2,010,000</td>
            <td>1</td><td>Walt Disney</td>
          </tr>
        </table>
    "#;

    /// kworb's Spotify daily table: `Streams` beside `Streams+`, blank rank and
    /// movement headers.
    const KWORB_TABLE: &str = r#"
        <table class="sortable">
          <thead>
            <tr><th></th><th></th><th>Artist and Title</th><th>Days</th>
                <th>Pk</th><th>Streams</th><th>Streams+</th><th>7Day</th>
                <th>7Day+</th><th>Total</th></tr>
          </thead>
          <tbody>
            <tr><td>1</td><td>=</td><td><a href="/spotify/track/x">Alex Warren - Ordinary</a></td>
                <td>112</td><td>1</td><td>1,466,839</td><td>+84,270</td>
                <td>10,268,873</td><td>-116,663</td><td>412,345,678</td></tr>
            <tr><td>2</td><td>NEW</td><td>Shakira - Dai Dai (w/ Burna Boy)</td>
                <td>1</td><td>2</td><td>982,014</td><td>+982,014</td>
                <td>982,014</td><td>+982,014</td><td>982,014</td></tr>
          </tbody>
        </table>
    "#;

    /// steamcharts' most-played leaderboard: blank rank header, app id in the link.
    const STEAMCHARTS_TABLE: &str = r#"
        <table id="top-games" class="common-table">
          <thead>
            <tr><th></th><th>Name</th><th>Current Players</th>
                <th>Peak Players</th><th>Hours Played</th></tr>
          </thead>
          <tbody>
            <tr>
              <td class="num">1.</td>
              <td id="js-app-name-730"><a href="/app/730">Counter-Strike 2</a></td>
              <td class="num">1,234,567</td><td class="num">1,800,000</td>
              <td class="num">9,999,999</td>
            </tr>
          </tbody>
        </table>
    "#;

    /* ------------------------------------------------------------- text */

    #[test]
    fn text_of_collapses_every_flavour_of_whitespace() {
        let doc = parse_document(
            "<p>  Percent,\n\t Seasonally\u{a0}\u{a0}Adjusted   <b> (SA) </b>\r\n</p>",
        );
        let p = select_one(&doc, css!("p")).unwrap();
        assert_eq!(text_of(p), "Percent, Seasonally Adjusted (SA)");
    }

    #[test]
    fn text_of_joins_across_element_boundaries_without_gluing_words() {
        let doc = parse_document("<div><span>Streaming</span> <span>Songs</span></div>");
        let div = select_one(&doc, css!("div")).unwrap();
        assert_eq!(text_of(div), "Streaming Songs");

        // No whitespace between the two elements means no space in the text,
        // exactly as cheerio's `.text()` concatenation gives it.
        let glued = parse_document("<div><span>Hot</span><span>100</span></div>");
        assert_eq!(text_of(select_one(&glued, css!("div")).unwrap()), "Hot100");
    }

    #[test]
    fn text_of_decodes_entities_resolved_by_the_parser() {
        let doc = parse_document("<h3>Tyler, The Creator &amp; Kali Uchis &#39;24</h3>");
        assert_eq!(
            text_of(select_one(&doc, css!("h3")).unwrap()),
            "Tyler, The Creator & Kali Uchis '24"
        );
    }

    #[test]
    fn text_of_is_empty_for_an_empty_element() {
        let doc = parse_document("<table><tr><td>   </td></tr></table>");
        assert_eq!(text_of(select_one(&doc, css!("td")).unwrap()), "");
    }

    #[test]
    fn text_of_inserts_nothing_at_an_element_boundary_just_as_cheerio_does() {
        // `.text()` is a concatenation of text nodes: a `<br>` between two runs
        // of copy does not become a space. Every surveyed parser reads it this
        // way, so this one stays faithful — `strip_markup` is where display copy
        // gets the boundary treated as a break.
        let doc = parse_document("<div>Fed holds<br>rates steady</div>");
        assert_eq!(
            text_of(select_one(&doc, css!("div")).unwrap()),
            "Fed holdsrates steady"
        );
    }

    #[test]
    fn own_text_ignores_nested_elements() {
        let doc = parse_document("<p><span>Units:</span> Percent <b>(SA)</b> monthly</p>");
        let p = select_one(&doc, css!("p")).unwrap();
        assert_eq!(own_text(p), "Percent monthly");
        assert_eq!(text_of(p), "Units: Percent (SA) monthly");
    }

    #[test]
    fn raw_text_preserves_whitespace_inside_a_json_payload() {
        let doc = parse_document(
            r#"<script id="media-scorecard-json" type="application/json">
{"criticsScore":{"score":"92","sentiment":"POSITIVE"},"description":"Two  spaces."}
</script>"#,
        );
        let script = select_one(&doc, css!("#media-scorecard-json")).unwrap();
        let raw = raw_text_of(script);
        assert!(raw.contains("Two  spaces."), "{raw:?}");
        let parsed: serde_json::Value = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(parsed["criticsScore"]["score"], "92");

        // The collapsing reader would have rewritten the description.
        assert!(text_of(script).contains("Two spaces."));
    }

    #[test]
    fn text_of_keeps_script_text_and_visible_text_drops_it() {
        let doc = parse_document(
            "<body><p>Units: Percent</p><script>var x = 'Units: Nonsense';</script>\
             <style>.c-label{color:red}</style></body>",
        );
        let body = select_one(&doc, css!("body")).unwrap();
        assert!(text_of(body).contains("Nonsense"));
        assert_eq!(visible_text_of(body), "Units: Percent");
    }

    #[test]
    fn collapse_ws_matches_the_javascript_normalisation() {
        assert_eq!(collapse_ws("  a \n b\t\tc  "), "a b c");
        assert_eq!(collapse_ws("AT\u{a0}&\u{a0}T"), "AT & T");
        assert_eq!(collapse_ws(""), "");
        assert_eq!(collapse_ws("   "), "");
    }

    #[test]
    fn text_after_label_eats_the_label_and_its_separator() {
        assert_eq!(
            text_after_label("Units: Percent, Seasonally Adjusted", "Units:"),
            "Percent, Seasonally Adjusted"
        );
        assert_eq!(
            text_after_label("Frequency Monthly", "Frequency"),
            "Monthly"
        );
        // Multi-byte labels do not panic the way a byte slice would.
        assert_eq!(text_after_label("Périod — Daily", "Périod"), "— Daily");
        // Not a prefix: the line comes back whole rather than mangled.
        assert_eq!(text_after_label("Percent", "Units:"), "Percent");
    }

    /* --------------------------------------------------------- entities */

    #[test]
    fn decode_entities_handles_the_forms_that_reach_the_wire() {
        assert_eq!(decode_entities("AT&amp;T"), "AT&T");
        assert_eq!(decode_entities("Q3 &#39;26"), "Q3 '26");
        assert_eq!(decode_entities("Q3 &#039;26"), "Q3 '26");
        assert_eq!(decode_entities("a&nbsp;b"), "a\u{a0}b");
        assert_eq!(collapse_ws(&decode_entities("a&nbsp;b")), "a b");
        assert_eq!(decode_entities("5 &lt; 6 &gt; 4"), "5 < 6 > 4");
        // Nothing to decode is left exactly as it was.
        assert_eq!(decode_entities("plain & simple"), "plain & simple");
    }

    #[test]
    fn strip_markup_takes_tags_off_and_escaping_out() {
        assert_eq!(
            strip_markup("<p>AT&amp;T beats Q3 &#39;26 estimates</p>"),
            "AT&T beats Q3 '26 estimates"
        );
        // A break or a block boundary separates copy that was never adjacent…
        assert_eq!(
            strip_markup("<div>Fed holds<br/>rates   steady</div>"),
            "Fed holds rates steady"
        );
        assert_eq!(
            strip_markup("<p>Halt lifted.</p><p>Trading resumes.</p>"),
            "Halt lifted. Trading resumes."
        );
        // …while an inline tag holds a word together.
        assert_eq!(
            strip_markup("AT<b>&amp;</b>T raises guidance"),
            "AT&T raises guidance"
        );
        // Double-escaped copy is decoded the second time here.
        assert_eq!(strip_markup("AT&amp;amp;T"), "AT&T");
        // Script bodies never reach a panel.
        assert_eq!(
            strip_markup("<p>Halted</p><script>alert(1)</script>"),
            "Halted"
        );
        // The fast path: no markup, no entities, just normalisation.
        assert_eq!(strip_markup("  spaced   out\n"), "spaced out");
        assert_eq!(strip_markup(""), "");
    }

    /* -------------------------------------------------------- selectors */

    #[test]
    fn dynamic_selectors_fail_softly() {
        assert!(selector("div.result a[href*='/series/']").is_some());
        assert!(selector("div[").is_none());
        assert!(selector("").is_none());
    }

    #[test]
    fn select_one_and_select_all_work_from_document_or_element() {
        let doc = parse_document(BILLBOARD_ROW);
        assert_eq!(select_all(&doc, css!(".c-label")).count(), 4);

        let row = select_one(&doc, css!(".o-chart-results-list-row-container")).unwrap();
        assert_eq!(
            text_of(select_one(row, css!(".c-label")).unwrap()),
            "1",
            "the rank is the first .c-label in the row"
        );
        assert_eq!(select_all(row, css!(".c-span")).count(), 2);
    }

    #[test]
    fn text_of_first_treats_blank_as_missing_so_or_else_chains_read_like_the_original() {
        let doc = parse_document("<h1 id=\"page-title\">  </h1><title>Unemployment Rate</title>");
        let title = text_of_first(&doc, css!("#page-title"))
            .or_else(|| text_of_first(&doc, css!("title")))
            .unwrap();
        assert_eq!(title, "Unemployment Rate");
    }

    #[test]
    fn require_one_reports_a_parse_failure_naming_the_page() {
        let doc = parse_document("<p>No data available</p>");
        let err = require_one(
            &doc,
            css!("table"),
            "box office table",
            "https://www.boxofficemojo.com/date/2026-08-17/",
        )
        .unwrap_err();
        assert_eq!(err.code, crate::codes::PARSE_FAILED);
        assert_eq!(
            err.message,
            "No box office table found on https://www.boxofficemojo.com/date/2026-08-17/"
        );
        assert!(err.hint.is_none(), "the caller supplies the hint");
    }

    /* -------------------------------------------------------- traversal */

    #[test]
    fn billboard_artist_is_the_c_label_immediately_after_the_title() {
        let doc = parse_document(BILLBOARD_ROW);
        let title = select_one(&doc, css!("h3.c-title, .c-title")).unwrap();
        assert_eq!(text_of(title), "Golden");

        let artist = next_sibling_matching(title, css!(".c-label, span")).unwrap();
        assert_eq!(text_of(artist), "HUNTR/X: EJAE, Audrey Nuna & REI AMI");
    }

    #[test]
    fn next_sibling_matching_refuses_a_sibling_further_down() {
        let doc = parse_document(BILLBOARD_ROW_UNLINKED);
        let title = select_one(&doc, css!("h3.c-title")).unwrap();

        // The immediate sibling is a tagline, so `.next('.c-label')` is a miss —
        // it must not skip ahead to the artist label two nodes later.
        assert!(next_sibling_matching(title, css!(".c-label")).is_none());
        assert_eq!(text_of(next_element_sibling(title).unwrap()), "New entry");

        // Which is what the parser's fallback is for.
        let artist = parent_element(title).and_then(|li| select_one(li, css!(".c-label")));
        assert_eq!(text_of(artist.unwrap()), "Alex Warren");

        // And `.nextAll()` is how you would say "skip whatever is between".
        assert_eq!(
            next_siblings_matching(title, css!(".c-label"))
                .map(text_of)
                .collect::<Vec<_>>(),
            vec!["Alex Warren".to_string()]
        );
    }

    #[test]
    fn billboard_stats_read_by_label_through_the_parent() {
        let doc = parse_document(BILLBOARD_ROW);
        let row = select_one(&doc, css!(".o-chart-results-list-row-container")).unwrap();

        let stats: Vec<(String, String)> = select_all(row, css!(".c-span"))
            .filter_map(|span| {
                let label = text_of(span);
                let value = parent_element(span).and_then(|li| select_one(li, css!(".c-label")))?;
                Some((label, text_of(value)))
            })
            .collect();

        assert_eq!(
            stats,
            vec![
                ("LW".to_string(), "2".to_string()),
                ("PEAK".to_string(), "1".to_string()),
            ]
        );
    }

    #[test]
    fn lazy_artwork_prefers_the_parked_url() {
        let doc = parse_document(BILLBOARD_ROW);
        let row = select_one(&doc, css!(".o-chart-results-list-row-container")).unwrap();
        let img = select_one(row, css!("img.c-lazy-image__img"))
            .or_else(|| select_one(row, css!("img")))
            .unwrap();

        assert_eq!(
            attr(img, "data-lazy-src").or_else(|| attr(img, "src")),
            Some("https://charts-static.billboard.com/img/golden.jpg")
        );
    }

    #[test]
    fn fred_label_is_the_first_matching_direct_child() {
        let doc = parse_document(FRED_META);

        // FRED's primary strategy: any of `p, li, div, tr` whose first
        // span/b/strong/th child *is* a known label. The wrapping `<div>` comes
        // first in document order and its text also starts with "Units:", which
        // is exactly why the label is identified by a direct child rather than
        // by the line's text.
        let (line, label) = select_all(&doc, css!("p, li, div, tr"))
            .find_map(|el| {
                let label = first_child_matching(el, css!("span, b, strong, th"))?;
                let text = text_of(label);
                text.trim_end_matches(':')
                    .eq_ignore_ascii_case("units")
                    .then_some((el, label))
            })
            .unwrap();

        assert_eq!(line.value().name(), "p");
        assert_eq!(text_of(label), "Units:");

        let whole = text_of(line);
        assert_eq!(whole, "Units: Percent, Seasonally Adjusted");
        assert_eq!(
            text_after_label(&whole, &text_of(label)),
            "Percent, Seasonally Adjusted"
        );

        // The same line's value is also its own text, with no subtraction.
        assert_eq!(own_text(line), "Percent, Seasonally Adjusted");
    }

    #[test]
    fn children_matching_does_not_reach_into_descendants() {
        let doc = parse_document("<p><em><span>Not the label</span></em> Units: Percent</p>");
        let p = select_one(&doc, css!("p")).unwrap();

        assert!(first_child_matching(p, css!("span")).is_none());
        // `.find()` would have taken the nested span and sliced the wrong prefix.
        assert!(select_one(p, css!("span")).is_some());

        assert_eq!(
            children_matching(p, css!("em, span"))
                .map(text_of)
                .collect::<Vec<_>>(),
            vec!["Not the label".to_string()]
        );
        assert_eq!(child_elements(p).count(), 1);
    }

    #[test]
    fn fred_legacy_meta_pairs_read_through_next() {
        let doc = parse_document(FRED_META);
        let label = select_all(&doc, css!(".series-meta-label"))
            .find(|el| text_of(*el) == "Source")
            .unwrap();

        let value = next_sibling_matching(label, css!(".series-meta-value, .fg-source-value"));
        assert_eq!(
            value.map(text_of).as_deref(),
            Some("U.S. Bureau of Labor Statistics")
        );

        // The `Units:` label has no `.series-meta-value` beside it, so the pair
        // strategy declines rather than picking up the next line's value.
        let units = select_one(&doc, css!("p .series-meta-label")).unwrap();
        assert!(next_sibling_matching(units, css!(".series-meta-value")).is_none());
    }

    #[test]
    fn closest_walks_out_to_the_result_card_and_includes_self() {
        let doc = parse_document(FRED_SEARCH);
        let link = select_all(&doc, css!("a[href*=\"/series/\"]"))
            .find(|a| attr(*a, "href") == Some("/series/UNRATE"))
            .unwrap();

        let card = closest(link, css!("div, li, tr")).unwrap();
        assert_eq!(attr(card, "class"), Some("series-pager-item"));

        let meta = select_one(card, css!(".series-meta, .fred-meta, .search-result-meta"));
        assert_eq!(
            meta.map(text_of).as_deref(),
            Some("Percent, Monthly, Seasonally Adjusted")
        );

        // jQuery's `.closest()` starts at the element itself.
        assert_eq!(closest(card, css!("div")).unwrap(), card);
        // And gives up at the root rather than wrapping around.
        assert!(closest(link, css!("table")).is_none());
    }

    #[test]
    fn custom_elements_keep_their_hyphenated_attributes() {
        let doc = parse_document(RT_SEARCH_ROW);
        let row = select_one(&doc, css!("search-page-media-row")).unwrap();

        assert_eq!(attr(row, "release-year"), Some("2026"));
        assert_eq!(attr(row, "tomatometer-score"), Some("87"));
        assert!(attr(row, "start-year").is_none());

        let href = attr_of_first(row, css!("a[data-qa=\"thumbnail-link\"]"), "href")
            .or_else(|| attr_of_first(row, css!("a"), "href"));
        assert_eq!(
            href,
            Some("https://www.rottentomatoes.com/m/dune_part_three")
        );

        let name = text_of_first(row, css!("[data-qa=\"info-name\"]"))
            .or_else(|| attr_of_first(row, css!("img"), "alt").map(collapse_ws));
        assert_eq!(name.as_deref(), Some("Dune: Part Three"));
    }

    #[test]
    fn ancestors_are_nearest_first_and_exclude_self() {
        let doc = parse_document(FRED_SEARCH);
        let link = select_one(&doc, css!(".series-pager-item a")).unwrap();
        let names: Vec<&str> = ancestors(link).map(|el| el.value().name()).collect();
        assert_eq!(names, vec!["div", "div", "body", "html"]);
    }

    /* ------------------------------------------------------------ table */

    #[test]
    fn header_key_keeps_percent_and_plus_columns_apart() {
        assert_eq!(header_key("YD"), "yd");
        assert_eq!(header_key("%± YD"), "pctyd");
        assert_eq!(header_key("%± LW"), "pctlw");
        assert_eq!(header_key("Streams"), "streams");
        assert_eq!(header_key("Streams+"), "streamsplus");
        assert_eq!(header_key("7Day+"), "7dayplus");
        assert_eq!(header_key("P+"), "pplus");
        assert_eq!(header_key("Artist and Title"), "artistandtitle");
        assert_eq!(header_key("Current Players"), "currentplayers");
        assert_eq!(header_key("To Date"), "todate");
        assert_eq!(header_key("  "), "");

        // Idempotent, which is what lets `column_index` accept either spelling.
        for header in ["%± YD", "Streams+", "Artist and Title", "To Date"] {
            let once = header_key(header);
            assert_eq!(header_key(&once), once, "{header}");
        }
    }

    #[test]
    fn mojo_columns_resolve_by_header_not_position() {
        let doc = parse_document(MOJO_TABLE);
        let table = Table::first_in(&doc).unwrap();

        assert_eq!(table.column_index("td"), Some(0));
        assert_eq!(table.column_index("yd"), Some(1));
        assert_eq!(table.column_index("release"), Some(2));
        assert_eq!(table.column_index("daily"), Some(3));
        // The pair that a `%`-stripping key would have collapsed.
        assert_eq!(table.column_index("pctyd"), Some(4));
        assert_eq!(table.column_index("pctlw"), Some(5));
        assert_ne!(table.column_index("yd"), table.column_index("pctyd"));
        assert_eq!(table.column_index("todate"), Some(8));
        assert_eq!(table.column_index("distributor"), Some(10));
        assert!(table.column_index("gross").is_none());

        // The header as printed works as a key too.
        assert_eq!(table.column_index("%± YD"), Some(4));
        assert!(table.has_column("theaters"));

        // The header row is not a data row.
        assert_eq!(table.len(), 2);
        assert!(!table.is_empty());

        let first = table.row(0).unwrap();
        assert_eq!(first.first_of(&["release", "title"]), Some("Weapons"));
        assert_eq!(first.get("daily"), Some("$4,150,000"));
        assert_eq!(first.get("pctyd"), Some("-18.3%"));
        assert_eq!(first.get("pctlw"), Some("+12.4%"));
        assert_eq!(first.get("distributor"), Some("Warner Bros."));
        assert_eq!(first.cell(0), Some("1"));
        assert_eq!(first.len(), 11);

        let second = table.row(1).unwrap();
        assert_eq!(second.get("yd"), Some("-"));
        assert_eq!(
            second.first_of(&["title", "release"]),
            Some("Freakier Friday")
        );
        assert!(second.first_of(&["nope", "alsonope"]).is_none());
        assert!(table.row(2).is_none());
    }

    #[test]
    fn kworb_keeps_a_value_column_apart_from_its_delta() {
        let doc = parse_document(KWORB_TABLE);
        let table = Table::first_in(&doc)
            .unwrap()
            .with_positional_headers(&["pos", "move"]);

        assert_eq!(table.column_index("pos"), Some(0));
        assert_eq!(table.column_index("move"), Some(1));
        assert_eq!(table.column_index("artistandtitle"), Some(2));
        assert_eq!(table.column_index("streams"), Some(5));
        assert_eq!(table.column_index("streamsplus"), Some(6));
        assert_eq!(table.column_index("7day"), Some(7));
        assert_eq!(table.column_index("7dayplus"), Some(8));
        assert_ne!(
            table.column_index("streams"),
            table.column_index("streamsplus")
        );
        assert!(!table.has_column("yesterday"));

        let row = table.row(0).unwrap();
        assert_eq!(row.get("pos"), Some("1"));
        assert_eq!(row.get("move"), Some("="));
        assert_eq!(row.get("artistandtitle"), Some("Alex Warren - Ordinary"));
        assert_eq!(row.get("streams"), Some("1,466,839"));
        assert_eq!(row.get("streamsplus"), Some("+84,270"));
        assert_eq!(row.get("7dayplus"), Some("-116,663"));

        let debut = table.row(1).unwrap();
        assert_eq!(debut.get("move"), Some("NEW"));
        assert_eq!(
            debut.get("artistandtitle"),
            Some("Shakira - Dai Dai (w/ Burna Boy)")
        );

        assert_eq!(table.rows().len(), 2);
        assert_eq!(
            table
                .rows()
                .filter_map(|r| r.get("pos"))
                .collect::<Vec<_>>(),
            vec!["1", "2"]
        );
    }

    #[test]
    fn positional_headers_only_fill_blanks() {
        let doc = parse_document(MOJO_TABLE);
        let table = Table::first_in(&doc)
            .unwrap()
            .with_positional_headers(&["pos", "move"]);
        // Mojo names both of those columns, so nothing is overwritten.
        assert!(table.column_index("pos").is_none());
        assert!(table.column_index("move").is_none());
        assert_eq!(table.column_index("td"), Some(0));

        // An empty name skips its position.
        let kworb = parse_document(KWORB_TABLE);
        let skipped = Table::first_in(&kworb)
            .unwrap()
            .with_positional_headers(&["", "move"]);
        assert!(skipped.column_index("pos").is_none());
        assert_eq!(skipped.column_index("move"), Some(1));
    }

    #[test]
    fn steamcharts_rank_is_positional_and_the_app_id_comes_off_the_row() {
        let doc = parse_document(STEAMCHARTS_TABLE);
        let table = Table::first_in(&doc)
            .unwrap()
            .with_positional_headers(&["rank"]);

        assert_eq!(table.headers().first().map(String::as_str), Some(""));
        assert_eq!(table.column_index("rank"), Some(0));
        assert_eq!(table.column_index("name"), Some(1));
        assert_eq!(table.column_index("currentplayers"), Some(2));
        assert_eq!(table.column_index("peakplayers"), Some(3));

        let row = table.row(0).unwrap();
        assert_eq!(row.get("rank"), Some("1."));
        assert_eq!(row.get("name"), Some("Counter-Strike 2"));
        assert_eq!(row.get("currentplayers"), Some("1,234,567"));
        assert_eq!(row.get("peakplayers"), Some("1,800,000"));

        let href =
            select_one(row.element(), css!("a[href*=\"/app/\"]")).and_then(|a| attr(a, "href"));
        assert_eq!(href, Some("/app/730"));

        assert_eq!(table.element().value().attr("id"), Some("top-games"));
    }

    #[test]
    fn a_table_with_no_th_has_no_columns_but_still_has_rows() {
        let doc = parse_document("<table><tr><td>1</td><td>Weapons</td></tr></table>");
        let table = Table::first_in(&doc)
            .unwrap()
            .with_positional_headers(&["rank"]);
        assert!(table.headers().is_empty());
        assert!(table.column_index("rank").is_none());
        assert_eq!(table.len(), 1);
        assert_eq!(table.row(0).unwrap().cell(1), Some("Weapons"));
    }

    #[test]
    fn require_first_in_names_the_page_when_there_is_no_table() {
        let doc = parse_document("<p>No data available</p>");
        let err =
            Table::require_first_in(&doc, "chart table", "https://kworb.net/spotify/").unwrap_err();
        assert_eq!(err.code, crate::codes::PARSE_FAILED);
        assert_eq!(
            err.message,
            "No chart table found on https://kworb.net/spotify/"
        );

        let doc = parse_document(MOJO_TABLE);
        assert!(Table::require_first_in(&doc, "box office table", "https://mojo/").is_ok());
    }

    #[test]
    fn table_cells_are_collapsed_like_every_other_read() {
        let doc = parse_document(
            "<table><tr><th>Release</th></tr>\
             <tr><td>  Weapons\n   <span>(2026)</span>  </td></tr></table>",
        );
        let table = Table::first_in(&doc).unwrap();
        assert_eq!(table.row(0).unwrap().get("release"), Some("Weapons (2026)"));
    }
}
