//! Encryption at rest (docs/04-security-compliance.md §2.6): envelope
//! encryption with AES-256-GCM. The master key (KEK) wraps data keys (DEKs),
//! one per conversation (or per database for shared content) per 30 days.
//! Stored values are `enc:v1:<key_id>:<base64(nonce|ciphertext|tag)>`, bound
//! to their table, column and row by the AAD.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use base64::Engine;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};

use crate::model::{ApiError, ApiResult};

pub const PREFIX: &str = "enc:v1:";
/// Plaintext that itself starts with `enc:` is stored behind this prefix,
/// so it is never mistaken for ciphertext (M11).
pub const RAW: &str = "enc:raw:";

/// How plaintext is stored when it is not encrypted.
pub fn store_plain(plain: &str) -> std::borrow::Cow<'_, str> {
    if plain.starts_with("enc:") {
        format!("{RAW}{plain}").into()
    } else {
        plain.into()
    }
}
/// Data keys rotate every 30 days.
pub const PERIOD_MS: i64 = 30 * 86_400_000;

pub fn period(now_ms: i64) -> i64 {
    now_ms.div_euclid(PERIOD_MS)
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn key(bytes: &[u8]) -> Result<LessSafeKey, ApiError> {
    UnboundKey::new(&AES_256_GCM, bytes)
        .map(LessSafeKey::new)
        .map_err(|_| ApiError::new("config", "a key must be 32 bytes"))
}

fn seal_raw(k: &LessSafeKey, aad: &[u8], plain: &[u8]) -> Vec<u8> {
    let mut nonce = [0u8; NONCE_LEN];
    SystemRandom::new().fill(&mut nonce).expect("random");
    let mut buf = plain.to_vec();
    k.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(aad),
        &mut buf,
    )
    .expect("seal");
    let mut out = nonce.to_vec();
    out.extend_from_slice(&buf);
    out
}

fn open_raw(k: &LessSafeKey, aad: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < NONCE_LEN + AES_256_GCM.tag_len() {
        return None;
    }
    let (n, rest) = data.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(n).ok()?;
    let mut buf = rest.to_vec();
    let plain = k.open_in_place(nonce, Aad::from(aad), &mut buf).ok()?;
    Some(plain.to_vec())
}

/// The master key.
pub struct Kek {
    key: LessSafeKey,
    /// Identifies the key without revealing it (stored in `meta`).
    pub fingerprint: String,
}

impl Kek {
    pub fn from_base64(s: &str) -> Result<Kek, String> {
        let bytes = b64()
            .decode(s.trim())
            .map_err(|_| "the master key must be base64".to_string())?;
        if bytes.len() != 32 {
            return Err("the master key must be 32 bytes (base64 of 32 random bytes)".into());
        }
        let fp = crate::auth::hash(&format!("agentim-kek:{}", s.trim()));
        Ok(Kek {
            key: key(&bytes).map_err(|e| e.message)?,
            fingerprint: fp[..16].to_string(),
        })
    }

    pub fn generate_base64() -> String {
        let mut b = [0u8; 32];
        SystemRandom::new().fill(&mut b).expect("random");
        b64().encode(b)
    }

    pub fn wrap(&self, key_id: &str, dek: &[u8]) -> String {
        b64().encode(seal_raw(&self.key, key_id.as_bytes(), dek))
    }

    pub fn unwrap(&self, key_id: &str, wrapped: &str) -> Option<Vec<u8>> {
        open_raw(&self.key, key_id.as_bytes(), &b64().decode(wrapped).ok()?)
    }
}

/// A data key created in the running batch, not committed yet.
pub struct Pending {
    pub scope: String,
    pub period: i64,
    pub key_id: String,
}

/// Per database: the master key and the data keys in use.
#[derive(Default)]
pub struct Crypto {
    pub kek: Option<Kek>,
    deks: RwLock<HashMap<String, Arc<LessSafeKey>>>,
    /// (scope, period) → key id; scope is a conversation id or "" for the database.
    current: Mutex<HashMap<(String, i64), String>>,
    pending: Mutex<Vec<Pending>>,
}

impl Crypto {
    pub fn new(kek: Option<Kek>) -> Crypto {
        Crypto {
            kek,
            ..Default::default()
        }
    }

