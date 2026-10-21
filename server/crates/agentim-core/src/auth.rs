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

// ------------------------------------------------------------ client tokens (JWT)

/// What a verified client token says (docs/13 §4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    pub kid: String,
    pub sub: String,
    pub iat: i64,
    pub exp: i64,
}

/// Why a client token was refused; the caller only sees 401.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JwtError {
    Malformed,
    Algorithm,
    UnknownKey,
    Signature,
    Expired,
    FromTheFuture,
    TooLong,
    Subject,
}

impl JwtError {
    pub fn as_str(self) -> &'static str {
        match self {
            JwtError::Malformed => "malformed",
            JwtError::Algorithm => "algorithm",
            JwtError::UnknownKey => "unknown_key",
            JwtError::Signature => "signature",
            JwtError::Expired => "expired",
            JwtError::FromTheFuture => "issued_in_the_future",
            JwtError::TooLong => "lifetime_too_long",
            JwtError::Subject => "subject",
        }
    }
}

/// Seconds a token's `iat` may be ahead of the server clock.
pub const JWT_LEEWAY_S: i64 = 60;

/// Whether a credential looks like a JWT (agent keys start with `atk_`).
pub fn looks_like_jwt(token: &str) -> bool {
    token.split('.').count() == 3
}

fn b64u() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

