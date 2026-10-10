//! Credentials: client tokens and API keys are random, shown once, and only
//! their SHA-256 is stored (docs/04-security-compliance.md §1, 07 §5).

use base64::Engine;
use sha2::{Digest, Sha256};

/// `<prefix>_` + 256 random bits, base64url without padding.
pub fn new_secret(prefix: &str) -> String {
    let mut b = [0u8; 32];
    b[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    b[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    format!(
        "{prefix}_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
    )
}

pub fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// Placeholder kept in the legacy `users.token` column (unique, never valid).
pub fn legacy_placeholder(user_id: &str) -> String {
    format!("x_{user_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_random_and_hashed() {
        let a = new_secret("atk");
        let b = new_secret("atk");
        assert_ne!(a, b);
        assert!(a.starts_with("atk_") && a.len() == 4 + 43, "{a}");
        assert_eq!(hash(&a).len(), 64);
        assert_eq!(hash(&a), hash(&a));
        assert_ne!(hash(&a), hash(&b));
    }
}
