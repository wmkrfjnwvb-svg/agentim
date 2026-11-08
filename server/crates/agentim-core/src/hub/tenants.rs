//! Tenants of a multi-tenant server (docs/en/design/multi-tenancy.md): the registry, which tenant a
//! request's host names, and the deployment's operations on tenants.

use serde_json::{Value, json};

use super::*;
use crate::db::admin::TenantRow;
use crate::logic::AdminOp;
use crate::tenant;

impl Hub {
    pub(crate) async fn load_tenants(&self) -> ApiResult<()> {
        let rows = self
            .read(self.control(), |db| {
                Box::pin(async move { db.tenants_all().await })
            })
            .await?;
        *self.inner.tenants.write().unwrap() =
            rows.into_iter().map(|t| (t.id.clone(), t)).collect();
        // Webhooks follow: a suspended tenant's stop, a resumed one's start.
        self.load_tenant_hooks().await
    }

    /// Whether this server serves several tenants (`[tenancy] enabled`).
    pub fn multi_tenant(&self) -> bool {
        self.inner.cfg.tenancy.enabled
    }

    /// The tenant a request's `Host` names: `default` on a single-tenant
    /// server; on a multi-tenant one `<tenant>.<host_suffix>` of a tenant
    /// that exists and is not suspended, else none (the caller answers 404
    /// or 401 without saying why).
    pub fn tenant_for_host(&self, host: Option<&str>) -> Option<String> {
        if !self.multi_tenant() {
            return Some(tenant::DEFAULT.to_string());
        }
        let host = host?.trim().to_ascii_lowercase();
        let host = host.rsplit_once(':').map_or(host.as_str(), |(h, port)| {
            if port.bytes().all(|b| b.is_ascii_digit()) {
                h
            } else {
                host.as_str()
            }
        });
        let suffix = format!(
            ".{}",
            self.inner.cfg.tenancy.host_suffix.to_ascii_lowercase()
        );
        let sub = host.strip_suffix(&suffix)?;
        if !tenant::valid(sub) {
            return None;
        }
        let t = self.inner.tenants.read().unwrap();
        t.get(sub)
            .filter(|t| t.suspended_at.is_none())
            .map(|t| t.id.clone())
    }

    /// A tenant's limit (`limits.<key>` as a number), none when unset or
    /// for `default`.
    pub fn tenant_limit(&self, tenant: &str, key: &str) -> Option<i64> {
        if tenant == tenant::DEFAULT {
            // A tenant's own instance carries its plan's limits;
            // a self-hosted server has none.
            return self
                .inner
                .own_limits
                .read()
                .unwrap()
                .get(key)
                .and_then(Value::as_i64);
        }
        self.inner
            .tenants
            .read()
            .unwrap()
            .get(tenant)
            .and_then(|t| t.limits.get(key))
            .and_then(Value::as_i64)
    }

    /// Refuses a new connection over the tenant's `connections` limit
    /// (this node's sessions, docs/en/design/multi-tenancy.md).
    pub fn tenant_connection_allowed(&self, tenant: &str) -> ApiResult<()> {
        if let Some(max) = self.tenant_limit(tenant, "connections")
            && self.tenant_connections(tenant) >= max
        {
            return Err(rule!(
                "tenant.connections",
                "rate_limited",
                format!("this project allows {max} connections at once")
            )
            .retry_after(5000));
        }
        Ok(())
    }

    /// Keeps the day's peak of connections at once (called on each new
    /// connection). A tenant is open on one node only (docs/en/design/multi-tenancy.md), so this
    /// instance's count is the tenant's.
    pub(crate) fn note_connections(&self) {
        let n = self.inner.sh.reg.session_count() as i64;
        let day = utc_day(now_ms());
        let mut p = self.inner.conn_peak.lock().unwrap();
        if p.0 != day {
            *p = (day, n);
        } else if n > p.1 {
            p.1 = n;
        }
    }

    /// The peak of connections at once on `day` (UTC), at least the
    /// current count when `day` is today.
    pub fn peak_connections(&self, day: &str) -> i64 {
        let now = self.inner.sh.reg.session_count() as i64;
        let p = self.inner.conn_peak.lock().unwrap();
        let today = utc_day(now_ms());
        match (p.0 == day, day == today) {
            (true, true) => p.1.max(now),
            (true, false) => p.1,
            (false, true) => now,
            (false, false) => 0,
        }
    }

