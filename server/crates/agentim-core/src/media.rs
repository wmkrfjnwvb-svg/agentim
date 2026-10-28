//! Image metadata (docs/02-architecture.md §2.7): before an image is
//! uploaded, EXIF and XMP (where GPS positions live) are removed without
//! re-encoding: the pixels stay exactly as they were. A JPEG keeps its
//! orientation in a minimal EXIF block of its own, so it still shows the
//! right way up. The same file is in the server (`server/.../media.rs`);
//! a test there checks they are identical.

/// What kind of image the bytes are, if one this module handles.
pub fn kind(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpeg")
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// The image without EXIF and XMP, or None when there was nothing to
/// remove (or it is not an image this handles, or it is malformed: then
/// it is uploaded as it is).
pub fn strip_metadata(b: &[u8]) -> Option<Vec<u8>> {
    match kind(b)? {
        "jpeg" => strip_jpeg(b),
        "png" => strip_png(b),
        "webp" => strip_webp(b),
        _ => None,
    }
}

/// EXIF orientation (1–8) of a JPEG, if it has one.
pub fn orientation(b: &[u8]) -> Option<u16> {
    if kind(b)? != "jpeg" {
        return None;
    }
    let mut i = 2;
    while i + 4 <= b.len() && b[i] == 0xFF {
        let marker = b[i + 1];
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if len < 2 || i + 2 + len > b.len() {
            return None;
        }
        let seg = &b[i + 4..i + 2 + len];
        if marker == 0xE1 && seg.starts_with(b"Exif\0\0") {
            return exif_orientation(&seg[6..]);
        }
        i += 2 + len;
    }
    None
}

fn exif_orientation(t: &[u8]) -> Option<u16> {
    if t.len() < 8 {
        return None;
    }
    let be = match &t[..2] {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |o: usize| -> Option<u16> {
        let s = t.get(o..o + 2)?;
        Some(if be {
            u16::from_be_bytes([s[0], s[1]])
        } else {
            u16::from_le_bytes([s[0], s[1]])
        })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let s = t.get(o..o + 4)?;
        let a = [s[0], s[1], s[2], s[3]];
        Some(if be {
            u32::from_be_bytes(a)
        } else {
            u32::from_le_bytes(a)
        })
    };
    let ifd = u32_at(4)? as usize;
    let n = u16_at(ifd)? as usize;
    for k in 0..n.min(512) {
        let e = ifd + 2 + k * 12;
        if u16_at(e)? == 0x0112 {
            let v = u16_at(e + 8)?;
            return (1..=8).contains(&v).then_some(v);
        }
    }
    None
}

/// A minimal EXIF segment with only the orientation (big-endian TIFF).
fn orientation_segment(o: u16) -> Vec<u8> {
    let mut tiff = vec![];
    tiff.extend_from_slice(b"MM\0\x2a\0\0\0\x08");
    tiff.extend_from_slice(&1u16.to_be_bytes());
    tiff.extend_from_slice(&0x0112u16.to_be_bytes());
    tiff.extend_from_slice(&3u16.to_be_bytes()); // SHORT
    tiff.extend_from_slice(&1u32.to_be_bytes());
    tiff.extend_from_slice(&o.to_be_bytes());
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&0u32.to_be_bytes()); // no next IFD
    let mut seg = vec![0xFF, 0xE1];
    let len = (2 + 6 + tiff.len()) as u16;
    seg.extend_from_slice(&len.to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&tiff);
    seg
}

fn strip_jpeg(b: &[u8]) -> Option<Vec<u8>> {
    let o = orientation(b);
    let mut out = Vec::with_capacity(b.len());
    out.extend_from_slice(&b[..2]);
    let mut i = 2;
    let mut removed = false;
    let mut wrote_orientation = false;
    loop {
        if i + 4 > b.len() || b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        // Start of scan: the rest is image data, copied as it is.
        if marker == 0xDA {
            out.extend_from_slice(&b[i..]);
            break;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if len < 2 || i + 2 + len > b.len() {
            return None;
        }
        let seg = &b[i..i + 2 + len];
        let body = &seg[4..];
        let meta = (marker == 0xE1 && (body.starts_with(b"Exif\0\0") || body.starts_with(b"http://ns.adobe.com/xap/1.0/\0") || body.starts_with(b"http://ns.adobe.com/xmp/extension/\0")))
            // Photoshop resources (APP13) can carry IPTC with places too.
            || marker == 0xED;
        if meta {
            removed = true;
            if let (Some(o), false) = (o, wrote_orientation) {
                out.extend_from_slice(&orientation_segment(o));
                wrote_orientation = true;
            }
        } else {
            out.extend_from_slice(seg);
        }
        i += 2 + len;
    }
    removed.then_some(out)
}

fn strip_png(b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(b.len());
    out.extend_from_slice(&b[..8]);
    let mut i = 8;
    let mut removed = false;
    while i + 12 <= b.len() {
        let len = u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize;
        let end = i.checked_add(12)?.checked_add(len)?;
        if end > b.len() {
            return None;
        }
        let ty = &b[i + 4..i + 8];
        // EXIF and text chunks (XMP lives in iTXt) go.
        if matches!(ty, b"eXIf" | b"iTXt" | b"tEXt" | b"zTXt") {
            removed = true;
        } else {
            out.extend_from_slice(&b[i..end]);
        }
        i = end;
        if ty == b"IEND" {
            break;
        }
    }
    removed.then_some(out)
}

fn strip_webp(b: &[u8]) -> Option<Vec<u8>> {
    let mut chunks = vec![];
    let mut i = 12;
    let mut removed = false;
    while i + 8 <= b.len() {
        let len = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
        let padded = len + (len & 1);
        let end = i.checked_add(8)?.checked_add(padded)?;
        if end > b.len() {
            return None;
        }
        let ty = &b[i..i + 4];
        if ty == b"EXIF" || ty == b"XMP " {
            removed = true;
        } else {
            chunks.push(b[i..end].to_vec());
        }
        i = end;
    }
    if !removed {
        return None;
    }
    // The extended header says which chunks follow: EXIF (0x08), XMP (0x04).
    for c in chunks.iter_mut() {
        if &c[..4] == b"VP8X" && c.len() > 8 {
            c[8] &= !(0x08 | 0x04);
        }
    }
    let body: usize = chunks.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(12 + body);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((4 + body) as u32).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    for c in chunks {
        out.extend_from_slice(&c);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny JPEG: SOI, APP0, an EXIF APP1 with GPS-like data and
    /// orientation 6, an XMP APP1, then the scan.
    fn jpeg(with_exif: bool, little: bool) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8];
        b.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x07, b'J', b'F', b'I', b'F', 0]);
        if with_exif {
            let mut tiff: Vec<u8> = if little {
                b"II\x2a\0\x08\0\0\0".to_vec()
            } else {
                b"MM\0\x2a\0\0\0\x08".to_vec()
            };
            let w16 = |v: u16| {
                if little {
                    v.to_le_bytes()
                } else {
                    v.to_be_bytes()
                }
            };
            let w32 = |v: u32| {
                if little {
                    v.to_le_bytes()
                } else {
                    v.to_be_bytes()
                }
            };
            tiff.extend_from_slice(&w16(2));
            // GPS IFD pointer (0x8825) and orientation 6.
            tiff.extend_from_slice(&w16(0x8825));
            tiff.extend_from_slice(&w16(4));
            tiff.extend_from_slice(&w32(1));
            tiff.extend_from_slice(&w32(99));
            tiff.extend_from_slice(&w16(0x0112));
            tiff.extend_from_slice(&w16(3));
            tiff.extend_from_slice(&w32(1));
            tiff.extend_from_slice(&w16(6));
            tiff.extend_from_slice(&[0, 0]);
            tiff.extend_from_slice(&w32(0));
            tiff.extend_from_slice(b"GPS 37.7749 N 122.4194 W");
            let mut seg = vec![0xFF, 0xE1];
            seg.extend_from_slice(&((2 + 6 + tiff.len()) as u16).to_be_bytes());
            seg.extend_from_slice(b"Exif\0\0");
            seg.extend_from_slice(&tiff);
            b.extend_from_slice(&seg);
            let xmp = b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta>place</x:xmpmeta>";
            b.extend_from_slice(&[0xFF, 0xE1]);
            b.extend_from_slice(&((2 + xmp.len()) as u16).to_be_bytes());
            b.extend_from_slice(xmp);
        }
        b.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 1, 2, 3, 4, 0xFF, 0xD9]);
        b
    }

    #[test]
    fn jpeg_loses_exif_and_xmp_but_keeps_orientation_and_pixels() {
        for little in [false, true] {
            let src = jpeg(true, little);
            assert_eq!(orientation(&src), Some(6), "little endian: {little}");
            let out = strip_metadata(&src).unwrap();
            assert!(!out.windows(3).any(|w| w == b"GPS"), "no position left");
            assert!(!out.windows(8).any(|w| w == b"xmpmeta>"), "no XMP left");
            assert_eq!(orientation(&out), Some(6), "still the right way up");
            // The scan, the image data, is byte for byte the same.
            assert!(out.ends_with(&[0xFF, 0xDA, 0x00, 0x02, 1, 2, 3, 4, 0xFF, 0xD9]));
            assert_eq!(
                strip_metadata(&out),
                Some(out.clone()),
                "stripping again keeps it"
            );
        }
        assert_eq!(
            strip_metadata(&jpeg(false, false)),
            None,
            "nothing to remove"
        );
        assert_eq!(orientation(&jpeg(false, false)), None);
    }

    #[test]
    fn png_loses_text_and_exif_chunks() {
        let chunk = |ty: &[u8], data: &[u8]| {
            let mut c = (data.len() as u32).to_be_bytes().to_vec();
            c.extend_from_slice(ty);
            c.extend_from_slice(data);
            c.extend_from_slice(&[0, 0, 0, 0]);
            c
        };
        let mut src = b"\x89PNG\r\n\x1a\n".to_vec();
        src.extend(chunk(b"IHDR", &[0; 13]));
        src.extend(chunk(b"eXIf", b"GPS here"));
        src.extend(chunk(b"iTXt", b"XML:com.adobe.xmp place"));
        src.extend(chunk(b"IDAT", &[9; 20]));
        src.extend(chunk(b"IEND", &[]));
        let out = strip_metadata(&src).unwrap();
        assert!(!out.windows(3).any(|w| w == b"GPS"));
        assert!(!out.windows(5).any(|w| w == b"place"));
        assert!(out.windows(4).any(|w| w == b"IDAT"));
        assert_eq!(out.len(), src.len() - (12 + 8) - (12 + 23));
        assert_eq!(strip_metadata(&out), None);
    }

    #[test]
    fn webp_loses_exif_and_xmp_chunks_and_flags() {
        let chunk = |ty: &[u8], data: &[u8]| {
            let mut c = ty.to_vec();
            c.extend_from_slice(&(data.len() as u32).to_le_bytes());
            c.extend_from_slice(data);
            if data.len() % 2 == 1 {
                c.push(0);
            }
            c
        };
        let mut body = b"WEBP".to_vec();
        body.extend(chunk(b"VP8X", &[0x0C, 0, 0, 0, 1, 0, 0, 1, 0, 0]));
        body.extend(chunk(b"VP8 ", &[1, 2, 3]));
        body.extend(chunk(b"EXIF", b"GPS!"));
        body.extend(chunk(b"XMP ", b"place"));
        let mut src = b"RIFF".to_vec();
        src.extend_from_slice(&(body.len() as u32).to_le_bytes());
        src.extend(body);
        let out = strip_metadata(&src).unwrap();
        assert!(!out.windows(4).any(|w| w == b"EXIF" || w == b"XMP "));
        assert_eq!(out[20] & 0x0C, 0, "the header no longer announces them");
        let size = u32::from_le_bytes([out[4], out[5], out[6], out[7]]) as usize;
        assert_eq!(size, out.len() - 8);
    }

    #[test]
    fn other_and_broken_bytes_are_left_alone() {
        assert_eq!(strip_metadata(b"%PDF-1.7"), None);
        assert_eq!(strip_metadata(b""), None);
        assert_eq!(strip_metadata(&[0xFF, 0xD8, 0xFF, 0xE1, 0xFF, 0xFF]), None);
        let mut cut = jpeg(true, false);
        cut.truncate(20);
        assert_eq!(strip_metadata(&cut), None);
        assert_eq!(
            strip_metadata(b"\x89PNG\r\n\x1a\n\xff\xff\xff\xffIDAT"),
            None
        );
        assert_eq!(
            strip_metadata(b"RIFF\x04\0\0\0WEBPEXIF\xff\xff\xff\xff"),
            None
        );
    }
}
