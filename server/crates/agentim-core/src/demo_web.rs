//! The demo at `/` (docs/12 §3.4): the React app built on the browser SDK
//! and the components, embedded at compile time (`build.rs`), or a page
//! saying how to get it.

use axum::{Router, extract::Path, response::Response, routing::get};

mod assets {
    include!(concat!(env!("OUT_DIR"), "/demo_web.rs"));
}

const PLACEHOLDER: &str = "<!doctype html><meta charset=utf-8><title>AgentIM demo</title>\
<body><h1>The demo is not in this build</h1>\
<p>Install a release or the Docker image, or build it first: \
<code>sh server/demo/build.sh</code>, then <code>cargo build</code> again.</p>";

// The SDK compiles its core with WebAssembly ('wasm-unsafe-eval'); it
// talks to this server only (WebSocket included in 'self').
const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; img-src 'self' data:; style-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

pub fn embedded() -> bool {
    !assets::FILES.is_empty()
}

fn answer(path: &str) -> Response {
    crate::console::serve(assets::FILES, path, PLACEHOLDER, CSP)
}

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/", get(|| async { answer("index.html") }))
        .route(
            "/assets/{*path}",
            get(|Path(p): Path<String>| async move { answer(&format!("assets/{p}")) }),
        )
}

#[cfg(test)]
mod tests {
    use axum::http::{StatusCode, header};

    use crate::console::serve;

    static FILES: &[(&str, &[u8])] = &[
        ("index.html", b"<!doctype html>app"),
        ("assets/index-abc.js", b"js"),
        ("assets/core-abc.wasm", b"\0asm"),
    ];

    fn get(files: &'static [(&str, &[u8])], path: &str) -> (StatusCode, String, String, String) {
        let r = serve(files, path, super::PLACEHOLDER, super::CSP);
        let h = |k| {
            r.headers()
                .get(k)
                .map(|v| v.to_str().unwrap().to_string())
                .unwrap_or_default()
        };
        (
            r.status(),
            h(header::CONTENT_TYPE),
            h(header::CACHE_CONTROL),
            h(header::CONTENT_SECURITY_POLICY),
        )
    }

    #[test]
    fn assets_are_cached_for_good_and_the_page_never() {
        let (s, ty, cache, _) = get(FILES, "assets/index-abc.js");
        assert_eq!(
            (s, ty.as_str(), cache.as_str()),
            (
                StatusCode::OK,
                "text/javascript; charset=utf-8",
                "public, max-age=31536000, immutable"
            )
        );
        let (_, ty, cache, _) = get(FILES, "assets/core-abc.wasm");
        // The type streaming compilation needs.
        assert_eq!(
            (ty.as_str(), cache.as_str()),
            ("application/wasm", "public, max-age=31536000, immutable")
        );
        for p in ["index.html", "", "anything/else", "assets/missing.js"] {
            let (s, ty, cache, _) = get(FILES, p);
            assert_eq!(
                (s, ty.as_str(), cache.as_str()),
                (StatusCode::OK, "text/html; charset=utf-8", "no-store"),
                "{p}"
            );
        }
    }

    #[test]
    fn the_page_may_compile_wasm_and_nothing_else_loosens() {
        let (_, _, _, csp) = get(FILES, "index.html");
        assert!(
            csp.contains("script-src 'self' 'wasm-unsafe-eval'"),
            "{csp}"
        );
        assert!(
            !csp.contains("unsafe-inline")
                && !csp.contains("'unsafe-eval'")
                && csp.contains("frame-ancestors 'none'"),
            "{csp}"
        );
    }

    #[test]
    fn without_a_build_it_says_how_to_get_one() {
        let r = serve(&[], "index.html", super::PLACEHOLDER, super::CSP);
        assert_eq!(r.status(), StatusCode::OK);
        let (_, _, cache, _) = get(&[], "assets/x.js");
        assert_eq!(cache, "no-store");
        assert!(super::PLACEHOLDER.contains("server/demo/build.sh"));
    }
}
