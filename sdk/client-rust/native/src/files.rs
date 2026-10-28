//! Files on the native runtime (docs/02-architecture.md §2.7): before the
//! core sees a file, its EXIF and XMP (GPS) are removed and an image gets
//! its thumbnail; then the core's `Put`s are carried out as HTTP PUTs of a
//! byte range of the file, with progress.
//!
//! The app sends `send_file {conv_id, path, name?, mime?, caption?,
//! reply_to?, client_msg_id?, keep_metadata?}`; the runtime turns it into
//! the core's `send_file` (a source path, the size, the thumbnail).

use std::io::Write;
use std::path::{Path, PathBuf};

use agentim_client_core::{ClientError, PutResult, media};
use serde_json::{Value, json};

/// Images up to this size have their metadata removed (it needs the
/// whole image in memory); larger ones go as they are (no camera writes
/// such JPEGs, PNGs or WebPs).
pub const STRIP_MAX: u64 = 256 << 20;
/// Images up to this many pixels get a thumbnail; larger are sent as files.
pub const THUMB_MAX_PIXELS: u64 = 40_000_000;
/// Longest side of a thumbnail (as the server checks).
pub const THUMB_SIDE: u32 = agentim_client_core::upload::THUMB_SIDE;

fn bad(m: impl Into<String>) -> ClientError {
    ClientError::new("bad_request", m)
}

fn mime_of(kind: Option<&str>, b: &[u8]) -> Option<&'static str> {
    match kind {
        Some("jpeg") => Some("image/jpeg"),
        Some("png") => Some("image/png"),
        Some("webp") => Some("image/webp"),
        _ if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") => Some("image/gif"),
        _ => None,
    }
}

/// Writes `data` to a new file in `dir` (named at random).
fn write_new(dir: &Path, ext: &str, data: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let mut b = [0u8; 12];
    let _ = getrandom::fill(&mut b);
    let name: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let p = dir.join(format!("{name}.{ext}"));
    let mut f = std::fs::File::create_new(&p)?;
    f.write_all(data)?;
    f.sync_all()?;
    Ok(p)
}

/// A thumbnail: its bytes, type, width and height.
pub type Thumb = (Vec<u8>, &'static str, u32, u32);

/// The thumbnail of an image (turned upright, longest side 320): JPEG,
/// or PNG when it has transparency. None when it is small enough to be
/// its own thumbnail, too large, or cannot be decoded. Also the upright
/// size of the original.
pub fn thumbnail(b: &[u8]) -> (Option<Thumb>, Option<(u32, u32)>) {
    use image::ImageEncoder;
    let Ok(reader) = image::ImageReader::new(std::io::Cursor::new(b)).with_guessed_format() else {
        return (None, None);
    };
    let Ok((w, h)) = reader.into_dimensions() else {
        return (None, None);
    };
    let o = media::orientation(b).unwrap_or(1);
    let upright = if (5..=8).contains(&o) { (h, w) } else { (w, h) };
    if w as u64 * h as u64 > THUMB_MAX_PIXELS || w.max(h) <= THUMB_SIDE {
        return (None, Some(upright));
    }
    let Ok(img) = image::load_from_memory(b) else {
        return (None, Some(upright));
    };
    let img = match o {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    };
    let t = img.thumbnail(THUMB_SIDE, THUMB_SIDE);
    let (tw, th) = (t.width(), t.height());
    let mut out = vec![];
    let r = if t.color().has_alpha() {
        t.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .ok()
            .map(|_| "image/png")
    } else {
        let rgb = t.to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 75)
            .write_image(rgb.as_raw(), tw, th, image::ExtendedColorType::Rgb8)
            .ok()
            .map(|_| "image/jpeg")
    };
    (r.map(|m| (out, m, tw, th)), Some(upright))
}