    /// A tenant's connections on this instance (a tenant's own instance
    /// holds only its own people).
    pub fn tenant_connections(&self, tenant: &str) -> i64 {
        if tenant == tenant::DEFAULT {
            self.inner.sh.reg.session_count() as i64
        } else {
            0
        }
    }

    /// Refuses a person who would be a new monthly active user once the
    /// tenant's `mau` limit is reached (the spend cap is on). `known`: the
    /// person exists (none: a first login).
    pub(crate) async fn tenant_mau_allowed(
        &self,
        tenant: &str,
        user: Option<&str>,
    ) -> ApiResult<()> {
        let Some(max) = self.tenant_limit(tenant, "mau") else {
            return Ok(());
        };
        let month = day_start_ms(&format!("{}-01", &utc_day(now_ms())[..7])).unwrap_or(0);
        if let Some(u) = user {
            let u = u.to_string();
            let seen = self
                .read(self.control(), move |db| {
                    let u = u.clone();
                    Box::pin(async move { db.user_seen_since(&u, month).await })
                })
                .await?;
            if seen {
                return Ok(());
            }
        }
        // The month's count, read at most every 30 seconds and counted up
        // here for each person let in meanwhile; checking and counting are
        // one step under the lock, so people arriving together cannot all
        // take the last place.
        let fresh = |c: &HashMap<String, (i64, std::time::Instant, i64)>| {
            c.get(tenant).is_some_and(|(_, at, m)| {
                at.elapsed() < std::time::Duration::from_secs(30) && *m == month
            })
        };
        if !fresh(&self.inner.mau_cache.lock().unwrap()) {
            let t = tenant.to_string();
            let n = self
                .read(self.control(), move |db| {
                    let t = t.clone();
                    Box::pin(async move { db.tenant_mau(&t, month).await })
                })
                .await?;
            let mut c = self.inner.mau_cache.lock().unwrap();
            // Another request may have refreshed it meanwhile and counted
            // people since: keep the larger count.
            if !fresh(&c) || c.get(tenant).is_some_and(|e| e.0 < n) {
                c.insert(tenant.to_string(), (n, std::time::Instant::now(), month));
            }
        }
        let mut c = self.inner.mau_cache.lock().unwrap();
        let e = c.get_mut(tenant).expect("the count was just read");
        if e.0 >= max {
            return Err(rule!(
                "tenant.mau",
                "rate_limited",
                format!(
                    "this project has reached its {max} monthly active users; new people can sign in next month or after an upgrade"
                )
            ));
        }
        e.0 += 1;
        Ok(())
    }

    /// Takes one request from the tenant's `requests_per_second` bucket
    /// (burst: twice the rate).
    pub fn tenant_request_allowed(&self, tenant: &str) -> ApiResult<()> {
        let Some(rate) = self.tenant_limit(tenant, "requests_per_second") else {
            return Ok(());
        };
        let rate = rate.max(1) as f64;
        let mut b = self.inner.tenant_buckets.lock().unwrap();
        let now = std::time::Instant::now();
        let e = b.entry(tenant.to_string()).or_insert((rate * 2.0, now));
        e.0 = (e.0 + now.duration_since(e.1).as_secs_f64() * rate).min(rate * 2.0);
        e.1 = now;
        if e.0 < 1.0 {
            return Err(rule!(
                "tenant.rate",
                "rate_limited",
                "this project is sending too many requests"
            )
            .retry_after(1000));
        }
        e.0 -= 1.0;
        Ok(())
    }

    /// Whether a tenant exists and is not suspended (`default` always is).
    pub fn tenant_active(&self, id: &str) -> bool {
        id == tenant::DEFAULT
            || self
                .inner
                .tenants
                .read()
                .unwrap()
                .get(id)
                .is_some_and(|t| t.suspended_at.is_none())
    }

    /// A new tenant (the deployment's admin API, called by the cloud API).
    pub async fn create_tenant(&self, id: &str, name: &str, limits: Value) -> ApiResult<Value> {
        self.create_tenant_in(id, name, limits, None).await
    }

