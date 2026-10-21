//! The console at `/console` (docs/12 §4): the built single-page app
//! embedded at compile time (`build.rs`), or a page saying how to get it.

use axum::{
    Router,
    extract::Path,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::get,
};

mod assets {
    include!(concat!(env!("OUT_DIR"), "/console.rs"));
}

const PLACEHOLDER: &str = "<!doctype html><meta charset=utf-8><title>AgentIM console</title>\
<body><h1>The console is not in this build</h1>\
<p>Install a release or the Docker image, or build it first: \
<code>cd server/console &amp;&amp; npm ci &amp;&amp; npm run build</code>, then <code>cargo build</code> again.</p>\
<p>The admin API (<code>/admin/v1</code>) and <code>agentim admin</code> work without it.</p>";

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

pub fn embedded() -> bool {
    !assets::FILES.is_empty()
}

fn file(path: &str) -> Option<&'static [u8]> {
    assets::FILES
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, b)| *b)
}

fn answer(path: &str) -> Response {
    // Hashed asset names never change; anything else is the app's page
    // (client-side routes such as /console/users).
    let (name, body, cache) = match file(path) {
        Some(b) if path.starts_with("assets/") => (path, b, "public, max-age=31536000, immutable"),
        _ => match file("index.html") {
            Some(b) => ("index.html", b, "no-store"),
            None => ("index.html", PLACEHOLDER.as_bytes(), "no-store"),
        },
    };
    let mut r = (StatusCode::OK, body).into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime(name)));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data:; style-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    // The setup link carries its token in the URL.
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    r
}

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route(
            "/console",
            get(|| async { Redirect::permanent("/console/") }),
        )
        .route("/console/", get(|| async { answer("index.html") }))
        .route(
            "/console/{*path}",
            get(|Path(p): Path<String>| async move { answer(&p) }),
        )
}
