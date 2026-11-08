//! Friends (docs/en/design/agents.md): a pair is friends when its row exists (the
//! smaller id in `a`); a request waits in `friend_requests` until answered.

use sqlx::Row;

use super::Db;
use crate::model::*;

/// The pair's key: the smaller id first.
pub fn pair<'a>(x: &'a str, y: &'a str) -> (&'a str, &'a str) {
    if x <= y { (x, y) } else { (y, x) }
}

impl Db<'_> {
    pub async fn are_friends(&mut self, x: &str, y: &str) -> ApiResult<bool> {
        let (a, b) = pair(x, y);
        let r = sqlx::query(self.d.q("SELECT 1 FROM friends WHERE a = ? AND b = ?"))
            .bind(a)
            .bind(b)
            .fetch_optional(&mut *self.c)
            .await?;
        Ok(r.is_some())
    }

    pub async fn friend_add(&mut self, x: &str, y: &str, now: i64) -> ApiResult<()> {
        let (a, b) = pair(x, y);
        sqlx::query(
            self.d
                .q("INSERT INTO friends (a, b, created_at) VALUES (?, ?, ?)"),
        )
        .bind(a)
        .bind(b)
        .bind(now)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// True when they were friends.
    pub async fn friend_delete(&mut self, x: &str, y: &str) -> ApiResult<bool> {
        let (a, b) = pair(x, y);
        let n = sqlx::query(self.d.q("DELETE FROM friends WHERE a = ? AND b = ?"))
            .bind(a)
            .bind(b)
            .execute(&mut *self.c)
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// A user's friends: (id, since).
    pub async fn friends_of(&mut self, id: &str) -> ApiResult<Vec<(String, i64)>> {
        let rows = sqlx::query(self.d.q(
            "SELECT b, created_at FROM friends WHERE a = ? UNION ALL SELECT a, created_at FROM friends WHERE b = ?",
        ))
        .bind(id)
        .bind(id)
        .fetch_all(&mut *self.c)
        .await?;
        let mut v: Vec<(String, i64)> = rows
            .iter()
            .map(|r| Ok((r.try_get::<String, _>(0)?, r.try_get::<i64, _>(1)?)))
            .collect::<sqlx::Result<_>>()?;
        v.sort();
        Ok(v)
    }

    /// The request from `from` to `to`, if waiting: (message, at).
    pub async fn friend_request_get(
        &mut self,
        from: &str,
        to: &str,
    ) -> ApiResult<Option<(String, i64)>> {
        let r =
            sqlx::query(self.d.q(
                "SELECT message, created_at FROM friend_requests WHERE from_id = ? AND to_id = ?",
            ))
            .bind(from)
            .bind(to)
            .fetch_optional(&mut *self.c)
            .await?;
        Ok(match r {
            Some(r) => Some((r.try_get(0)?, r.try_get(1)?)),
            None => None,
        })
    }

    pub async fn friend_request_put(
        &mut self,
        from: &str,
        to: &str,
        message: &str,
        now: i64,
    ) -> ApiResult<()> {
        sqlx::query(self.d.q(
            "INSERT INTO friend_requests (from_id, to_id, message, created_at) VALUES (?, ?, ?, ?)",
        ))
        .bind(from)
        .bind(to)
        .bind(message)
        .bind(now)
        .execute(&mut *self.c)
        .await?;
        Ok(())
    }

    /// Removes the requests between the two, both ways; how many.
    pub async fn friend_requests_clear(&mut self, x: &str, y: &str) -> ApiResult<u64> {
        let n = sqlx::query(self.d.q(
            "DELETE FROM friend_requests WHERE (from_id = ? AND to_id = ?) OR (from_id = ? AND to_id = ?)",
        ))
        .bind(x)
        .bind(y)
        .bind(y)
        .bind(x)
        .execute(&mut *self.c)
        .await?
        .rows_affected();
        Ok(n)
    }

    /// Requests sent by `id` (outgoing) or to it: (other, message, at).
    pub async fn friend_requests_of(
        &mut self,
        id: &str,
        incoming: bool,
    ) -> ApiResult<Vec<(String, String, i64)>> {
        let q = if incoming {
            "SELECT from_id, message, created_at FROM friend_requests WHERE to_id = ? ORDER BY created_at, from_id"
        } else {
            "SELECT to_id, message, created_at FROM friend_requests WHERE from_id = ? ORDER BY created_at, to_id"
        };
        let rows = sqlx::query(self.d.q(q))
            .bind(id)
            .fetch_all(&mut *self.c)
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?, r.try_get(2)?)))
            .collect::<sqlx::Result<_>>()
            .map_err(Into::into)
    }

    pub async fn friend_policy(&mut self, id: &str) -> ApiResult<Option<String>> {
        let r = sqlx::query(self.d.q("SELECT friend_policy FROM users WHERE id = ?"))
            .bind(id)
            .fetch_optional(&mut *self.c)
            .await?;
        Ok(match r {
            Some(r) => r.try_get::<Option<String>, _>(0)?,
            None => None,
        })
    }

    pub async fn set_friend_policy(&mut self, id: &str, policy: &str) -> ApiResult<()> {
        sqlx::query(self.d.q("UPDATE users SET friend_policy = ? WHERE id = ?"))
            .bind(policy)
            .bind(id)
            .execute(&mut *self.c)
            .await?;
        Ok(())
    }
}
