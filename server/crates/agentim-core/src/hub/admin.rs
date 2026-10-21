//! Console accounts (docs/12 §4): the one-time setup, password + TOTP
//! login with lockout, sessions, admin keys for `agentim admin`, admin
//! management and the app settings changed in the console.

use serde_json::{Value, json};

use super::{Hub, err, now_ms};
use crate::db::admin::{AdminKeyRow, AdminRow, AdminSessionRow};
use crate::logic::{AdminOp, CredChange, Op};
use crate::model::*;

pub const ROLES: &[&str] = &["owner", "developer", "operator", "moderator"];
/// A session unused this long ends, and any session after `SESSION_MAX`.
pub const SESSION_IDLE_MS: i64 = 12 * 3600 * 1000;
pub const SESSION_MAX_MS: i64 = 7 * 24 * 3600 * 1000;

/// Who calls the admin API.
#[derive(Debug, Clone)]
pub struct Admin {
    pub id: String,
    pub name: String,
    pub role: String,
    /// The console session (cookie) and its CSRF token; None for a key.
    pub session: Option<(String, String)>,
}

impl Admin {
    pub fn actor(&self) -> String {
        format!("admin:{}", self.name)
    }
}

fn check_name(name: &str) -> ApiResult<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.@".contains(c))
    {
        return Err(err(
            "bad_request",
            "admin names are 1..64 of A-Z a-z 0-9 _ - . @",
        ));
    }
    Ok(())
}

fn check_role(role: &str) -> ApiResult<()> {
    if !ROLES.contains(&role) {
        return Err(err(
            "bad_request",
            "role is owner, developer, operator or moderator",
        ));
    }
    Ok(())
}

impl Hub {
    async fn admin_op(&self, op: AdminOp) -> ApiResult<Value> {
        self.control().call(Op::Admin(op)).await
    }

    pub async fn admins(&self) -> ApiResult<Vec<AdminRow>> {
        self.read(self.control(), |db| {
            Box::pin(async move { db.admins().await })
        })
        .await
    }

    async fn admin_by(&self, col: &'static str, v: &str) -> ApiResult<Option<AdminRow>> {
        let v = v.to_string();
        self.read(self.control(), move |db| {
            Box::pin(async move { db.admin_by(col, &v).await })
        })
        .await
    }

    // ------------------------------------------------------------ setup

    /// A one-time setup token while there is no admin (printed at start-up
    /// as a console link; only its hash is kept).
    pub async fn setup_token(&self) -> ApiResult<Option<String>> {
        if !self.admins().await?.is_empty() {
            return Ok(None);
        }
        let t = crate::auth::new_secret("setup");
        *self.inner.setup.lock().unwrap() = Some(crate::auth::hash(&t));
        Ok(Some(t))
    }

    pub async fn setup_needed(&self) -> ApiResult<bool> {
        Ok(self.admins().await?.is_empty() && self.inner.setup.lock().unwrap().is_some())
    }

    /// Creates the first owner with the setup token (used once).
    pub async fn admin_setup(
        &self,
        token: &str,
        name: &str,
        password: &str,
    ) -> ApiResult<(Admin, String)> {
        check_name(name)?;
        crate::auth::check_password_policy(name, password).map_err(|m| err("bad_request", m))?;
        {
            let mut s = self.inner.setup.lock().unwrap();
            if s.as_deref() != Some(crate::auth::hash(token).as_str()) {
                return Err(err("forbidden", "unknown or used setup link"));
            }
            *s = None;
        }
        if !self.admins().await?.is_empty() {
            return Err(err("conflict", "the console is already set up"));
        }
        let a = self.create_admin(name, "owner", password).await?;
        self.admin_session_for(&a).await
    }

    // ------------------------------------------------------------ admins

    pub async fn create_admin(
        &self,
        name: &str,
        role: &str,
        password: &str,
    ) -> ApiResult<AdminRow> {
        check_name(name)?;
        check_role(role)?;
        crate::auth::check_password_policy(name, password).map_err(|m| err("bad_request", m))?;
        let row = AdminRow {
            id: format!("adm_{}", crate::db::new_id()),
            name: name.to_string(),
            role: role.to_string(),
            created_at: now_ms(),
            disabled_at: None,
        };
        let pw = password.to_string();
        let hash = self
            .hash_job(move || crate::auth::hash_password(&pw))
            .await?;
        self.admin_op(AdminOp::Create(row.clone())).await?;
        self.cred(&row.id, CredChange::SetPassword(hash)).await?;
        Ok(row)
    }

