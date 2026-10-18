//! Frames between nodes (docs/11 §6.3). A fixed 16-byte header, then a
//! JSON body: new fields are optional and unknown ones are ignored, so
//! adjacent versions understand each other.
//!
//! ```text
//! 0..4   magic "AIMB"
//! 4..8   body length, big-endian u32
//! 8      encoding version (1)
//! 9      message type
//! 10..12 flags (0)
//! 12..16 reserved (0)
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};

pub const MAGIC: &[u8; 4] = b"AIMB";
pub const HEADER: usize = 16;
pub const VERSION: u8 = 1;
/// Largest body; a bigger frame closes the connection.
pub const MAX_BODY: usize = 8 << 20;
/// Protocol versions this program speaks.
pub const PROTO_MIN: u32 = 1;
pub const PROTO_MAX: u32 = 1;
/// What this version can do beyond protocol 1; a node sends a peer only
/// what the peer declared (docs/11 §6.3). Commands a peer does not know
/// come back `unsupported_op`.
pub const CAPABILITIES: &[&str] = &["forward", "publish", "check"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Hello = 1,
    ForwardReq = 3,
    ForwardResp = 4,
    Publish = 5,
    Ping = 6,
    Pong = 7,
    /// "Look at the lease table now" (a shard was handed to you).
    Check = 8,
}

impl Kind {
    pub fn from_u8(b: u8) -> Option<Kind> {
        Some(match b {
            1 => Kind::Hello,
            3 => Kind::ForwardReq,
            4 => Kind::ForwardResp,
            5 => Kind::Publish,
            6 => Kind::Ping,
            7 => Kind::Pong,
            8 => Kind::Check,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hello {
    pub cluster_id: String,
    pub node_id: String,
    pub boot_id: String,
    pub proto_min: u32,
    pub proto_max: u32,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardReq {
    pub id: u64,
    pub db: usize,
    pub shard: u32,
    /// The writer command (`agentim_core::logic::Op`).
    pub op: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardResp {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<WireError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireError {
    pub code: String,
    #[serde(default)]
    pub message: String,
}

/// One frame: header and body.
pub fn encode(kind: Kind, body: &[u8]) -> Result<Vec<u8>, String> {
    if body.len() > MAX_BODY {
        return Err(format!("frame of {} bytes is over the limit", body.len()));
    }
    let mut f = Vec::with_capacity(HEADER + body.len());
    f.extend_from_slice(MAGIC);
    f.extend_from_slice(&(body.len() as u32).to_be_bytes());
    f.push(VERSION);
    f.push(kind as u8);
    f.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    f.extend_from_slice(body);
    Ok(f)
}

pub fn encode_json<T: Serialize>(kind: Kind, v: &T) -> Result<Vec<u8>, String> {
    encode(kind, &serde_json::to_vec(v).map_err(|e| e.to_string())?)
}

/// A decoded header: message type (None when unknown) and body length.
pub fn header(h: &[u8; HEADER]) -> Result<(Option<Kind>, usize), String> {
    if &h[0..4] != MAGIC {
        return Err("bad magic".into());
    }
    let len = u32::from_be_bytes([h[4], h[5], h[6], h[7]]) as usize;
    if len > MAX_BODY {
        return Err(format!("frame of {len} bytes is over the limit"));
    }
    Ok((Kind::from_u8(h[9]), len))
}

/// Reads one frame; Ok(None) at a clean end of the stream. Frames of an
/// unknown type come back as `(None, body)` for the caller to skip.
pub async fn read<R: AsyncRead + Unpin>(
    r: &mut R,
) -> Result<Option<(Option<Kind>, Vec<u8>)>, String> {
    let mut h = [0u8; HEADER];
    // A clean end only between frames; inside a header it is truncation.
    if r.read(&mut h[..1]).await.map_err(|e| e.to_string())? == 0 {
        return Ok(None);
    }
    r.read_exact(&mut h[1..])
        .await
        .map_err(|e| format!("truncated frame: {e}"))?;
    let (kind, len) = header(&h)?;
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)
        .await
        .map_err(|e| format!("truncated frame: {e}"))?;
    Ok(Some((kind, body)))
}

/// Versions both sides speak: the highest common one.
pub fn negotiate(a: (u32, u32), b: (u32, u32)) -> Option<u32> {
    let lo = a.0.max(b.0);
    let hi = a.1.min(b.1);
    (lo <= hi).then_some(hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_layout_is_fixed() {
        let f = encode(Kind::Publish, b"{}").unwrap();
        assert_eq!(&f[..], b"AIMB\0\0\0\x02\x01\x05\0\0\0\0\0\0{}");
        assert_eq!(f.len(), HEADER + 2);
    }

    #[tokio::test]
    async fn frames_round_trip_and_bad_ones_are_refused() {
        let hello = Hello {
            cluster_id: "c".into(),
            node_id: "a".into(),
            boot_id: "b".into(),
            proto_min: 1,
            proto_max: 1,
            version: "x".into(),
            capabilities: vec!["forward".into()],
        };
        let f = encode_json(Kind::Hello, &hello).unwrap();
        let (k, body) = read(&mut &f[..]).await.unwrap().unwrap();
        assert_eq!(k, Some(Kind::Hello));
        assert_eq!(serde_json::from_slice::<Hello>(&body).unwrap(), hello);
        // Unknown fields are ignored, missing optional ones default.
        let h: Hello = serde_json::from_str(
            r#"{"cluster_id":"c","node_id":"a","boot_id":"b","proto_min":1,"proto_max":2,"new":1}"#,
        )
        .unwrap();
        assert_eq!(h.version, "");
        assert!(h.capabilities.is_empty(), "an older node declares none");
        // Every kind round-trips; unknown kinds are reported, not errors.
        for k in [1u8, 3, 4, 5, 6, 7, 8] {
            assert_eq!(Kind::from_u8(k).unwrap() as u8, k);
        }
        let mut odd = encode(Kind::Ping, b"").unwrap();
        odd[9] = 200;
        assert_eq!(read(&mut &odd[..]).await.unwrap().unwrap().0, None);
        // Clean end, bad magic, truncation, over the limit.
        assert!(read(&mut &b""[..]).await.unwrap().is_none());
        let mut bad = f.clone();
        bad[0] = b'X';
        assert!(read(&mut &bad[..]).await.is_err());
        assert!(read(&mut &f[..f.len() - 1]).await.is_err());
        assert!(read(&mut &f[..10]).await.is_err());
        let mut big = encode(Kind::Ping, b"").unwrap();
        big[4..8].copy_from_slice(&((MAX_BODY + 1) as u32).to_be_bytes());
        assert!(read(&mut &big[..]).await.is_err());
        assert!(encode(Kind::Ping, &vec![0u8; MAX_BODY + 1]).is_err());
        let exact = encode(Kind::Ping, &vec![0u8; MAX_BODY]).unwrap();
        assert_eq!(
            read(&mut &exact[..]).await.unwrap().unwrap().1.len(),
            MAX_BODY
        );
    }

    #[test]
    fn versions_meet_or_not() {
        assert_eq!(negotiate((1, 3), (2, 5)), Some(3));
        assert_eq!(negotiate((1, 1), (1, 1)), Some(1));
        assert_eq!(negotiate((1, 2), (3, 4)), None);
        assert_eq!(negotiate((3, 4), (1, 2)), None);
    }
}
