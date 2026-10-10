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

// ------------------------------------------------------------ passwords

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};

/// Argon2id with the OWASP minimum (19 MiB, 2 passes, 1 lane).
fn argon() -> argon2::Argon2<'static> {
    argon2::Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon2::Params::new(19 * 1024, 2, 1, None).expect("valid argon2 params"),
    )
}

pub fn check_password_policy(name: &str, password: &str) -> Result<(), &'static str> {
    let n = password.chars().count();
    if !(10..=128).contains(&n) {
        return Err("passwords are 10..128 characters");
    }
    if password.eq_ignore_ascii_case(name) {
        return Err("the password must differ from the user name");
    }
    Ok(())
}

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::encode_b64(uuid::Uuid::new_v4().as_bytes()).expect("16-byte salt");
    argon()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hash")
        .to_string()
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| argon().verify_password(password.as_bytes(), &h).is_ok())
}

/// A valid hash nobody knows the password of, verified for unknown users so
/// the response time does not tell which names exist.
pub fn dummy_hash() -> &'static str {
    static H: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    H.get_or_init(|| hash_password(&new_secret("dummy")))
}

// ----------------------------------------------------------------- TOTP

use hmac::{KeyInit, Mac};

pub const TOTP_STEP: i64 = 30;

/// 160 random bits, base32 (what authenticator apps expect).
pub fn new_totp_secret() -> String {
    let mut b = [0u8; 20];
    b[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    b[16..].copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..4]);
    data_encoding::BASE32_NOPAD.encode(&b)
}

/// RFC 6238 (HMAC-SHA1, 6 digits) for one time step.
pub fn totp_at(secret_b32: &str, step: i64) -> Option<String> {
    let key = data_encoding::BASE32_NOPAD
        .decode(secret_b32.as_bytes())
        .ok()?;
    let mut mac = hmac::Hmac::<sha1::Sha1>::new_from_slice(&key).ok()?;
    mac.update(&(step as u64).to_be_bytes());
    let h = mac.finalize().into_bytes();
    let o = (h[h.len() - 1] & 0x0f) as usize;
    let bin = u32::from_be_bytes([h[o] & 0x7f, h[o + 1], h[o + 2], h[o + 3]]);
    Some(format!("{:06}", bin % 1_000_000))
}

/// The step a code matches within ±1 step of `now_s`, if any.
pub fn totp_check(secret_b32: &str, code: &str, now_s: i64) -> Option<i64> {
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let cur = now_s.div_euclid(TOTP_STEP);
    [cur - 1, cur, cur + 1]
        .into_iter()
        .find(|s| totp_at(secret_b32, *s).as_deref() == Some(code))
}

pub fn otpauth_uri(issuer: &str, account: &str, secret: &str) -> String {
    let enc = |s: &str| {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    format!(
        "otpauth://totp/{}:{}?secret={secret}&issuer={}&algorithm=SHA1&digits=6&period=30",
        enc(issuer),
        enc(account),
        enc(issuer)
    )
}

#[cfg(test)]
mod password_tests {
    use super::*;

    #[test]
    fn passwords() {
        let h = hash_password("correct horse battery");
        assert!(h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{h}");
        assert!(verify_password("correct horse battery", &h));
        assert!(!verify_password("correct horse batterY", &h));
        assert!(!verify_password("x", "not a hash"));
        assert_ne!(h, hash_password("correct horse battery"), "salted");
        assert!(!verify_password("anything", dummy_hash()));
        assert!(check_password_policy("alice", "short").is_err());
        assert!(check_password_policy("alice", &"x".repeat(129)).is_err());
        assert!(check_password_policy("alicealice", "ALICEALICE").is_err());
        assert!(check_password_policy("alice", "ten chars!").is_ok());
        assert!(
            check_password_policy("alice", &"密".repeat(10)).is_ok(),
            "characters, not bytes"
        );
    }

    #[test]
    fn totp_matches_rfc_6238() {
        // RFC 6238 appendix B, SHA1 secret "12345678901234567890".
        let secret = data_encoding::BASE32_NOPAD.encode(b"12345678901234567890");
        for (t, code) in [
            (59, "287082"),
            (1111111109, "081804"),
            (1111111111, "050471"),
            (1234567890, "005924"),
            (2000000000, "279037"),
        ] {
            assert_eq!(totp_at(&secret, t / TOTP_STEP).unwrap(), code, "t={t}");
        }
        assert_eq!(
            totp_check(&secret, "081804", 1111111109),
            Some(1111111109 / 30)
        );
        assert_eq!(
            totp_check(&secret, "081804", 1111111109 + 30),
            Some(1111111109 / 30),
            "one step late"
        );
        assert_eq!(
            totp_check(&secret, "081804", 1111111109 + 61),
            None,
            "two steps late"
        );
        for bad in ["", "12345", "1234567", "abcdef"] {
            assert_eq!(totp_check(&secret, bad, 59), None);
        }
        let s = new_totp_secret();
        assert_eq!(s.len(), 32);
        assert!(
            otpauth_uri("AgentIM", "a b", &s).starts_with("otpauth://totp/AgentIM:a%20b?secret=")
        );
    }
}