    pub async fn update_admin(
        &self,
        name: &str,
        role: Option<&str>,
        disabled: Option<bool>,
    ) -> ApiResult<AdminRow> {
        if let Some(r) = role {
            check_role(r)?;
        }
        let a = self
            .admin_by("name", name)
            .await?
            .ok_or_else(|| err("not_found", "no such admin"))?;
        if role.is_none() && disabled.is_none() {
            return Err(err("bad_request", "nothing to change"));
        }
        self.admin_op(AdminOp::Update {
            id: a.id.clone(),
            role: role.map(String::from),
            disabled,
        })
        .await?;
        self.admin_by("id", &a.id)
            .await?
            .ok_or_else(|| err("not_found", "no such admin"))
    }

    /// An owner sets another admin's password; their sessions end.
    pub async fn reset_admin_password(&self, name: &str, password: &str) -> ApiResult<()> {
        let a = self
            .admin_by("name", name)
            .await?
            .ok_or_else(|| err("not_found", "no such admin"))?;
        self.store_admin_password(&a, password).await?;
        self.admin_op(AdminOp::SessionsDelete {
            admin_id: Some(a.id.clone()),
            hash: None,
        })
        .await?;
        Ok(())
    }

    async fn store_admin_password(&self, a: &AdminRow, password: &str) -> ApiResult<()> {
        crate::auth::check_password_policy(&a.name, password).map_err(|m| err("bad_request", m))?;
        let pw = password.to_string();
        let hash = self
            .hash_job(move || crate::auth::hash_password(&pw))
            .await?;
        self.cred(&a.id, CredChange::SetPassword(hash)).await?;
        Ok(())
    }

    // ------------------------------------------------------------ login

    /// Password (and TOTP) login; a new session. Every failure looks the
    /// same except a missing second factor; unknown names cost a hash too.
    pub async fn admin_login(
        &self,
        name: &str,
        password: &str,
        totp: Option<&str>,
    ) -> ApiResult<(Admin, String)> {
        let invalid = || err("invalid_credentials", "wrong name, password or code");
        if name.len() > 255 || password.len() > 1024 {
            return Err(err("bad_request", "too long"));
        }
        let a = self.admin_by("name", name).await?;
        let c = match &a {
            Some(a) => self.credentials(&a.id).await?,
            None => Default::default(),
        };
        let now = now_ms();
        if let Some(until) = c.locked_until
            && until > now
        {
            return Err(err(
                "rate_limited",
                format!(
                    "too many failed logins; retry in {}s",
                    (until - now) / 1000 + 1
                ),
            ));
        }
        let hash = c
            .password_hash
            .clone()
            .unwrap_or_else(|| crate::auth::dummy_hash().to_string());
        let pw = password.to_string();
        let ok = self
            .hash_job(move || crate::auth::verify_password(&pw, &hash))
            .await?;
        let (Some(a), true, true) = (a, ok, c.password_hash.is_some()) else {
            if let Some(a) = self.admin_by("name", name).await? {
                self.login_failed(&a.id).await?;
            }
            return Err(invalid());
        };
        let mut step = None;
        if let Some(secret) = &c.totp_secret {
            let Some(code) = totp else {
                return Err(err(
                    "totp_required",
                    "enter the code from your authenticator app",
                ));
            };
            match crate::auth::totp_check(secret, code, now / 1000) {
                Some(s) => step = Some(s),
                None => {
                    self.login_failed(&a.id).await?;
                    return Err(invalid());
                }
            }
        }
        if a.disabled_at.is_some() {
            return Err(err("forbidden", "the admin is disabled"));
        }
        self.cred(&a.id, CredChange::Succeeded { totp_step: step })
            .await
            .map_err(|e| if e.code == "conflict" { invalid() } else { e })?;
        self.admin_session_for(&a).await
    }