/// The app's `send_file {path, …}` as the core's `send_file`: copies made
/// here (without metadata, the thumbnail) go to `dir`.
pub fn prepare(cmd: &Value, dir: &Path) -> Result<Value, ClientError> {
    let path = cmd["path"]
        .as_str()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| bad("path is required"))?;
    let meta = std::fs::metadata(path).map_err(|e| bad(format!("cannot read {path}: {e}")))?;
    if !meta.is_file() {
        return Err(bad(format!("{path} is not a file")));
    }
    let name = cmd["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .map(String::from)
        .or_else(|| {
            Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "file".into());
    let mut head = [0u8; 16];
    let n = {
        use std::io::Read;
        let mut f =
            std::fs::File::open(path).map_err(|e| bad(format!("cannot read {path}: {e}")))?;
        f.read(&mut head).unwrap_or(0)
    };
    let head = &head[..n];
    let kind = media::kind(head);
    let image_mime = mime_of(kind, head);
    let mut file = json!({"source": path, "name": name, "size": meta.len()});
    let mut out = json!({"cmd": "send_file"});
    for k in ["conv_id", "caption", "reply_to", "client_msg_id"] {
        if !cmd[k].is_null() {
            out[k] = cmd[k].clone();
        }
    }
    match image_mime {
        Some(mime) if meta.len() <= STRIP_MAX => {
            let data = std::fs::read(path).map_err(|e| bad(format!("cannot read {path}: {e}")))?;
            let (thumb, size) = thumbnail(&data);
            // Location and camera data go unless the app keeps them.
            if cmd["keep_metadata"] != true
                && let Some(clean) = media::strip_metadata(&data)
            {
                let p = write_new(dir, kind.unwrap_or("img"), &clean).map_err(|e| {
                    ClientError::new("storage", format!("cannot write a copy: {e}"))
                })?;
                file["source"] = json!(p.to_string_lossy());
                file["size"] = json!(clean.len());
            }
            file["mime"] = json!(mime);
            if let Some((w, h)) = size {
                file["width"] = json!(w);
                file["height"] = json!(h);
            }
            if let Some((t, tm, tw, th)) = thumb {
                let ext = if tm == "image/png" { "png" } else { "jpg" };
                let p = write_new(dir, ext, &t).map_err(|e| {
                    ClientError::new("storage", format!("cannot write a thumbnail: {e}"))
                })?;
                out["thumb"] = json!({"source": p.to_string_lossy(), "size": t.len(), "width": tw, "height": th});
            }
        }
        Some(mime) => file["mime"] = json!(mime),
        None => {
            if let Some(m) = cmd["mime"].as_str() {
                file["mime"] = json!(m);
            }
        }
    }
    out["file"] = file;
    Ok(out)
}

/// One PUT of `len` bytes of `source` from `offset`, with its exact
/// length (S3 signs it), reporting progress through `progress`.
pub async fn put(
    http: &reqwest::Client,
    url: &str,
    headers: &std::collections::BTreeMap<String, String>,
    source: &str,
    offset: u64,
    len: u64,
    progress: impl Fn(u64) + Send + Sync + 'static,
) -> PutResult {
    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let failed = |status: Option<u16>, detail: String| PutResult::Failed { status, detail };
    let mut f = match tokio::fs::File::open(source).await {
        Ok(f) => f,
        // The source is gone: nothing a retry can fix.
        Err(e) => return PutResult::Unreadable(format!("cannot read {source}: {e}")),
    };
    if let Err(e) = f.seek(std::io::SeekFrom::Start(offset)).await {
        return failed(None, format!("cannot read {source}: {e}"));
    }
    let sent = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counter = sent.clone();
    let last = std::sync::Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let stream = tokio_util::io::ReaderStream::with_capacity(f.take(len), 64 << 10).map(move |c| {
        if let Ok(b) = &c {
            let n = counter.fetch_add(b.len() as u64, std::sync::atomic::Ordering::Relaxed)
                + b.len() as u64;
            // A few times a second at most.
            let mut l = last.lock().unwrap_or_else(|e| e.into_inner());
            if l.elapsed() >= std::time::Duration::from_millis(250) {
                *l = std::time::Instant::now();
                progress(n);
            }
        }
        c
    });
    let mut req = http
        .put(url)
        .header("content-length", len)
        .body(reqwest::Body::wrap_stream(stream));
    for (k, v) in headers {
        if !k.eq_ignore_ascii_case("content-length") {
            req = req.header(k, v);
        }
    }
    match req.send().await {
        Ok(r) if r.status().is_success() => {
            if sent.load(std::sync::atomic::Ordering::Relaxed) != len {
                return failed(None, format!("{source} is shorter than expected"));
            }
            let etag = r
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            PutResult::Done(etag)
        }
        Ok(r) => {
            let status = r.status().as_u16();
            let body = r.text().await.unwrap_or_default();
            failed(Some(status), body.chars().take(300).collect())
        }
        Err(e) => failed(None, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg_with_gps(w: u32, h: u32) -> Vec<u8> {
        let mut out = vec![];
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 7])
        });
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
            .encode_image(&img)
            .unwrap();
        // An APP1 EXIF block (orientation 6, and a GPS pointer) after SOI.
        let mut tiff = b"MM\0\x2a\0\0\0\x08".to_vec();
        tiff.extend_from_slice(&[0, 2]);
        tiff.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0]);
        tiff.extend_from_slice(&[0x88, 0x25, 0, 4, 0, 0, 0, 1, 0, 0, 0, 0x26]);
        tiff.extend_from_slice(&[0, 0, 0, 0]);
        tiff.extend_from_slice(b"GPS 51.5N 0.12W");
        let mut seg = b"Exif\0\0".to_vec();
        seg.extend_from_slice(&tiff);
        let mut app1 = vec![0xFF, 0xE1];
        app1.extend_from_slice(&((seg.len() + 2) as u16).to_be_bytes());
        app1.extend_from_slice(&seg);
        let mut j = out[..2].to_vec();
        j.extend_from_slice(&app1);
        j.extend_from_slice(&out[2..]);
        j
    }

    #[test]
    fn a_photo_loses_its_location_and_gets_an_upright_thumbnail() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("IMG_1.jpg");
        std::fs::write(&photo, jpeg_with_gps(800, 600)).unwrap();
        let cmd = json!({"cmd": "send_file", "conv_id": "c", "path": photo, "caption": "hi"});
        let out = prepare(&cmd, &dir.path().join("uploads")).unwrap();
        assert_eq!(out["conv_id"], "c");
        assert_eq!(out["caption"], "hi");
        let f = &out["file"];
        assert_eq!(
            (f["name"].as_str(), f["mime"].as_str()),
            (Some("IMG_1.jpg"), Some("image/jpeg"))
        );
        // Orientation 6: shown 600 wide, 800 high.
        assert_eq!(
            (f["width"].as_u64(), f["height"].as_u64()),
            (Some(600), Some(800))
        );
        let copy = std::fs::read(f["source"].as_str().unwrap()).unwrap();
        assert_ne!(
            f["source"].as_str().unwrap(),
            photo.to_str().unwrap(),
            "a copy"
        );
        assert_eq!(f["size"].as_u64(), Some(copy.len() as u64));
        assert!(
            !copy.windows(3).any(|w| w == b"GPS"),
            "the location is gone"
        );
        assert_eq!(media::orientation(&copy), Some(6), "the orientation stays");
        let t = &out["thumb"];
        let tb = std::fs::read(t["source"].as_str().unwrap()).unwrap();
        assert_eq!(t["size"].as_u64(), Some(tb.len() as u64));
        let ti = image::load_from_memory(&tb).unwrap();
        assert_eq!(
            (ti.width(), ti.height()),
            (240, 320),
            "upright, longest side 320"
        );
        assert!(tb.starts_with(&[0xFF, 0xD8]), "a JPEG");
        // The app may keep the metadata.
        let out = prepare(
            &json!({"cmd": "send_file", "conv_id": "c", "path": photo, "keep_metadata": true}),
            &dir.path().join("uploads"),
        )
        .unwrap();
        assert_eq!(out["file"]["source"], json!(photo));
    }

    #[test]
    fn small_images_transparent_ones_and_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let up = dir.path().join("uploads");
        // Small: its own thumbnail.
        let small = dir.path().join("s.png");
        image::RgbaImage::from_pixel(100, 50, image::Rgba([1, 2, 3, 128]))
            .save(&small)
            .unwrap();
        let out = prepare(&json!({"conv_id": "c", "path": small}), &up).unwrap();
        assert!(out["thumb"].is_null());
        assert_eq!(out["file"]["width"], 100);
        // Transparent and large: a PNG thumbnail.
        let big = dir.path().join("b.png");
        image::RgbaImage::from_pixel(1000, 500, image::Rgba([1, 2, 3, 128]))
            .save(&big)
            .unwrap();
        let out = prepare(&json!({"conv_id": "c", "path": big}), &up).unwrap();
        let tb = std::fs::read(out["thumb"]["source"].as_str().unwrap()).unwrap();
        assert!(tb.starts_with(b"\x89PNG"));
        assert_eq!(
            (
                out["thumb"]["width"].as_u64(),
                out["thumb"]["height"].as_u64()
            ),
            (Some(320), Some(160))
        );
        // A document: as it is, with the app's type.
        let doc = dir.path().join("report.pdf");
        std::fs::write(&doc, b"%PDF-1.7 ...").unwrap();
        let out = prepare(
            &json!({"conv_id": "c", "path": doc, "mime": "application/pdf"}),
            &up,
        )
        .unwrap();
        assert_eq!(
            out["file"],
            json!({"source": doc, "name": "report.pdf", "size": 12, "mime": "application/pdf"})
        );
        assert!(out["thumb"].is_null());
        // A broken "image": a file without a thumbnail.
        let broken = dir.path().join("x.jpg");
        std::fs::write(&broken, [0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0]).unwrap();
        let out = prepare(&json!({"conv_id": "c", "path": broken}), &up).unwrap();
        assert!(out["thumb"].is_null() && out["file"]["width"].is_null());
        // Refused: no path, a folder, missing.
        for bad in [
            json!({"conv_id": "c"}),
            json!({"conv_id": "c", "path": dir.path()}),
            json!({"conv_id": "c", "path": "/nope/x"}),
        ] {
            assert_eq!(prepare(&bad, &up).unwrap_err().code, "bad_request", "{bad}");
        }
    }
}