    /// A new tenant on a database cluster (none: the first configured;
    /// ignored when tenants share the deployment's databases). Its own
    /// database is created first, so a refused tenant leaves no row.
    pub async fn create_tenant_in(
        &self,
        id: &str,
        name: &str,
        limits: Value,
        cluster: Option<&str>,
    ) -> ApiResult<Value> {
        if !tenant::valid(id) || id == tenant::DEFAULT {
            return Err(rule!(
                "tenant.id",
                "bad_request",
                format!(
                    "a tenant id is 1..{} of a-z 0-9 -, not starting or ending with -, and not `default`",
                    tenant::MAX_LEN
                ),
            ));
        }
        if name.trim().is_empty() || name.len() > 255 || name.contains('\0') {
            return Err(rule!(
                "tenant.name",
                "bad_request",
                "a tenant name is 1..255 bytes"
            ));
        }
        if !(limits.is_object() || limits.is_null()) {
            return Err(rule!(
                "tenant.limits",
                "bad_request",
                "limits is a JSON object"
            ));
        }
        let cluster = self.pick_cluster(cluster)?;
        if self.inner.tenants.read().unwrap().contains_key(id) {
            return Err(rule!("tenant.exists", "conflict", "the tenant exists"));
        }
        if let Some(c) = &cluster {
            self.create_silo_database(id, c).await?;
        }
        let row = TenantRow {
            id: id.to_string(),
            name: name.trim().to_string(),
            limits: if limits.is_null() { json!({}) } else { limits },
            suspended_at: None,
            created_at: now_ms(),
            cluster,
        };
        let v = self.admin_op(AdminOp::TenantCreate(row.clone())).await?;
        self.inner
            .tenants
            .write()
            .unwrap()
            .insert(row.id.clone(), row);
        Ok(v)
    }

    /// Every tenant.
    pub async fn tenants(&self) -> ApiResult<Vec<Value>> {
        self.load_tenants().await?;
        Ok(self
            .inner
            .tenants
            .read()
            .unwrap()
            .values()
            .map(TenantRow::view)
            .collect())
    }

    /// Changes a tenant's name or limits, or suspends / resumes it. A
    /// suspended tenant's requests are refused and its connections closed.
    pub async fn update_tenant(
        &self,
        id: &str,
        name: Option<&str>,
        limits: Option<Value>,
        suspended: Option<bool>,
    ) -> ApiResult<Value> {
        if name.is_none() && limits.is_none() && suspended.is_none() {
            return Err(rule!("tenant.nothing", "bad_request", "nothing to change"));
        }
        if name.is_some_and(|n| n.trim().is_empty() || n.len() > 255 || n.contains('\0')) {
            return Err(rule!(
                "tenant.name",
                "bad_request",
                "a tenant name is 1..255 bytes"
            ));
        }
        if limits.as_ref().is_some_and(|l| !l.is_object()) {
            return Err(rule!(
                "tenant.limits",
                "bad_request",
                "limits is a JSON object"
            ));
        }
        self.admin_op(AdminOp::TenantUpdate {
            id: id.to_string(),
            name: name.map(|n| n.trim().to_string()),
            limits,
            suspended_at: suspended.map(|s| s.then(now_ms)),
        })
        .await?;
        self.load_tenants().await?;
        // An open instance takes its new limits at once.
        let row = self.inner.tenants.read().unwrap().get(id).cloned();
        if let (Some(row), Some(h)) = (row, self.inner.silos.lock().await.get(id).cloned()) {
            h.set_own_limits(row.limits);
        }
        if suspended == Some(false) && self.siloed() {
            // Resumed: open again, so its queued webhooks go out.
            let _ = self.tenant_hub(id).await;
        }
        if suspended == Some(true) {
            // Its own instance closes and its connections end.
            if let Some(h) = self.inner.silos.lock().await.remove(id) {
                h.close_all_sessions();
                h.close().await;
            }
        }
        let t = self.inner.tenants.read().unwrap();
        Ok(t.get(id).map(TenantRow::view).unwrap_or(Value::Null))
    }

    /// Closes every connection (a tenant's instance suspended or deleted).
    pub(crate) fn close_all_sessions(&self) {
        let users: Vec<String> = self
            .inner
            .sh
            .users
            .read()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        for u in users {
            self.inner.sh.reg.kick(&u, None);
        }
    }

