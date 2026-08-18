//! Billboard routes.
//!
//! Three endpoints: the static list of charts worth a keystroke, one chart, and
//! the artwork proxy — the only endpoint in the server that fetches a URL the
//! caller chose, and therefore the only one that needs to argue about it.

use std::sync::LazyLock;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::HeaderValue;
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use terminal_core::types::{BillboardChart, BillboardChartsResponse};

use crate::app::AppState;
use crate::error::{codes, Result, UpstreamError};
use crate::http::{FetchOptions, Http};
use crate::routes::helpers::QueryParams;
use crate::sources::billboard;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/billboard/charts", get(charts))
        .route("/api/billboard/chart/{slug}", get(chart))
        .route("/api/billboard/art", get(art))
}

/// `GET /api/billboard/charts`
///
/// A constant, so it takes no query and touches no cache — the list exists so
/// the client can offer completions without a round trip to billboard.com.
async fn charts() -> Json<BillboardChartsResponse> {
    Json(BillboardChartsResponse {
        charts: billboard::known_charts(),
    })
}

/// `GET /api/billboard/chart/{slug}?date`
async fn chart(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<BillboardChart>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());
    // An empty `date` is "the current week", not a date to validate.
    let date = params.get("date").filter(|value| !value.is_empty());

    let chart = billboard::get_chart(&state, &slug, date).await?;
    Ok(Json((*chart).clone()))
}

/* --------------------------------------------------------- artwork proxy */

/// How long a browser may keep a piece of artwork. Chart art for a given week
/// never changes, so this is as immutable as a cache header gets.
const ART_CACHE_CONTROL: &str = "public, max-age=86400, immutable";

/// One attempt, and not a long one: the artwork column degrades to a blank cell,
/// so a slow CDN must not hold a connection open behind a panel refresh.
const ART_TIMEOUT: Duration = Duration::from_secs(15);

/// The client the artwork proxy fetches with, and the reason this module has a
/// client of its own.
///
/// **It does not follow redirects.** The allowlist in
/// [`billboard::assert_art_url`] is checked against the URL the caller supplied;
/// a followed redirect is resolved *after* that check, so an allowlisted host
/// could bounce the request to `169.254.169.254` or to localhost and the
/// allowlist would have decided nothing. Billboard's CDN serves artwork
/// directly, so refusing to follow costs nothing and closes the hole.
///
/// The shared [`Http`] in [`AppState`] follows up to ten redirects, which every
/// scrape here needs; weakening it for this one route would trade a real
/// capability for a hole, so the route carries its own client instead.
///
/// Public because a test cannot reach this code path through the URL: the
/// allowlist admits only `https://` billboard.com hosts, so no fixture server
/// can stand in for one. [`fetch_art`] is exercised against a fixture directly.
pub fn art_http() -> &'static Http {
    static ART_HTTP: LazyLock<Http> = LazyLock::new(|| {
        Http::with_client(
            reqwest::Client::builder()
                .gzip(true)
                .brotli(true)
                .deflate(true)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("the artwork HTTP client could not be built"),
        )
    });
    &ART_HTTP
}

/// Fetch one piece of artwork, and prove it is artwork.
///
/// Returns the body and the content type to pass through. Separated from the
/// handler so the three failures that matter — a redirect, a body that is not an
/// image, and one that is too big — are testable against a fixture server; see
/// [`art_http`] for why the handler itself is not.
pub async fn fetch_art(http: &Http, url: &str) -> Result<(Vec<u8>, String)> {
    let (bytes, content_type) = http
        .fetch_bytes(
            url,
            FetchOptions::new()
                // Not the browser page header set: this is an image request, and
                // the two headers below are the two the CDN actually reads.
                .browser_headers(false)
                .header("Accept", "image/*")
                // Billboard's CDN turns away requests with no referrer.
                .header("Referer", "https://www.billboard.com/")
                .timeout(ART_TIMEOUT)
                // The panel redraws on its own; a retry storm against a CDN that
                // is refusing us helps nobody.
                .retries(0)
                .max_bytes(billboard::MAX_ART_BYTES),
        )
        .await?;

    let content_type = content_type.unwrap_or_default();
    if !content_type.starts_with("image/") {
        return Err(UpstreamError::new(
            "Artwork URL did not return an image",
            codes::BAD_UPSTREAM_BODY,
        ));
    }

    Ok((bytes, content_type))
}