    async fn admin_session_for(&self, a: &AdminRow) -> ApiResult<(Admin, String)> {
        let token = crate::auth::new_secret("ads");
        let csrf = crate::auth::new_secret("csrf");
        let now = now_ms();
        let hash = crate::auth::hash(&token);
        self.admin_op(AdminOp::SessionPut(AdminSessionRow {
            hash: hash.clone(),
            admin_id: a.id.clone(),
            csrf: csrf.clone(),
            created_at: now,
            last_seen: now,
        }))
        .await?;
        Ok((
            Admin {
                id: a.id.clone(),
                name: a.name.clone(),
                role: a.role.clone(),
                session: Some((hash, csrf)),
            },
            token,
        ))
    }

    /// The admin of a console session cookie, if it is still good.
    pub async fn admin_from_session(&self, token: &str) -> ApiResult<Option<Admin>> {
        let hash = crate::auth::hash(token);
        let h = hash.clone();
        let Some(s) = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.admin_session(&h).await })
            })
            .await?
        else {
            return Ok(None);
        };
        let now = now_ms();
        if now - s.last_seen > SESSION_IDLE_MS || now - s.created_at > SESSION_MAX_MS {
            self.admin_op(AdminOp::SessionsDelete {
                admin_id: None,
                hash: Some(hash),
            })
            .await?;
            return Ok(None);
        }
        let Some(a) = self.admin_by("id", &s.admin_id).await? else {
            return Ok(None);
        };
        if a.disabled_at.is_some() {
            return Ok(None);
        }
        // At most one write a minute per session.
        if now - s.last_seen > 60_000 {
            self.control()
                .post(Op::Admin(AdminOp::SessionTouch { hash: hash.clone() }));
        }
        Ok(Some(Admin {
            id: a.id,
            name: a.name,
            role: a.role,
            session: Some((hash, s.csrf)),
        }))
    }

    pub async fn admin_logout(&self, a: &Admin) -> ApiResult<()> {
        if let Some((hash, _)) = &a.session {
            self.admin_op(AdminOp::SessionsDelete {
                admin_id: None,
                hash: Some(hash.clone()),
            })
            .await?;
        }
        Ok(())
    }

    /// Changes one's own password (the current one is needed); other
    /// sessions end.
    pub async fn admin_change_password(
        &self,
        a: &Admin,
        current: &str,
        new: &str,
    ) -> ApiResult<()> {
        let row = self
            .admin_by("id", &a.id)
            .await?
            .ok_or_else(|| err("not_found", "no such admin"))?;
        let c = self.credentials(&a.id).await?;
        let h = c.password_hash.unwrap_or_default();
        let cur = current.to_string();
        if !self
            .hash_job(move || crate::auth::verify_password(&cur, &h))
            .await?
        {
            return Err(err("forbidden", "the current password is wrong"));
        }
        self.store_admin_password(&row, new).await?;
        self.admin_op(AdminOp::SessionsDelete {
            admin_id: Some(a.id.clone()),
            hash: None,
        })
        .await?;
        Ok(())
    }

    pub async fn admin_totp_setup(&self, a: &Admin) -> ApiResult<Value> {
        if self.credentials(&a.id).await?.totp_secret.is_some() {
            return Err(err(
                "conflict",
                "two-factor login is already on; turn it off first",
            ));
        }
        let secret = crate::auth::new_totp_secret();
        self.cred(&a.id, CredChange::TotpPending(secret.clone()))
            .await?;
        Ok(
            json!({"secret": secret, "uri": crate::auth::otpauth_uri("AgentIM console", &a.name, &secret)}),
        )
    }

    pub async fn admin_totp_enable(&self, a: &Admin, code: &str) -> ApiResult<()> {
        let c = self.credentials(&a.id).await?;
        let Some(p) = c.totp_pending else {
            return Err(err("conflict", "set up two-factor login first"));
        };
        let step = crate::auth::totp_check(&p, code, now_ms() / 1000)
            .ok_or_else(|| err("bad_request", "wrong code"))?;
        self.cred(&a.id, CredChange::TotpEnable { step }).await?;
        Ok(())
    }

    pub async fn admin_totp_disable(&self, a: &Admin, code: &str) -> ApiResult<()> {
        let c = self.credentials(&a.id).await?;
        let Some(s) = c.totp_secret else {
            return Err(err("conflict", "two-factor login is off"));
        };
        let step = crate::auth::totp_check(&s, code, now_ms() / 1000)
            .ok_or_else(|| err("bad_request", "wrong code"))?;
        self.cred(
            &a.id,
            CredChange::Succeeded {
                totp_step: Some(step),
            },
        )
        .await
        .map_err(|_| err("bad_request", "this code was already used"))?;
        self.cred(&a.id, CredChange::TotpDisable).await?;
        Ok(())
    }

    pub async fn admin_totp_on(&self, admin_id: &str) -> ApiResult<bool> {
        Ok(self.credentials(admin_id).await?.totp_secret.is_some())
    }

    /// Ends sessions unused too long (a ticker calls it).
    pub async fn expire_admin_sessions(&self) -> ApiResult<u64> {
        let now = now_ms();
        let v = self
            .admin_op(AdminOp::SessionsExpire {
                idle_before: now - SESSION_IDLE_MS,
                max_before: now - SESSION_MAX_MS,
            })
            .await?;
        Ok(v["deleted"].as_u64().unwrap_or(0))
    }

    // ------------------------------------------------------------ keys

    /// A key for `agentim admin`; the secret is shown once.
    pub async fn create_admin_key(
        &self,
        name: &str,
        role: &str,
    ) -> ApiResult<(String, AdminKeyRow)> {
        if name.is_empty() || name.len() > 64 || name.contains('\0') {
            return Err(err("bad_request", "a key name is 1..64 bytes"));
        }
        check_role(role)?;
        let key_id = format!(
            "adk{}",
            &crate::auth::hash(&crate::auth::new_secret("k"))[..12]
        );
        let secret = format!("{}_{}", key_id, &crate::auth::new_secret("x")[2..]);
        let row = AdminKeyRow {
            key_id,
            hash: crate::auth::hash(&secret),
            name: name.to_string(),
            role: role.to_string(),
            created_at: now_ms(),
            revoked_at: None,
        };
        self.admin_op(AdminOp::KeyPut(row.clone())).await?;
        Ok((secret, row))
    }

    pub async fn admin_keys(&self) -> ApiResult<Vec<AdminKeyRow>> {
        self.read(self.control(), |db| {
            Box::pin(async move { db.admin_keys().await })
        })
        .await
    }

    pub async fn revoke_admin_key(&self, key_id: &str) -> ApiResult<()> {
        let v = self
            .admin_op(AdminOp::KeyRevoke {
                key_id: key_id.to_string(),
            })
            .await?;
        if v["revoked"] != true {
            return Err(err("not_found", "no such key in force"));
        }
        Ok(())
    }

    /// The admin an `adk…` key stands for.
    pub async fn admin_from_key(&self, secret: &str) -> ApiResult<Option<Admin>> {
        let hash = crate::auth::hash(secret);
        Ok(self
            .admin_keys()
            .await?
            .into_iter()
            .find(|k| k.hash == hash && k.revoked_at.is_none())
            .map(|k| Admin {
                id: k.key_id.clone(),
                name: format!("key:{}", k.name),
                role: k.role,
                session: None,
            }))
    }

    // ------------------------------------------------------------ settings

    /// Reads the app settings changed in the console (start-up).
    pub(crate) async fn load_app_settings(&self) -> ApiResult<()> {
        let v = self
            .read(self.control(), |db| {
                Box::pin(async move { db.app_setting("default", "clients.allow").await })
            })
            .await?;
        if let Some(a) = v.and_then(|v| serde_json::from_value(v).ok()) {
            *self.inner.sh.allow.write().unwrap() = a;
        }
        Ok(())
    }

    pub fn client_defaults(&self) -> crate::config::AllowConfig {
        self.inner.sh.allow.read().unwrap().clone()
    }

    /// The app's defaults of what people may do in their client.
    pub async fn set_client_defaults(&self, v: &Value) -> ApiResult<crate::config::AllowConfig> {
        if !v.is_object() {
            return Err(err("bad_request", "the client defaults are a JSON object"));
        }
        let a: crate::config::AllowConfig =
            serde_json::from_value(v.clone()).map_err(|e| err("bad_request", e.to_string()))?;
        if [a.edit_minutes, a.recall_minutes]
            .iter()
            .any(|m| m.is_some_and(|m| !(0..=525_600).contains(&m)))
        {
            return Err(err(
                "bad_request",
                "edit_minutes and recall_minutes are 0..525600",
            ));
        }
        self.admin_op(AdminOp::Setting {
            tenant: "default".into(),
            key: "clients.allow".into(),
            value: json!(a),
        })
        .await?;
        Ok(a)
    }
}