    pub fn enabled(&self) -> bool {
        self.kek.is_some()
    }

    pub fn has_key(&self, key_id: &str) -> bool {
        self.deks.read().unwrap().contains_key(key_id)
    }

    /// Unwraps a stored data key into the cache.
    pub fn load(&self, key_id: &str, wrapped: &str) -> ApiResult<()> {
        let kek = self.kek.as_ref().ok_or_else(|| {
            ApiError::new("storage", "content is encrypted but encryption is off")
        })?;
        let dek = kek.unwrap(key_id, wrapped).ok_or_else(|| {
            ApiError::new(
                "storage",
                format!("data key {key_id} does not open with this master key"),
            )
        })?;
        self.deks
            .write()
            .unwrap()
            .insert(key_id.to_string(), Arc::new(key(&dek)?));
        Ok(())
    }

    pub fn current(&self, scope: &str, period: i64) -> Option<String> {
        self.current
            .lock()
            .unwrap()
            .get(&(scope.to_string(), period))
            .cloned()
    }

    pub fn set_current(&self, scope: &str, period: i64, key_id: &str) {
        self.current
            .lock()
            .unwrap()
            .insert((scope.to_string(), period), key_id.to_string());
    }

    /// A new data key for the running batch: `(key_id, wrapped)`.
    pub fn create(&self, scope: &str, period: i64) -> ApiResult<(String, String)> {
        let kek = self
            .kek
            .as_ref()
            .ok_or_else(|| ApiError::new("internal", "encryption is off"))?;
        let mut dek = [0u8; 32];
        SystemRandom::new().fill(&mut dek).expect("random");
        let key_id = format!("k_{}", crate::db::new_id());
        let wrapped = kek.wrap(&key_id, &dek);
        self.deks
            .write()
            .unwrap()
            .insert(key_id.clone(), Arc::new(key(&dek)?));
        self.set_current(scope, period, &key_id);
        self.pending.lock().unwrap().push(Pending {
            scope: scope.to_string(),
            period,
            key_id: key_id.clone(),
        });
        Ok((key_id, wrapped))
    }