/// `GET /api/billboard/art?u=`
///
/// Billboard serves art from a CDN that rejects some referrers and is not
/// reachable from every network the terminal runs on. Routing it through the
/// server keeps the page on one origin and makes the column work everywhere the
/// API itself works.
async fn art(RawQuery(raw): RawQuery) -> Result<Response> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());
    let target = billboard::assert_art_url(params.get("u").unwrap_or_default())?;

    let (bytes, content_type) = fetch_art(art_http(), target.as_str()).await?;

    let content_type = HeaderValue::from_str(&content_type).map_err(|_| {
        UpstreamError::new(
            "Artwork URL returned an unusable content type",
            codes::BAD_UPSTREAM_BODY,
        )
    })?;

    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(CONTENT_TYPE, content_type);
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static(ART_CACHE_CONTROL));
    Ok(response)
}

#[cfg(test)]
mod tests {
    //! The artwork proxy's fetch half.
    //!
    //! The allowlist is tested in `sources::billboard`; what is left is what
    //! happens once a URL has passed it, and the case that matters most is the
    //! redirect — the one failure that would turn this route back into the open
    //! relay the allowlist exists to prevent.

    use super::*;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A one-pixel GIF, which is a real image with a real content type.
    const PIXEL: &[u8] =
        b"GIF89a\x01\x00\x01\x00\x00\xff\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x00;";

    #[tokio::test]
    async fn passes_an_image_through_with_its_content_type() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/art.gif"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(PIXEL, "image/gif"))
            .mount(&server)
            .await;

        let (bytes, content_type) = fetch_art(art_http(), &format!("{}/art.gif", server.uri()))
            .await
            .expect("a GIF is artwork");

        assert_eq!(bytes, PIXEL);
        assert_eq!(content_type, "image/gif");
    }

    #[tokio::test]
    async fn refuses_to_follow_a_redirect() {
        // The whole point of the allowlist is that the caller cannot choose the
        // destination. A followed redirect resolves after the check, so the
        // allowlisted host would be choosing it for them.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/art.gif"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/internal.gif"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/internal.gif"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(PIXEL, "image/gif"))
            .expect(0)
            .mount(&server)
            .await;

        let err = fetch_art(art_http(), &format!("{}/art.gif", server.uri()))
            .await
            .expect_err("a redirect is not artwork");

        assert_eq!(err.code, codes::UPSTREAM_STATUS);
        assert_eq!(err.status, Some(302));
        // `expect(0)` on the redirect target is verified when the server drops.
    }

    #[tokio::test]
    async fn refuses_a_body_that_is_not_an_image() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<!doctype html>", "text/html"))
            .mount(&server)
            .await;

        let err = fetch_art(art_http(), &format!("{}/art.gif", server.uri()))
            .await
            .expect_err("HTML is not artwork");

        assert_eq!(err.code, codes::BAD_UPSTREAM_BODY);
    }

    #[tokio::test]
    async fn refuses_a_missing_content_type() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(PIXEL.to_vec()))
            .mount(&server)
            .await;

        // wiremock declares `application/octet-stream` for a raw byte body; a
        // type that is not `image/*` is refused however it is spelled.
        let err = fetch_art(art_http(), &format!("{}/art", server.uri()))
            .await
            .expect_err("an unlabelled body is not artwork");

        assert_eq!(err.code, codes::BAD_UPSTREAM_BODY);
    }

    #[tokio::test]
    async fn refuses_a_body_over_the_cap() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(vec![b'x'; billboard::MAX_ART_BYTES + 1], "image/gif"),
            )
            .mount(&server)
            .await;

        let err = fetch_art(art_http(), &format!("{}/fat.gif", server.uri()))
            .await
            .expect_err("3 MiB is already generous for a thumbnail");

        assert_eq!(err.code, codes::RESPONSE_TOO_LARGE);
    }

    #[tokio::test]
    async fn reports_an_upstream_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            // One attempt: the panel redraws on its own.
            .expect(1)
            .mount(&server)
            .await;

        let err = fetch_art(art_http(), &format!("{}/art.gif", server.uri()))
            .await
            .expect_err("403");

        assert_eq!(err.code, codes::UPSTREAM_STATUS);
        assert_eq!(err.status, Some(403));
    }
}