fn hs256(secret: &[u8], input: &str) -> Vec<u8> {
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(secret).expect("any key length");
    mac.update(input.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Signs a client token the way a developer's backend does (the CLI, the
/// demo and tests use it).
pub fn jwt_sign(kid: &str, secret: &str, sub: &str, iat: i64, exp: i64) -> String {
    let head =
        b64u().encode(serde_json::json!({"alg": "HS256", "typ": "JWT", "kid": kid}).to_string());
    let body = b64u().encode(serde_json::json!({"sub": sub, "iat": iat, "exp": exp}).to_string());
    let input = format!("{head}.{body}");
    let sig = b64u().encode(hs256(secret.as_bytes(), &input));
    format!("{input}.{sig}")
}

/// Verifies a client token: HS256 only, a known key, `iat` not ahead of
/// `now_s` by more than the leeway, unexpired, at most `max_s` long.
/// `secret_of(kid)` gives a key's secret, None for unknown or revoked.
pub fn jwt_verify(
    token: &str,
    secret_of: impl Fn(&str) -> Option<String>,
    now_s: i64,
    max_s: i64,
) -> Result<Claims, JwtError> {
    use JwtError::*;
    if token.len() > 4096 {
        return Err(Malformed);
    }
    let mut parts = token.split('.');
    let (Some(h), Some(b), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Malformed);
    };
    let json = |p: &str| -> Result<serde_json::Value, JwtError> {
        let raw = b64u().decode(p).map_err(|_| Malformed)?;
        let v: serde_json::Value = serde_json::from_slice(&raw).map_err(|_| Malformed)?;
        if v.is_object() { Ok(v) } else { Err(Malformed) }
    };
    let head = json(h)?;
    // Only HS256: `none` and other algorithms are refused, so a token
    // cannot pick how it is checked.
    if head["alg"] != "HS256" {
        return Err(Algorithm);
    }
    if head.get("crit").is_some() {
        return Err(Malformed);
    }
    let kid = head["kid"].as_str().ok_or(Malformed)?.to_string();
    let secret = secret_of(&kid).ok_or(UnknownKey)?;
    let sig = b64u().decode(s).map_err(|_| Malformed)?;
    let input = &token[..h.len() + 1 + b.len()];
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(input.as_bytes());
    mac.verify_slice(&sig).map_err(|_| Signature)?;
    let body = json(b)?;
    let int = |k: &str| body[k].as_i64().ok_or(Malformed);
    let (iat, exp) = (int("iat")?, int("exp")?);
    let sub = body["sub"].as_str().ok_or(Subject)?.to_string();
    if iat > now_s + JWT_LEEWAY_S {
        return Err(FromTheFuture);
    }
    if exp <= now_s {
        return Err(Expired);
    }
    if exp - iat > max_s || exp <= iat {
        return Err(TooLong);
    }
    Ok(Claims { kid, sub, iat, exp })
}

/// A new signing key: id and secret (the secret is shown once).
pub fn new_signing_key() -> (String, String) {
    let kid = format!("sk_{}", &hash(&new_secret("k"))[..16]);
    (kid, new_secret("sks"))
}

#[cfg(test)]
mod jwt_tests {
    use super::*;

    const DAY: i64 = 86400;

    fn keys(kid: &str) -> Option<String> {
        (kid == "k1").then(|| "s3cret".to_string())
    }

    #[test]
    fn a_signed_token_verifies() {
        let t = jwt_sign("k1", "s3cret", "u1", 1000, 1000 + DAY);
        assert!(looks_like_jwt(&t));
        assert!(!looks_like_jwt("atk_abc"));
        let c = jwt_verify(&t, keys, 1000, 30 * DAY).unwrap();
        assert_eq!(
            c,
            Claims {
                kid: "k1".into(),
                sub: "u1".into(),
                iat: 1000,
                exp: 1000 + DAY
            }
        );
    }

    /// A token from a common JWT library (PyJWT 2.9: jwt.encode({"sub":
    /// "alice", "iat": 1700000000, "exp": 1700003600}, "s3cret",
    /// headers={"kid": "k1"})) is accepted: the format is the standard one.
    #[test]
    fn a_library_token_verifies() {
        let t = "eyJhbGciOiJIUzI1NiIsImtpZCI6ImsxIiwidHlwIjoiSldUIn0.eyJzdWIiOiJhbGljZSIsImlhdCI6MTcwMDAwMDAwMCwiZXhwIjoxNzAwMDAzNjAwfQ.ridO7VIajyWQwFauc8ZH4GIGe4j3vGg3JRd1ugJjg9M";
        let c = jwt_verify(t, keys, 1_700_000_100, 30 * DAY).unwrap();
        assert_eq!(c.sub, "alice");
    }

    #[test]
    fn refusals() {
        use JwtError::*;
        let now = 10 * DAY;
        let ok = |sub: &str, iat: i64, exp: i64| jwt_sign("k1", "s3cret", sub, iat, exp);
        let v = |t: &str| jwt_verify(t, keys, now, 30 * DAY).map(|_| ());
        assert_eq!(v(&ok("u", now - 10, now - 1)), Err(Expired));
        assert_eq!(v(&ok("u", now - 10, now)), Err(Expired), "exp is exclusive");
        assert_eq!(v(&ok("u", now + 61, now + DAY)), Err(FromTheFuture));
        assert_eq!(v(&ok("u", now + 60, now + DAY)), Ok(()), "60 s leeway");
        assert_eq!(v(&ok("u", now, now + 30 * DAY + 1)), Err(TooLong));
        assert_eq!(v(&ok("u", now, now + 30 * DAY)), Ok(()), "exactly the most");
        assert_eq!(
            v(&jwt_sign("k2", "s3cret", "u", now, now + 9)),
            Err(UnknownKey)
        );
        assert_eq!(
            v(&jwt_sign("k1", "other", "u", now, now + 9)),
            Err(Signature)
        );
        assert_eq!(v("a.b"), Err(Malformed));
        assert_eq!(v("a.b.c.d"), Err(Malformed));
        assert_eq!(v("!!.b.c"), Err(Malformed));
        assert_eq!(v(&"x".repeat(5000)), Err(Malformed));
        // A forged header: `none`, another algorithm, no kid, critical
        // extensions; the body is valid and signed with the right key.
        let body = b64u().encode(r#"{"sub":"u","iat":864000,"exp":864009}"#);
        let forge = |head: &str| {
            let h = b64u().encode(head);
            let input = format!("{h}.{body}");
            format!("{input}.{}", b64u().encode(hs256(b"s3cret", &input)))
        };
        assert_eq!(v(&forge(r#"{"alg":"none","kid":"k1"}"#)), Err(Algorithm));
        assert_eq!(v(&forge(r#"{"alg":"HS512","kid":"k1"}"#)), Err(Algorithm));
        assert_eq!(v(&forge(r#"{"alg":"RS256","kid":"k1"}"#)), Err(Algorithm));
        assert_eq!(v(&forge(r#"{"alg":"HS256"}"#)), Err(Malformed));
        assert_eq!(
            v(&forge(r#"{"alg":"HS256","kid":"k1","crit":["x"]}"#)),
            Err(Malformed)
        );
        assert_eq!(v(&forge(r#"{"alg":"HS256","kid":"k1"}"#)), Ok(()));
        let unsigned = format!("{}.{body}.", b64u().encode(r#"{"alg":"none","kid":"k1"}"#));
        assert_eq!(v(&unsigned), Err(Algorithm));
        // Body fields.
        let signed_body = |b: &str| {
            let h = b64u().encode(r#"{"alg":"HS256","kid":"k1"}"#);
            let input = format!("{h}.{}", b64u().encode(b));
            format!("{input}.{}", b64u().encode(hs256(b"s3cret", &input)))
        };
        assert_eq!(
            v(&signed_body(r#"{"iat":864000,"exp":864009}"#)),
            Err(Subject)
        );
        assert_eq!(
            v(&signed_body(r#"{"sub":5,"iat":864000,"exp":864009}"#)),
            Err(Subject)
        );
        assert_eq!(
            v(&signed_body(r#"{"sub":"u","exp":864009}"#)),
            Err(Malformed)
        );
        assert_eq!(
            v(&signed_body(r#"{"sub":"u","iat":864000}"#)),
            Err(Malformed)
        );
        assert_eq!(
            v(&signed_body(r#"{"sub":"u","iat":"864000","exp":864009}"#)),
            Err(Malformed)
        );
        assert_eq!(v(&signed_body(r#"[1]"#)), Err(Malformed));
        assert_eq!(
            v(&signed_body(r#"{"sub":"u","iat":864009,"exp":864009}"#)),
            Err(TooLong)
        );
        // A tampered body breaks the signature.
        let t = ok("u", now, now + 9);
        let mut p: Vec<&str> = t.split('.').collect();
        let other = b64u().encode(r#"{"sub":"admin","iat":864000,"exp":864009}"#);
        p[1] = &other;
        assert_eq!(v(&p.join(".")), Err(Signature));
    }

    #[test]
    fn signing_keys_are_random() {
        let (a, sa) = new_signing_key();
        let (b, sb) = new_signing_key();
        assert!(a.starts_with("sk_") && a.len() == 19, "{a}");
        assert_ne!(a, b);
        assert_ne!(sa, sb);
    }
}