    /// Marks where a job starts in the pending list.
    pub fn mark(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// A job rolled back: forget the keys it created.
    pub fn rollback_to(&self, mark: usize) {
        let gone: Vec<Pending> = self.pending.lock().unwrap().drain(mark..).collect();
        self.forget(gone);
    }

    /// The batch committed: its new keys are real.
    pub fn commit(&self) {
        self.pending.lock().unwrap().clear();
    }

    /// The batch failed: none of its new keys exist.
    pub fn abort(&self) {
        let gone: Vec<Pending> = self.pending.lock().unwrap().drain(..).collect();
        self.forget(gone);
    }

    fn forget(&self, gone: Vec<Pending>) {
        let mut cur = self.current.lock().unwrap();
        let mut deks = self.deks.write().unwrap();
        for p in gone {
            deks.remove(&p.key_id);
            if cur.get(&(p.scope.clone(), p.period)) == Some(&p.key_id) {
                cur.remove(&(p.scope, p.period));
            }
        }
    }

    /// Conversation shredded: its keys must not be used again.
    pub fn drop_scope(&self, scope: &str) {
        self.current.lock().unwrap().retain(|(s, _), _| s != scope);
    }

    /// Evicts unwrapped data keys from the cache (after they were deleted).
    pub fn evict(&self, key_ids: &[String]) {
        let mut d = self.deks.write().unwrap();
        for k in key_ids {
            d.remove(k);
        }
    }

    pub fn seal(&self, key_id: &str, aad: &str, plain: &str) -> ApiResult<String> {
        let k = self
            .deks
            .read()
            .unwrap()
            .get(key_id)
            .cloned()
            .ok_or_else(|| ApiError::new("internal", "data key not loaded"))?;
        Ok(format!(
            "{PREFIX}{key_id}:{}",
            b64().encode(seal_raw(&k, aad.as_bytes(), plain.as_bytes()))
        ))
    }

    /// Opens a stored value. Values without the prefix, or naming a key this
    /// database never had, were written as plaintext.
    pub fn open(&self, aad: &str, stored: &str) -> Result<String, String> {
        if let Some(plain) = stored.strip_prefix(RAW) {
            return Ok(plain.to_string());
        }
        let Some((key_id, data)) = parse(stored) else {
            return Ok(stored.to_string());
        };
        let Some(k) = self.deks.read().unwrap().get(key_id).cloned() else {
            return Err(format!("data key {key_id} is not loaded"));
        };
        let data = b64()
            .decode(data)
            .map_err(|_| "damaged ciphertext".to_string())?;
        let plain = open_raw(&k, aad.as_bytes(), &data)
            .ok_or_else(|| format!("content at {aad} does not decrypt (changed or moved)"))?;
        String::from_utf8(plain).map_err(|_| "decrypted content is not text".to_string())
    }
}

/// `(key_id, base64)` of an encrypted value.
pub fn parse(stored: &str) -> Option<(&str, &str)> {
    let rest = stored.strip_prefix(PREFIX)?;
    let (k, d) = rest.split_once(':')?;
    k.starts_with("k_").then_some((k, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx() -> Crypto {
        Crypto::new(Some(Kek::from_base64(&Kek::generate_base64()).unwrap()))
    }

    #[test]
    fn seal_and_open() {
        let c = cx();
        let (k, wrapped) = c.create("c_1", 7).unwrap();
        let s = c.seal(&k, "messages.content:m_1", "secret ü").unwrap();
        assert!(s.starts_with("enc:v1:k_") && !s.contains("secret"));
        assert_eq!(c.open("messages.content:m_1", &s).unwrap(), "secret ü");
        // Moved to another row or column: refused, not returned as text.
        assert!(c.open("messages.content:m_2", &s).is_err());
        assert!(c.open("messages.payload:m_1", &s).is_err());
        // Changed: refused.
        let mut t = s.clone();
        t.pop();
        t.push(if s.ends_with('A') { 'B' } else { 'A' });
        assert!(c.open("messages.content:m_1", &t).is_err());
        // Plaintext from before encryption reads as is.
        assert_eq!(c.open("x", "hello").unwrap(), "hello");
        assert_eq!(c.open("x", "enc:v1:notakey").unwrap(), "enc:v1:notakey");
        // Plaintext that looks like ciphertext is escaped when stored.
        let looks = "enc:v1:k_x:AAAA";
        let stored = store_plain(looks);
        assert_eq!(stored, "enc:raw:enc:v1:k_x:AAAA");
        assert_eq!(c.open("x", &stored).unwrap(), looks);
        assert_eq!(store_plain("hello"), "hello");
        assert_eq!(c.open("x", &store_plain("enc:raw:x")).unwrap(), "enc:raw:x");
        // The wrapped key opens only with its master key and its id.
        let other = cx();
        assert!(other.load(&k, &wrapped).is_err());
        let again = Crypto::new(Some(Kek::from_base64(&b64().encode([0u8; 32])).unwrap()));
        assert!(again.load(&k, &wrapped).is_err());
    }

    #[test]
    fn pending_keys_follow_the_batch() {
        let c = cx();
        let m = c.mark();
        let (k1, _) = c.create("c_1", 1).unwrap();
        c.rollback_to(m);
        assert!(
            !c.has_key(&k1) && c.current("c_1", 1).is_none(),
            "job rolled back"
        );
        let (k2, _) = c.create("c_1", 1).unwrap();
        c.abort();
        assert!(!c.has_key(&k2), "batch failed");
        let (k3, _) = c.create("c_1", 1).unwrap();
        c.commit();
        assert_eq!(c.current("c_1", 1).as_deref(), Some(k3.as_str()));
    }

    #[test]
    fn master_keys() {
        assert!(Kek::from_base64("short").is_err());
        assert!(Kek::from_base64(&b64().encode([1u8; 16])).is_err());
        let s = Kek::generate_base64();
        assert_eq!(
            Kek::from_base64(&s).unwrap().fingerprint,
            Kek::from_base64(&format!(" {s}\n")).unwrap().fingerprint
        );
        assert_ne!(
            Kek::from_base64(&s).unwrap().fingerprint,
            Kek::from_base64(&Kek::generate_base64())
                .unwrap()
                .fingerprint
        );
        assert_eq!(period(0), 0);
        assert_eq!(period(PERIOD_MS - 1), 0);
        assert_eq!(period(PERIOD_MS), 1);
    }
}
