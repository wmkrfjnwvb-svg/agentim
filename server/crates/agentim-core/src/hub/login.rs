//! Logging in (docs/13 §4.2): a person connects with a client token their
//! developer's backend signed itself (JWT, HS256, a signing key managed by
//! the deployer); an agent connects with its key. A person's device is
//! registered by its first connection, and a user id seen for the first
//! time is created.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Hub, err, now_ms};
use crate::logic::Op;
use crate::model::*;

/// Who a connection is, and how to check later that it still may be.
#[derive(Debug, Clone)]
pub struct Login {
    pub user: User,
    pub device_id: String,
    /// The device's row in `tokens`.
    pub hash: String,
    /// A signed client token: its key and when it was issued (seconds).
    pub jwt: Option<(String, i64)>,
}

/// Longest client-chosen device id.
const DEVICE_MAX: usize = 64;

fn valid_device(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= DEVICE_MAX
        && d.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_label(v: Option<&str>) -> bool {
    v.is_none_or(|v| !v.is_empty() && v.len() <= 64 && !v.contains('\0'))
}

/// The server's id of a person's device: the client's id is only unique
/// for that person.
pub fn device_id_of(user_id: &str, client_device: &str) -> String {
    let h = Sha256::digest(format!("{user_id}\0{client_device}").as_bytes());
    format!("d_{}", &hex::encode(h)[..32])
}

/// The `tokens` row of a person's device (never a valid credential).
pub fn device_hash(device_id: &str) -> String {
    crate::auth::hash(&format!("device\0{device_id}"))
}

impl Hub {
    // ------------------------------------------------------------ keys

    /// Reads the signing keys and cutoffs into memory (start-up and after
    /// another process or node changed them).
    pub(crate) async fn load_signing(&self) -> ApiResult<()> {
        let (keys, cutoffs) = self
            .read(self.control(), |db| {
                Box::pin(async move { Ok((db.signing_keys().await?, db.cutoffs().await?)) })
            })
            .await?;
        let crypto = self.control().crypto.clone();
        let mut live = std::collections::HashMap::new();
        for k in keys.into_iter().filter(|k| k.revoked_at.is_none()) {
            match open_secret(&crypto, &k.kid, &k.secret) {
                Some(s) => {
                    live.insert(k.kid, (s, k.created_at));
                }
                None => tracing::error!(
                    kid = k.kid,
                    "a signing key cannot be read (wrong master key?); tokens signed with it are refused"
                ),
            }
        }
        let sh = &self.inner.sh;
        *sh.signing.write().unwrap() = live;
        *sh.cutoffs.write().unwrap() = cutoffs.into_iter().map(|(u, d, at)| ((u, d), at)).collect();
        Ok(())
    }

    /// A new signing key; the secret is returned once.
    pub async fn create_signing_key(&self, name: &str) -> ApiResult<Value> {
        if name.is_empty() || name.len() > 64 || name.contains('\0') {
            return Err(err("bad_request", "a key name is 1..64 bytes"));
        }
        let (kid, secret) = crate::auth::new_signing_key();
        let crypto = self.control().crypto.clone();
        let stored = match &crypto.kek {
            Some(kek) => format!("k:{}", kek.wrap(&kid, secret.as_bytes())),
            None => format!("p:{secret}"),
        };
        let row = crate::db::SigningKeyRow {
            kid: kid.clone(),
            name: name.to_string(),
            secret: stored,
            created_at: now_ms(),
            revoked_at: None,
        };
        let at = row.created_at;
        self.control().call(Op::CreateSigningKey(row)).await?;
        self.inner
            .sh
            .signing
            .write()
            .unwrap()
            .insert(kid.clone(), (secret.clone(), at));
        Ok(json!({"kid": kid, "name": name, "secret": secret}))
    }

    /// Every signing key (never the secrets).
    pub async fn signing_keys(&self) -> ApiResult<Vec<Value>> {
        let keys = self
            .read(self.control(), |db| {
                Box::pin(async move { db.signing_keys().await })
            })
            .await?;
        Ok(keys
            .into_iter()
            .map(|k| {
                json!({"kid": k.kid, "name": k.name, "created_at": k.created_at, "revoked_at": k.revoked_at})
            })
            .collect())
    }

    /// Revokes a key: tokens signed with it are refused at once and the
    /// devices that connected with it are disconnected.
    pub async fn revoke_signing_key(&self, kid: &str) -> ApiResult<()> {
        let v = self
            .control()
            .call(Op::RevokeSigningKey { kid: kid.into() })
            .await?;
        if v["revoked"] != true {
            return Err(err("not_found", "no such signing key in force"));
        }
        Ok(())
    }

    /// Signs a client token with the newest key in force, as a developer's
    /// backend would (the `token sign` command, the demo, tests).
    pub fn sign_token(&self, user_id: &str, ttl_s: i64) -> ApiResult<String> {
        if ttl_s <= 0 || ttl_s > self.max_token_s() {
            return Err(err(
                "bad_request",
                format!("ttl is 1..{} seconds", self.max_token_s()),
            ));
        }
        let keys = self.inner.sh.signing.read().unwrap();
        let Some((kid, (secret, _))) = keys.iter().max_by_key(|(k, (_, at))| (*at, (*k).clone()))
        else {
            return Err(err(
                "conflict",
                "no signing key: create one with `agentim signing-key create`",
            ));
        };
        let now = now_ms() / 1000;
        Ok(crate::auth::jwt_sign(
            kid,
            secret,
            user_id,
            now,
            now + ttl_s,
        ))
    }

    fn max_token_s(&self) -> i64 {
        self.inner.cfg.auth.max_token_days * 86400
    }

    // ------------------------------------------------------------ connecting

    /// Who a credential is. A person: a signed client token and the
    /// device id their app chose; an agent: its key. None for anything
    /// refused (the caller answers 401 without saying why).
    pub async fn authenticate_client(
        &self,
        token: &str,
        device: Option<&str>,
        device_name: Option<&str>,
        platform: Option<&str>,
    ) -> Result<Login, ApiError> {
        self.authenticate_client_from(token, device, device_name, platform, None)
            .await
    }

    /// `authenticate_client` for a caller at `ip` (lookups of unknown keys
    /// are limited per address, D130).
    pub async fn authenticate_client_from(
        &self,
        token: &str,
        device: Option<&str>,
        device_name: Option<&str>,
        platform: Option<&str>,
        ip: Option<std::net::IpAddr>,
    ) -> Result<Login, ApiError> {
        let refused = |why: &str| err("unauthorized", why.to_string());
        if !crate::auth::looks_like_jwt(token) {
            // Agent keys. People only log in with signed tokens.
            let (user, device_id) = self
                .authenticate_or_load_from(token, ip)
                .await
                .ok_or_else(|| refused("unknown key"))?;
            if user.kind != UserKind::Agent {
                return Err(refused("people log in with a signed client token"));
            }
            return Ok(Login {
                user,
                device_id,
                hash: crate::auth::hash(token),
                jwt: None,
            });
        }
        let now_s = now_ms() / 1000;
        let verify = || {
            let keys = self.inner.sh.signing.read().unwrap();
            crate::auth::jwt_verify(
                token,
                |k| keys.get(k).map(|(s, _)| s.clone()),
                now_s,
                self.max_token_s(),
            )
        };
        let mut claims = verify();
        // A key created by another process or node a moment ago: read the
        // keys again (at most once a second, so made-up key ids cannot
        // load the database).
        if claims == Err(crate::auth::JwtError::UnknownKey) && self.may_look_up(ip) {
            self.load_signing().await?;
            claims = verify();
        }
        let claims = claims.map_err(|e| refused(e.as_str()))?;
        let Some(device) = device.filter(|d| valid_device(d)) else {
            return Err(err(
                "bad_request",
                "connect with device=<the id this app chose for the device, 1..64 of A-Z a-z 0-9 _ ->",
            ));
        };
        if !valid_label(device_name) || !valid_label(platform) {
            return Err(err(
                "bad_request",
                "device_name and platform are 1..64 bytes",
            ));
        }
        if !crate::open_api::valid_user_id(&claims.sub) {
            return Err(refused("subject"));
        }
        let user = match self.user_or_load(&claims.sub).await? {
            Some(u) => u,
            None => self.first_login(&claims.sub).await?,
        };
        if user.kind != UserKind::Human || user.name != claims.sub {
            return Err(refused("subject"));
        }
        // A valid token of someone the developer turned off: the app
        // should stop retrying (403, not 401, docs/08 §2.9).
        if self.is_deactivated(&user.id) {
            return Err(err("forbidden", "this person is deactivated"));
        }
        let device_id = device_id_of(&user.id, device);
        if self.cut_off(&user.id, &device_id, claims.iat) {
            return Err(refused("revoked"));
        }
        let hash = device_hash(&device_id);
        let issued_by = format!("jwt:{}", claims.kid);
        let known = self
            .inner
            .sh
            .tokens
            .read()
            .unwrap()
            .get(&hash)
            .is_some_and(|t| t.device_id == device_id);
        let row = crate::db::TokenRow {
            hash: hash.clone(),
            user_id: user.id.clone(),
            device_id: device_id.clone(),
            device_name: device_name.map(String::from),
            platform: platform.map(String::from),
            issued_by: Some(issued_by),
            created_at: now_ms(),
            expires_at: None,
            last_seen: Some(now_ms()),
        };
        let fresh = if known && self.last_issuer(&hash).as_deref() == row.issued_by.as_deref() {
            self.control().post(Op::TouchToken { hash: hash.clone() });
            false
        } else {
            let v = self
                .control()
                .call(Op::RegisterDevice { row: row.clone() })
                .await?;
            self.remember_issuer(&hash, row.issued_by.as_deref());
            // In a cluster the write may have run on another node; its
            // cache update reaches this one a moment later. The session is
            // only opened once this node knows the device (a revocation in
            // between keeps it unknown: retry, then refused).
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !self.inner.sh.tokens.read().unwrap().contains_key(&hash) {
                if std::time::Instant::now() > deadline {
                    return Err(err("unavailable", "the device is being registered; retry")
                        .retry_after(1000));
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            v["new"] == true
        };
        if fresh {
            // The user's other devices are told (security notice).
            self.inner
                .sh
                .reg
                .deliver(vec![crate::registry::Out::ToUser {
                    user_id: user.id.clone(),
                    frame: json!({"type": "device.added", "device": row.view()}),
                }]);
        }
        Ok(Login {
            user,
            device_id,
            hash,
            jwt: Some((claims.kid, claims.iat)),
        })
    }

    /// A person seen for the first time: created, as `PUT /v1/users/{id}`
    /// would. (Deleted users keep their row, so their ids are found, and
    /// refused, before this.)
    async fn first_login(&self, name: &str) -> ApiResult<User> {
        match self.create_user(name, UserKind::Human, None, None).await {
            Ok((u, _)) => Ok(u),
            // Another connection created them first.
            Err(e) if e.code == "conflict" => self
                .user_or_load(name)
                .await?
                .ok_or_else(|| err("unauthorized", "subject")),
            Err(e) => Err(e),
        }
    }

    /// Whether tokens of the user (or the device) issued at `iat` were
    /// revoked: issued no later than the second of the revocation (a token
    /// signed in that same second is refused too; the app gets a new one).
    pub(crate) fn cut_off(&self, user_id: &str, device_id: &str, iat: i64) -> bool {
        let c = self.inner.sh.cutoffs.read().unwrap();
        [String::new(), device_id.to_string()].into_iter().any(|d| {
            c.get(&(user_id.to_string(), d))
                .is_some_and(|at| iat <= *at)
        })
    }

    /// Whether a connection's credential still holds (checked once its
    /// session is registered, so a revocation in between is caught, M13).
    pub fn login_valid(&self, l: &Login) -> bool {
        let sh = &self.inner.sh;
        if sh.deactivated.read().unwrap().contains(&l.user.id) {
            return false;
        }
        let t = sh.tokens.read().unwrap().get(&l.hash).cloned();
        let Some(t) = t else { return false };
        if t.device_id != l.device_id || t.expires_at.is_some_and(|e| e <= now_ms()) {
            return false;
        }
        match &l.jwt {
            None => true,
            Some((kid, iat)) => {
                sh.signing.read().unwrap().contains_key(kid)
                    && !self.cut_off(&l.user.id, &l.device_id, *iat)
            }
        }
    }

    fn last_issuer(&self, hash: &str) -> Option<String> {
        self.inner.issuers.lock().unwrap().get(hash).cloned()
    }

    fn remember_issuer(&self, hash: &str, by: Option<&str>) {
        let mut m = self.inner.issuers.lock().unwrap();
        if m.len() > 100_000 {
            m.clear();
        }
        if let Some(by) = by {
            m.insert(hash.to_string(), by.to_string());
        }
    }
}

/// A stored signing secret in plain text.
fn open_secret(crypto: &crate::crypto::Crypto, kid: &str, stored: &str) -> Option<String> {
    if let Some(p) = stored.strip_prefix("p:") {
        return Some(p.to_string());
    }
    let w = stored.strip_prefix("k:")?;
    let bytes = crypto.kek.as_ref()?.unwrap(kid, w)?;
    String::from_utf8(bytes).ok()
}

/// Re-wraps stored signing secrets with a new master key (`rotate-key`).
pub fn rewrap_secret(
    old: &crate::crypto::Crypto,
    new: &crate::crypto::Kek,
    kid: &str,
    stored: &str,
) -> Option<String> {
    let plain = open_secret(old, kid, stored)?;
    Some(format!("k:{}", new.wrap(kid, plain.as_bytes())))
}