// ---------------------------------------------------------------- views

impl Hub {
    pub(crate) async fn control_call(&self, op: Op) -> ApiResult<Value> {
        self.control().call(op).await
    }

    /// The console's front page of the app.
    pub async fn admin_overview(&self) -> ApiResult<Value> {
        let users = self.users();
        let people = users.iter().filter(|u| u.kind == UserKind::Human).count();
        let agents = users
            .iter()
            .filter(|u| u.kind == UserKind::Agent && !self.is_destroyed(&u.id))
            .count();
        let mut conversations = 0i64;
        for s in &self.inner.shards {
            let v = s.call(Op::Stats).await?;
            conversations += v["counts"]["conversations"].as_i64().unwrap_or(0);
        }
        let open_reports = self.reports(true, 200, 0).await?.len();
        Ok(json!({
            "people": people,
            "agents": agents,
            "conversations": conversations,
            "online": self.inner.sh.reg.online_users().len(),
            "open_reports": open_reports,
            "api_keys": self.api_keys().iter().filter(|k| k.revoked_at.is_none()).count(),
            "signing_keys": self.inner.sh.signing.read().unwrap().len(),
        }))
    }

    /// People and agents by id, after `after`, names containing `q`.
    pub fn admin_users(
        &self,
        q: Option<&str>,
        kind: Option<UserKind>,
        after: Option<&str>,
        limit: i64,
    ) -> Value {
        let mut all: Vec<User> = self
            .users()
            .into_iter()
            .filter(|u| kind.is_none_or(|k| u.kind == k))
            .filter(|u| q.is_none_or(|q| u.name.contains(q)))
            .filter(|u| after.is_none_or(|a| u.name.as_str() > a))
            .filter(|u| !self.is_destroyed(&u.id))
            .collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        let more = all.len() as i64 > limit;
        all.truncate(limit as usize);
        let sh = &self.inner.sh;
        let items: Vec<Value> = all
            .iter()
            .map(|u| {
                json!({
                    "id": u.name,
                    "kind": u.kind,
                    "owner": u.owner_id.as_deref().and_then(|o| sh.user(o)).map(|o| o.name),
                    "team": u.team,
                    "deactivated": self.is_deactivated(&u.id),
                    "stopped": self.is_stopped(&u.id),
                    "online": sh.reg.sessions_of_user(&u.id) > 0,
                })
            })
            .collect();
        json!({"users": items, "has_more": more})
    }