    /// A tenant's usage for a UTC day (`YYYY-MM-DD`), what the cloud bills
    /// by (docs/en/design/multi-tenancy.md): `mau` is the month's distinct people so far
    /// (agents do not count), `messages` and `agent_messages` that day,
    /// `storage_bytes` and `connections` now (this node's connections).
    pub async fn tenant_usage(&self, id: &str, day: &str) -> ApiResult<Value> {
        let Some(start) = day_start_ms(day) else {
            return Err(rule!("tenant.day", "bad_request", "day is YYYY-MM-DD"));
        };
        if !self.inner.tenants.read().unwrap().contains_key(id) && id != tenant::DEFAULT {
            return Err(rule!("tenant.unknown", "not_found", "no such tenant"));
        }
        // A tenant in its own database: everything there is its.
        if self.siloed() && id != tenant::DEFAULT {
            let h = self.tenant_hub(id).await?;
            let mut v = Box::pin(h.tenant_usage(tenant::DEFAULT, day)).await?;
            v["tenant"] = json!(id);
            return Ok(v);
        }
        let month = day_start_ms(&format!("{}-01", &day[..7])).unwrap_or(start);
        let t = id.to_string();
        let mau = self
            .read(self.control(), move |db| {
                let t = t.clone();
                Box::pin(async move { db.tenant_mau(&t, month).await })
            })
            .await?;
        let (mut messages, mut agent_messages, mut storage) = (0i64, 0i64, 0i64);
        for s in &self.inner.shards {
            let t = id.to_string();
            let (m, a, b) = self
                .read(s, move |db| {
                    let t = t.clone();
                    Box::pin(async move {
                        let (m, a) = db.tenant_messages(&t, start, start + 86_400_000).await?;
                        Ok((m, a, db.tenant_storage(&t).await?))
                    })
                })
                .await?;
            messages += m;
            agent_messages += a;
            storage += b;
        }
        // A tenant is open on one node only (docs/en/design/multi-tenancy.md): its count is here.
        let connections = self.tenant_connections(id);
        let peak = if id == tenant::DEFAULT {
            self.peak_connections(day)
        } else {
            connections
        };
        // What built-in model keys have cost so far, ever (docs/en/design/assistants.md).
        let ai_cost = self
            .read(self.control(), |db| {
                Box::pin(async move { db.builtin_cost_since(0).await })
            })
            .await?;
        Ok(json!({
            "tenant": id, "day": day, "mau": mau, "messages": messages,
            "agent_messages": agent_messages, "storage_bytes": storage,
            "connections": connections, "peak_connections": peak, "ai_builtin_cost_nanos": ai_cost,
        }))
    }
}

/// The UTC date (`YYYY-MM-DD`) of a time in milliseconds.
pub fn utc_day(ms: i64) -> String {
    // Howard Hinnant's civil_from_days.
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Midnight UTC of `YYYY-MM-DD` in milliseconds.
pub(crate) fn day_start_ms(day: &str) -> Option<i64> {
    let b = day.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i64 = day[..4].parse().ok()?;
    let m: i64 = day[5..7].parse().ok()?;
    let d: i64 = day[8..].parse().ok()?;
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days_in = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1970..=9999).contains(&y)
        || !(1..=12).contains(&m)
        || d < 1
        || d > days_in[(m - 1) as usize]
    {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400_000)
}

#[cfg(test)]
mod tests {
    #[test]
    fn days() {
        use super::day_start_ms as d;
        assert_eq!(d("1970-01-01"), Some(0));
        assert_eq!(d("2000-02-29"), Some(951_782_400_000));
        assert_eq!(d("2026-11-01"), Some(1_793_491_200_000));
        for bad in [
            "2026-02-29",
            "1900-02-28",
            "2026-13-01",
            "2026-00-10",
            "2026-1-01",
            "x",
            "2026-11-31",
            "2026/11/01",
        ] {
            assert_eq!(d(bad), None, "{bad}");
        }
        assert_eq!(d("2024-02-29"), Some(1_709_164_800_000));
        // The two directions agree over many years.
        for day in (0..30_000).step_by(37) {
            let ms = day * 86_400_000 + 12_345;
            assert_eq!(d(&super::utc_day(ms)), Some(day * 86_400_000));
        }
    }
}
