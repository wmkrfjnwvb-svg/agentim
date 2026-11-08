//! Friends (docs/en/design/agents.md): two-way, by request and answer; who may add a
//! user is its `friend_policy` (people: `confirm` by default; the app's
//! team agents: `anyone`, so adding one is adding a friend; a person's own
//! assistant: only its owner). Blocking ends a friendship.

use serde_json::{Value, json};

use super::Hub;
use crate::logic::{FriendOp, Op};
use crate::model::*;

/// Requests one person may have waiting at once.
pub const FRIEND_REQUESTS_MAX: usize = 100;
/// Longest message with a request.
pub const FRIEND_MESSAGE_MAX: usize = 500;

pub const POLICIES: &[&str] = &["anyone", "confirm", "nobody"];

impl Hub {
    fn friend_target(&self, me: &User, id: &str) -> ApiResult<User> {
        let sh = &self.inner.sh;
        let t = sh
            .user(id)
            .filter(|u| u.kind != UserKind::System && !self.is_destroyed(&u.id))
            .ok_or_else(|| rule!("account.unknown_user", "not_found", "user"))?;
        if t.id == me.id {
            return Err(rule!(
                "friend.self",
                "bad_request",
                "you cannot be your own friend"
            ));
        }
        Ok(t)
    }

    fn people_only(me: &User) -> ApiResult<()> {
        if me.kind != UserKind::Human {
            return Err(rule!(
                "friend.people_only",
                "forbidden",
                "friends are for people; agents are added as friends"
            ));
        }
        Ok(())
    }

    /// Who may add `t` when it set nothing itself.
    fn default_friend_policy(&self, me: &User, t: &User) -> &'static str {
        match t.kind {
            UserKind::Agent if t.team => "anyone",
            UserKind::Agent if t.owner_id.as_deref() == Some(me.id.as_str()) => "anyone",
            UserKind::Agent => "nobody",
            UserKind::Human | UserKind::System => "confirm",
        }
    }

    pub async fn friend_request(&self, me: &User, id: &str, message: &str) -> ApiResult<Value> {
        Self::people_only(me)?;
        let t = self.friend_target(me, id)?;
        if message.chars().count() > FRIEND_MESSAGE_MAX
            || message.chars().any(|c| c.is_control() && c != '\n')
        {
            return Err(rule!(
                "friend.message",
                "bad_request",
                format!("a request's message is at most {FRIEND_MESSAGE_MAX} characters")
            ));
        }
        if self.is_deactivated(&t.id) {
            return Err(rule!("account.unknown_user", "not_found", "user"));
        }
        let default_policy = self.default_friend_policy(me, &t).to_string();
        self.control()
            .call(Op::Friend(FriendOp::Request {
                from: me.id.clone(),
                to: t.id,
                message: message.to_string(),
                default_policy,
                now: now_ms(),
            }))
            .await
    }

    pub async fn friend_answer(&self, me: &User, id: &str, accept: bool) -> ApiResult<Value> {
        Self::people_only(me)?;
        let t = self.friend_target(me, id)?;
        self.control()
            .call(Op::Friend(FriendOp::Answer {
                me: me.id.clone(),
                from: t.id,
                accept,
                now: now_ms(),
            }))
            .await
    }

    /// Ends a friendship, or withdraws my waiting request.
    pub async fn friend_remove(&self, me: &User, id: &str) -> ApiResult<Value> {
        Self::people_only(me)?;
        let t = self.friend_target(me, id)?;
        self.control()
            .call(Op::Friend(FriendOp::Remove {
                me: me.id.clone(),
                other: t.id,
            }))
            .await
    }

    pub async fn set_friend_policy(&self, me: &User, policy: &str) -> ApiResult<Value> {
        Self::people_only(me)?;
        if !POLICIES.contains(&policy) {
            return Err(rule!(
                "friend.policy",
                "bad_request",
                "friend_policy is anyone, confirm or nobody"
            ));
        }
        self.control()
            .call(Op::Friend(FriendOp::Policy {
                user: me.id.clone(),
                policy: policy.to_string(),
            }))
            .await
    }

    /// The developer's backend sets (or ends) a friendship directly.
    pub async fn set_friends(&self, a: &User, b: &User, on: bool) -> ApiResult<Value> {
        if a.id == b.id {
            return Err(rule!(
                "friend.self",
                "bad_request",
                "you cannot be your own friend"
            ));
        }
        self.control()
            .call(Op::Friend(FriendOp::Set {
                a: a.id.clone(),
                b: b.id.clone(),
                on,
                now: now_ms(),
            }))
            .await
    }

    fn friend_view(&self, id: &str, at: i64, message: Option<String>) -> Value {
        let u = self.inner.sh.user(id);
        let mut v = json!({
            "user_id": id,
            "name": u.as_ref().map(|u| u.name.clone()),
            "kind": u.as_ref().map(|u| u.kind),
            "at": at,
        });
        if let Some(m) = message {
            v["message"] = json!(m);
        }
        v
    }

    /// Friends and the requests waiting both ways.
    pub async fn list_friends(&self, me: &User) -> ApiResult<Value> {
        let id = me.id.clone();
        let (friends, incoming, outgoing, policy) = self
            .read(self.control(), move |db| {
                Box::pin(async move {
                    Ok((
                        db.friends_of(&id).await?,
                        db.friend_requests_of(&id, true).await?,
                        db.friend_requests_of(&id, false).await?,
                        db.friend_policy(&id).await?,
                    ))
                })
            })
            .await?;
        Ok(json!({
            "friends": friends.into_iter().map(|(u, at)| self.friend_view(&u, at, None)).collect::<Vec<_>>(),
            "incoming": incoming.into_iter().map(|(u, m, at)| self.friend_view(&u, at, Some(m))).collect::<Vec<_>>(),
            "outgoing": outgoing.into_iter().map(|(u, m, at)| self.friend_view(&u, at, Some(m))).collect::<Vec<_>>(),
            "friend_policy": policy.unwrap_or_else(|| "confirm".into()),
        }))
    }

    pub async fn are_friends(&self, a: &str, b: &str) -> ApiResult<bool> {
        let (a, b) = (a.to_string(), b.to_string());
        self.read(self.control(), move |db| {
            Box::pin(async move { db.are_friends(&a, &b).await })
        })
        .await
    }

    /// The app's agents people can add (team agents that run): the "agent
    /// square" of a client.
    pub async fn list_app_agents(&self) -> ApiResult<Value> {
        let users: Vec<User> = self
            .inner
            .sh
            .users
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|u| {
                u.kind == UserKind::Agent
                    && u.team
                    && !self.is_destroyed(&u.id)
                    && !self.is_stopped(&u.id)
                    && !self.is_deactivated(&u.id)
            })
            .cloned()
            .collect();
        let roles = self.agent_roles().await?;
        let mut out = vec![];
        for u in users {
            let mut v = json!({"user_id": u.id, "name": u.name});
            if let Some(r) = roles.get(&u.id) {
                v["role"] = json!(r);
                if let Ok(role) = self.role(r).await {
                    v["description"] = role["description"].clone();
                    v["tags"] = role["tags"].clone();
                }
            }
            out.push(v);
        }
        out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Ok(json!({"agents": out}))
    }
}