    /// One user: profile, devices, agents, permissions.
    pub async fn admin_user(&self, u: &User) -> ApiResult<Value> {
        let mut p = self.user_profile(&u.id).await?.unwrap_or_default();
        let sh = &self.inner.sh;
        if let Some(o) = u.owner_id.as_deref().and_then(|o| sh.user(o)) {
            p["owner"] = json!(o.name);
        }
        let devices: Vec<Value> = self
            .devices(&u.id)
            .await?
            .iter()
            .map(|d| d.view())
            .collect();
        p["devices"] = json!(devices);
        p["stopped"] = json!(self.is_stopped(&u.id));
        p["online"] = json!(sh.reg.sessions_of_user(&u.id) > 0);
        if u.kind == UserKind::Human {
            p["permissions"] = json!(self.permissions(u));
            p["permission_overrides"] = json!(self.permission_overrides(&u.id));
            let agents: Vec<Value> = sh
                .agents_of(&u.id)
                .iter()
                .filter(|a| !self.is_destroyed(&a.id))
                .map(|a| json!({"id": a.name, "team": a.team, "stopped": self.is_stopped(&a.id)}))
                .collect();
            p["agents"] = json!(agents);
        }
        Ok(p)
    }

    /// Conversations by id over every database (metadata only).
    pub async fn admin_conversations(
        &self,
        q: Option<&str>,
        after: Option<&str>,
        limit: i64,
    ) -> ApiResult<Value> {
        let mut all = vec![];
        for s in &self.inner.shards {
            let (q, a) = (q.map(String::from), after.map(String::from));
            let page = self
                .read(s, move |db| {
                    Box::pin(async move {
                        db.conversations_page(q.as_deref(), a.as_deref(), limit + 1)
                            .await
                    })
                })
                .await?;
            all.extend(page);
        }
        all.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        let more = all.len() as i64 > limit;
        all.truncate(limit as usize);
        Ok(json!({"conversations": all, "has_more": more}))
    }

