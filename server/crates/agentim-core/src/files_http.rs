//! The local folder's addresses (docs/02-architecture.md §2.7): parts and
//! thumbnails are PUT to `/files/u/…`, files are read from `/files/d/…`.
//! Both are signed by the server and short-lived, like S3's presigned
//! addresses; no token is needed, and any site may use them (CORS `*`):
//! the signature is the permission.

use axum::{
    Router,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::filestore::Store;
use crate::model::now_ms;
use crate::server::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/files/u/{conv}/{file}/{part}",
            put(upload).options(preflight),
        )
        .route(
            "/files/d/{conv}/{file}/{variant}",
            get(download).options(preflight),
        )
}

fn cors(mut r: Response) -> Response {
    let h = r.headers_mut();
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    h.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("ETag, Content-Length, Content-Range"),
    );
    r
}

async fn preflight(headers: HeaderMap) -> Response {
    let mut r = StatusCode::NO_CONTENT.into_response();
    let h = r.headers_mut();
    h.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, PUT, OPTIONS"),
    );
    let asked = headers
        .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("content-type"));
    h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, asked);
    h.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("600"),
    );
    cors(r)
}

fn refuse(status: StatusCode, msg: &'static str) -> Response {
    cors((status, msg).into_response())
}

#[derive(Deserialize)]
struct UploadQ {
    len: u64,
    exp: i64,
    sig: String,
}

async fn upload(
    State(st): State<AppState>,
    Path((conv, file, part)): Path<(String, String, String)>,
    Query(q): Query<UploadQ>,
    body: Body,
) -> Response {
    let store = st.hub.file_store();
    if !store.verify(
        &Store::upload_msg(&conv, &file, &part, q.len, q.exp),
        &q.sig,
        q.exp,
        now_ms(),
    ) {
        return refuse(
            StatusCode::FORBIDDEN,
            "the address is wrong or expired; ask for new ones",
        );
    }
    let Some(path) = store.local_path(&conv, &file, &part) else {
        return refuse(StatusCode::NOT_FOUND, "not stored here");
    };
    // A thrown away upload takes no more parts (no orphans in the folder).
    if !st.hub.upload_open(&conv, &file).await {
        return refuse(StatusCode::NOT_FOUND, "the upload is complete or gone");
    }
    match write_part(&path, body, q.len).await {
        Ok(()) => {
            let mut r = StatusCode::OK.into_response();
            if let Ok(v) = HeaderValue::from_str(&format!("\"{part}\"")) {
                r.headers_mut().insert(header::ETAG, v);
            }
            cors(r)
        }
        Err(Wrong::Size) => refuse(
            StatusCode::BAD_REQUEST,
            "the body does not have the signed length",
        ),
        Err(Wrong::Io(e)) => {
            tracing::warn!(error = %e, "writing an uploaded part");
            refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "could not store the part; retry",
            )
        }
    }
}

enum Wrong {
    Size,
    Io(std::io::Error),
}

/// Exactly `len` bytes, into a temporary file renamed into place: a part is
/// either whole or not there. Written again, it is replaced.
async fn write_part(path: &std::path::Path, body: Body, len: u64) -> Result<(), Wrong> {
    let dir = path.parent().ok_or(Wrong::Size)?;
    tokio::fs::create_dir_all(dir).await.map_err(Wrong::Io)?;
    let tmp = path.with_extension(format!("tmp{}", uuid::Uuid::new_v4().simple()));
    let mut f = tokio::fs::File::create(&tmp).await.map_err(Wrong::Io)?;
    let mut n = 0u64;
    let mut stream = body.into_data_stream();
    let result = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| Wrong::Io(std::io::Error::other(e.to_string())))?;
            n += chunk.len() as u64;
            if n > len {
                return Err(Wrong::Size);
            }
            f.write_all(&chunk).await.map_err(Wrong::Io)?;
        }
        if n != len {
            return Err(Wrong::Size);
        }
        f.sync_all().await.map_err(Wrong::Io)?;
        Ok(())
    }
    .await;
    drop(f);
    match result {
        Ok(()) => tokio::fs::rename(&tmp, path).await.map_err(Wrong::Io),
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(e)
        }
    }
}

#[derive(Deserialize)]
struct DownloadQ {
    #[serde(rename = "type")]
    mime: String,
    disp: String,
    exp: i64,
    sig: String,
}

