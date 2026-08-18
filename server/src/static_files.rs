//! Serving the built client.
//!
//! In production the terminal is one process on one port: the same server that
//! answers `/api` also hands out the SvelteKit build. In development this is
//! switched off — Vite serves the client on :5173 and proxies `/api` here — so
//! the whole thing is optional and absent unless `CLIENT_DIR` names a real
//! directory.

use std::path::{Path, PathBuf};

use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;

/// How long a browser may keep an asset. An hour, as the Express server said:
/// long enough that a reload is cheap, short enough that a deploy is visible
/// without a hard refresh.
const CACHE_CONTROL: &str = "public, max-age=3600";

/// Attach static serving and the SPA fallback to `router`, if `client_dir` is
/// set and exists.
///
/// The fallback is what makes a hard reload on any path work: the terminal has
/// exactly one page, so anything that is not a file and not `/api` is answered
/// with `index.html` and routed client-side.
pub fn attach(router: Router, client_dir: Option<&Path>) -> Router {
    let Some(dir) = client_dir else {
        return router.fallback(no_client);
    };

    if !dir.is_dir() {
        tracing::warn!(
            path = %dir.display(),
            "CLIENT_DIR does not exist; serving the API only"
        );
        return router.fallback(no_client);
    }

    let index = dir.join("index.html");
    if !index.is_file() {
        tracing::warn!(
            path = %index.display(),
            "CLIENT_DIR has no index.html; serving the API only"
        );
        return router.fallback(no_client);
    }

    tracing::info!(path = %dir.display(), "serving the built client");

    let service = ServeDir::new(dir)
        // A directory request must not silently become index.html here; the
        // SPA fallback below owns that, and owning it twice makes a missing
        // asset look like a working page.
        .append_index_html_on_directories(false)
        .fallback(ServeFile::new(index));

    router
        .fallback_service(service)
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static(CACHE_CONTROL),
        ))
}

/// What a non-API path answers when no client is built.
///
/// Worth being explicit rather than serving a bare 404: hitting the API port in
/// a browser during development is a normal thing to do, and the useful answer
/// is "you want the Vite server", not "not found".
async fn no_client() -> Response {
    (
        StatusCode::NOT_FOUND,
        "No client build is being served.\n\n\
         In development the client runs under Vite on http://127.0.0.1:5173 and \
         proxies /api here.\n\
         In production, build it (`make build`) and set CLIENT_DIR to the output \
         directory.\n",
    )
        .into_response()
}

/// Resolve `CLIENT_DIR` against the executable as well as the working
/// directory, so a deployed binary finds its client without being launched from
/// a particular place.
pub fn resolve(configured: Option<&Path>) -> Option<PathBuf> {
    let configured = configured?;

    if configured.is_dir() {
        return Some(configured.to_path_buf());
    }

    if configured.is_relative() {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                let beside = exe_dir.join(configured);
                if beside.is_dir() {
                    return Some(beside);
                }
            }
        }
    }

    // Hand back the configured path even though it is missing, so `attach` logs
    // one clear warning naming what was actually looked for.
    Some(configured.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use axum::routing::get;
    use tower::ServiceExt;

    fn api_router() -> Router {
        Router::new().route("/api/health", get(|| async { "ok" }))
    }

    async fn body_text(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn build_a_client(dir: &Path) {
        std::fs::create_dir_all(dir.join("_app")).unwrap();
        std::fs::write(
            dir.join("index.html"),
            "<!doctype html><title>terminal</title>",
        )
        .unwrap();
        std::fs::write(dir.join("_app/app.js"), "console.log('app')").unwrap();
    }

    /// A scratch directory that cleans itself up.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("terminal-static-{name}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn serves_an_asset_with_an_hour_of_cache_control() {
        let dir = TempDir::new("assets");
        build_a_client(&dir.0);
        let app = attach(api_router(), Some(&dir.0));

        let response = app
            .oneshot(
                Request::get("/_app/app.js")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("cache-control").unwrap(),
            "public, max-age=3600"
        );
        assert_eq!(body_text(response).await, "console.log('app')");
    }

    #[tokio::test]
    async fn answers_an_unknown_path_with_the_app_so_a_hard_reload_works() {
        let dir = TempDir::new("fallback");
        build_a_client(&dir.0);
        let app = attach(api_router(), Some(&dir.0));

        let response = app
            .oneshot(
                Request::get("/some/deep/panel/route")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_text(response).await.contains("<!doctype html>"));
    }

    #[tokio::test]
    async fn the_api_still_wins_over_the_fallback() {
        let dir = TempDir::new("api-precedence");
        build_a_client(&dir.0);
        let app = attach(api_router(), Some(&dir.0));

        let response = app
            .oneshot(
                Request::get("/api/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_text(response).await, "ok");
    }

    #[tokio::test]
    async fn explains_itself_when_no_client_is_built() {
        let app = attach(api_router(), None);

        let response = app
            .oneshot(Request::get("/").body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_text(response).await;
        assert!(
            body.contains("5173"),
            "should point at the Vite server: {body}"
        );
    }

    #[tokio::test]
    async fn a_configured_but_missing_directory_degrades_to_api_only() {
        let missing = std::env::temp_dir().join("terminal-static-definitely-absent");
        let _ = std::fs::remove_dir_all(&missing);
        let app = attach(api_router(), Some(&missing));

        let response = app
            .oneshot(Request::get("/").body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // The API is unaffected.
        let app = attach(api_router(), Some(&missing));
        let response = app
            .oneshot(
                Request::get("/api/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn resolve_returns_an_existing_directory_unchanged() {
        let dir = TempDir::new("resolve");
        build_a_client(&dir.0);
        assert_eq!(resolve(Some(&dir.0)), Some(dir.0.clone()));
        assert_eq!(resolve(None), None);
    }
}