    /// One conversation's metadata: members with roles, no content.
    pub async fn admin_conversation(&self, conv_id: &str) -> ApiResult<Value> {
        self.conv_detail(conv_id).await
    }

    /// Configured webhooks (never their secrets) and the events waiting.
    pub async fn admin_webhooks(&self) -> ApiResult<Value> {
        let mut queued = 0i64;
        for s in &self.inner.shards {
            let v = s.call(Op::Stats).await?;
            queued += v["counts"]["webhook_outbox"].as_i64().unwrap_or(0);
        }
        let hooks: Vec<Value> = self
            .inner
            .cfg
            .webhooks
            .iter()
            .map(|w| json!({"name": w.name, "url": w.url, "events": w.events, "max_attempts": w.max_attempts}))
            .collect();
        Ok(
            json!({"webhooks": hooks, "queued": queued, "before_send": self.inner.cfg.before_send.as_ref().map(|b| json!({"url": b.url}))}),
        )
    }

    /// Audit records over every database, newest first.
    pub async fn admin_audit(
        &self,
        prefix: Option<&str>,
        before: Option<&str>,
        limit: i64,
    ) -> ApiResult<Value> {
        let mut all = vec![];
        for s in &self.inner.shards {
            let (p, b) = (prefix.map(String::from), before.map(String::from));
            let page = self
                .read(s, move |db| {
                    Box::pin(
                        async move { db.audit_page(p.as_deref(), b.as_deref(), limit + 1).await },
                    )
                })
                .await?;
            all.extend(page);
        }
        all.sort_by(|a, b| b["id"].as_str().cmp(&a["id"].as_str()));
        let more = all.len() as i64 > limit;
        all.truncate(limit as usize);
        let sh = &self.inner.sh;
        for r in all.iter_mut() {
            if let Some(u) = r["actor_id"].as_str().and_then(|a| sh.user(a)) {
                r["actor"] = json!(u.name);
            } else {
                r["actor"] = r["actor_id"].clone();
            }
        }
        Ok(json!({"records": all, "has_more": more}))
    }

    /// What the deployer looks at: databases and writers, connections,
    /// encryption, version.
    pub fn admin_deploy_status(&self) -> Value {
        let mut h = self.health();
        let cfg = &self.inner.cfg;
        let dbs: Vec<Value> = self
            .inner
            .shards
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "dialect": s.dialect.as_str(),
                    "mode": format!("{:?}", s.mode).to_lowercase(),
                    "encrypted": s.crypto.encrypt_new,
                    "writer": s.writer_stats(),
                })
            })
            .collect();
        h["database_details"] = json!(dbs);
        h["version"] = json!(env!("CARGO_PKG_VERSION"));
        h["control"] = json!(cfg.storage.control);
        h["units"] = json!(
            cfg.units
                .iter()
                .map(|u| json!({"name": u.name, "database": u.database}))
                .collect::<Vec<_>>()
        );
        h["clustered"] = json!(self.clustered());
        h["connections"] = json!(self.inner.sh.reg.online_users().len());
        h
    }

    /// `agentim cluster status` / `drain` through the cluster crate.
    pub async fn cluster_admin(&self, cmd: crate::cli::ClusterCmd) -> ApiResult<Value> {
        let Some(f) = self.inner.cluster_query.get() else {
            return Err(err("bad_request", "this server is not part of a cluster"));
        };
        f(self.inner.cfg.clone(), cmd)
            .await
            .map_err(|e| err("bad_request", e))
    }

    /// Lets the admin API reach the cluster crate's status and drain.
    pub fn set_cluster_query(&self, f: crate::cli::ClusterQuery) {
        let _ = self.inner.cluster_query.set(f);
    }
}