/// One `bytes=a-b`, `bytes=a-` or `bytes=-n` range of a file of `size`.
pub fn parse_range(v: &str, size: u64) -> Option<(u64, u64)> {
    let r = v.trim().strip_prefix("bytes=")?;
    if r.contains(',') || size == 0 {
        return None;
    }
    let (a, b) = r.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            if n == 0 {
                return None;
            }
            (size.saturating_sub(n), size - 1)
        }
        (a, "") => (a.parse().ok()?, size - 1),
        (a, b) => (a.parse().ok()?, b.parse::<u64>().ok()?.min(size - 1)),
    };
    (start <= end && start < size).then_some((start, end))
}

async fn download(
    State(st): State<AppState>,
    method: Method,
    Path((conv, file, variant)): Path<(String, String, String)>,
    Query(q): Query<DownloadQ>,
    headers: HeaderMap,
) -> Response {
    let store = st.hub.file_store();
    let msg = Store::download_msg(&conv, &file, &variant, &q.mime, &q.disp, q.exp);
    if !store.verify(&msg, &q.sig, q.exp, now_ms()) {
        return refuse(
            StatusCode::FORBIDDEN,
            "the address is wrong or expired; ask for a new one",
        );
    }
    if variant != "original" && variant != "thumb" {
        return refuse(StatusCode::NOT_FOUND, "no such variant");
    }
    let Some(path) = store.local_path(&conv, &file, &variant) else {
        return refuse(StatusCode::NOT_FOUND, "not stored here");
    };
    let Ok(mut f) = tokio::fs::File::open(&path).await else {
        return refuse(StatusCode::NOT_FOUND, "the file is gone");
    };
    let size = match f.metadata().await {
        Ok(m) => m.len(),
        Err(_) => return refuse(StatusCode::NOT_FOUND, "the file is gone"),
    };
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|v| parse_range(v, size));
    let (status, start, end) = match range {
        None => (StatusCode::OK, 0, size.saturating_sub(1)),
        Some(Some((a, b))) => (StatusCode::PARTIAL_CONTENT, a, b),
        Some(None) => {
            let mut r = refuse(StatusCode::RANGE_NOT_SATISFIABLE, "range");
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{size}")) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            return r;
        }
    };
    let len = if size == 0 { 0 } else { end - start + 1 };
    let body = if method == Method::HEAD || len == 0 {
        Body::empty()
    } else {
        if f.seek(std::io::SeekFrom::Start(start)).await.is_err() {
            return refuse(StatusCode::SERVICE_UNAVAILABLE, "could not read the file");
        }
        Body::from_stream(tokio_util_stream(f.take(len)))
    };
    let mut r = (status, body).into_response();
    let h = r.headers_mut();
    let set = |h: &mut HeaderMap, k: header::HeaderName, v: &str| {
        if let Ok(v) = HeaderValue::from_str(v) {
            h.insert(k, v);
        }
    };
    set(h, header::CONTENT_TYPE, &q.mime);
    set(h, header::CONTENT_DISPOSITION, &q.disp);
    set(h, header::CONTENT_LENGTH, &len.to_string());
    set(h, header::ACCEPT_RANGES, "bytes");
    // The content never changes; only the address expires.
    set(
        h,
        header::CACHE_CONTROL,
        "private, max-age=31536000, immutable",
    );
    set(h, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(
        h,
        header::CONTENT_SECURITY_POLICY,
        "sandbox; default-src 'none'",
    );
    if status == StatusCode::PARTIAL_CONTENT {
        set(
            h,
            header::CONTENT_RANGE,
            &format!("bytes {start}-{end}/{size}"),
        );
    }
    cors(r)
}

/// Reads a file in 64 KiB chunks as a body stream.
fn tokio_util_stream<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    r: R,
) -> impl futures_util::Stream<Item = std::io::Result<Vec<u8>>> + Send {
    futures_util::stream::unfold(r, |mut r| async move {
        let mut buf = vec![0u8; 64 << 10];
        match r.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(buf), r))
            }
            Err(e) => Some((Err(e), r)),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::parse_range;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-9", 100), Some((0, 9)));
        assert_eq!(parse_range("bytes=90-", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-200", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=50-500", 100), Some((50, 99)));
        assert_eq!(parse_range("bytes=99-99", 100), Some((99, 99)));
        for bad in [
            "bytes=100-",
            "bytes=5-4",
            "bytes=-0",
            "bytes=a-b",
            "items=0-1",
            "bytes=0-1,3-4",
            "bytes=-",
        ] {
            assert_eq!(parse_range(bad, 100), None, "{bad}");
        }
        assert_eq!(parse_range("bytes=0-0", 0), None);
    }
}
