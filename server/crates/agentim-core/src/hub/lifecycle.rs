//! An owner's emergency stop, resume and destroy of an agent (docs/03-agents.md
//! §4.3–§4.4). The control database records the decision first (from then on
//! the agent can do nothing); each conversation the agent is in records it
//! after. A destroy interrupted by a crash is finished at the next start.

use serde_json::{Value, json};

use super::{Hub, err};
use crate::logic::Op;
use crate::model::*;

const NOTE_MAX: usize = 500;

impl Hub {
    /// Conversations the user is a member of (the control database's
    /// member index).
    pub(super) async fn convs_of(&self, user_id: &str) -> ApiResult<Vec<String>> {
        let ids = vec![user_id.to_string()];
        let pairs = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.member_index_for(&ids).await })
            })
            .await?;
        Ok(pairs.into_iter().map(|(c, _)| c).collect())
    }

    /// Runs a per-conversation op where the conversation lives, following
    /// it if it moves meanwhile.
    async fn on_conv_op(&self, conv_id: &str, op: impl Fn() -> Op) -> ApiResult<Value> {
        let mut attempt = 0;
        loop {
            let shard = self.conv_shard(conv_id)?;
            match shard.call(op()).await {
                Err(e)
                    if (e.code == "conv_moved" || e.code == "conv_migrating") && attempt < 20 =>
                {
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                res => return res,
            }
        }
    }

    /// The caller's own agent, not destroyed.
    pub(super) fn live_agent(&self, me: &User, agent_id: &str) -> ApiResult<User> {
        let a = self.owned_agent(me, agent_id)?;
        if self.inner.sh.destroyed.read().unwrap().contains(&a.id) {
            return Err(err("not_found", "the agent is gone"));
        }
        Ok(a)
    }

    pub fn is_stopped(&self, user_id: &str) -> bool {
        self.inner.sh.stopped.read().unwrap().contains(user_id)
    }

    pub fn is_destroyed(&self, user_id: &str) -> bool {
        self.inner.sh.destroyed.read().unwrap().contains(user_id)
    }

    pub async fn stop_agent(
        &self,
        me: &User,
        agent_id: &str,
        note: Option<String>,
    ) -> ApiResult<Value> {
        let agent = self.live_agent(me, agent_id)?;
        if note
            .as_ref()
            .is_some_and(|n| n.is_empty() || n.len() > NOTE_MAX || n.contains('\0'))
        {
            return Err(err("bad_request", "reason must be 1..500 bytes"));
        }
        self.stop_agent_by(agent, &me.id, note).await
    }

    /// Stops an agent on behalf of `by` (its owner, or the system when the
    /// owner is deactivated).
    pub(super) async fn stop_agent_by(
        &self,
        agent: User,
        by: &str,
        note: Option<String>,
    ) -> ApiResult<Value> {
        self.control()
            .call(Op::SetStopped {
                agent_id: agent.id.clone(),
                on: true,
                by: by.to_string(),
            })
            .await?;
        // From here every writer refuses the agent; its connections go.
        self.inner
            .sh
            .stopped
            .write()
            .unwrap()
            .insert(agent.id.clone());
        let kicked = self.inner.sh.reg.kick(&agent.id, None);
        let convs = self.convs_of(&agent.id).await?;
        for c in &convs {
            self.on_conv_op(c, || Op::AgentStopped {
                conv_id: c.clone(),
                agent: agent.clone(),
                on: true,
                by: by.to_string(),
                reason: note.clone(),
            })
            .await?;
        }
        Ok(
            json!({"agent_id": agent.id, "stopped": true, "conversations": convs.len(), "disconnected": kicked}),
        )
    }

    /// The person's live agents are stopped when the person is
    /// deactivated: their keys are in that person's hands (docs/03-agents.md
    /// §1.2). Reactivating does not resume them; the owner does.
    pub(super) async fn stop_agents_of(&self, owner_id: &str) -> ApiResult<()> {
        let owner = owner_id.to_string();
        let ids = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.agent_ids_of(&owner).await })
            })
            .await?;
        let mut agents = vec![];
        for id in ids {
            if let Some(a) = self.user_or_load(&id).await? {
                agents.push(a);
            }
        }
        for a in agents {
            // Team agents keep working; an administrator transfers them.
            if a.team || self.is_destroyed(&a.id) || self.is_stopped(&a.id) {
                continue;
            }
            match self
                .stop_agent_by(a, "system", Some("owner deactivated".into()))
                .await
            {
                Err(e) if e.code == "conflict" => {}
                r => {
                    r?;
                }
            }
        }
        Ok(())
    }

    /// Gives a team agent a new owner (an administrator's action, docs/03
    /// §9.3.1). With `rotate_keys` its keys are revoked, so copies the
    /// previous owner kept stop working; the new owner issues new ones.
    pub async fn transfer_agent(
        &self,
        agent_id: &str,
        new_owner: &str,
        rotate_keys: bool,
    ) -> ApiResult<Value> {
        let _one = self.inner.transfer_lock.lock().await;
        let a = self
            .user_or_load(agent_id)
            .await?
            .filter(|u| u.kind == UserKind::Agent && !self.is_destroyed(&u.id))
            .ok_or_else(|| err("not_found", "agent"))?;
        if !a.team {
            return Err(err(
                "bad_request",
                "only team agents change owner; a person's assistant stays theirs",
            ));
        }
        let o = self
            .user_or_load(new_owner)
            .await?
            .filter(|u| u.kind == UserKind::Human && !self.is_deactivated(&u.id))
            .ok_or_else(|| err("not_found", "no such person"))?;
        if o.unit_id != a.unit_id {
            return Err(err(
                "bad_request",
                "the new owner must be in the agent's unit",
            ));
        }
        if a.owner_id.as_deref() == Some(o.id.as_str()) {
            return Err(err("conflict", "the agent already belongs to them"));
        }
        let v = self
            .control()
            .call(Op::TransferAgent {
                agent_id: a.id.clone(),
                owner_id: o.id.clone(),
                owner_cap: self.inner.cfg.agents.max_per_owner,
            })
            .await?;
        let mut moved = a.clone();
        moved.owner_id = Some(o.id.clone());
        self.inner
            .sh
            .users
            .write()
            .unwrap()
            .insert(moved.id.clone(), moved);
        for s in &self.inner.shards {
            s.call(Op::SetMemberOwner {
                agent_id: a.id.clone(),
                owner_id: o.id.clone(),
            })
            .await?;
        }
        if rotate_keys {
            self.revoke_tokens(&a.id, None).await?;
        }
        Ok(json!({"agent_id": a.id, "from": v["from"], "to": o.id, "keys_revoked": rotate_keys}))
    }

    pub async fn resume_agent(&self, me: &User, agent_id: &str) -> ApiResult<Value> {
        let agent = self.live_agent(me, agent_id)?;
        self.control()
            .call(Op::SetStopped {
                agent_id: agent.id.clone(),
                on: false,
                by: me.id.clone(),
            })
            .await?;
        self.inner.sh.stopped.write().unwrap().remove(&agent.id);
        let convs = self.convs_of(&agent.id).await?;
        for c in &convs {
            self.on_conv_op(c, || Op::AgentStopped {
                conv_id: c.clone(),
                agent: agent.clone(),
                on: false,
                by: me.id.clone(),
                reason: None,
            })
            .await?;
        }
        Ok(json!({"agent_id": agent.id, "stopped": false, "conversations": convs.len()}))
    }

    pub async fn destroy_agent(
        &self,
        me: &User,
        agent_id: &str,
        confirm: &str,
    ) -> ApiResult<Value> {
        let agent = self.live_agent(me, agent_id)?;
        if confirm != agent.name {
            return Err(err(
                "bad_request",
                "confirm with the agent's name: destroying cannot be undone",
            ));
        }
        self.control()
            .call(Op::Destroy {
                agent_id: agent.id.clone(),
                by: me.id.clone(),
            })
            .await?;
        {
            let sh = &self.inner.sh;
            sh.destroyed.write().unwrap().insert(agent.id.clone());
            sh.deactivated.write().unwrap().insert(agent.id.clone());
            sh.stopped.write().unwrap().remove(&agent.id);
        }
        self.revoke_tokens(&agent.id, None).await?;
        let n = self.remove_destroyed(&agent, &me.id).await?;
        Ok(json!({"agent_id": agent.id, "destroyed": true, "conversations": n}))
    }

    /// Takes a destroyed agent out of every conversation it is still in.
    async fn remove_destroyed(&self, agent: &User, by: &str) -> ApiResult<usize> {
        let convs = self.convs_of(&agent.id).await?;
        for c in &convs {
            let shard_idx = self.conv_shard(c).map(|s| s.idx);
            match self
                .on_conv_op(c, || Op::RemoveDestroyed {
                    conv_id: c.clone(),
                    agent: agent.clone(),
                    by: by.to_string(),
                })
                .await
            {
                Ok(_) => {}
                // Gone meanwhile (dissolved): nothing left to remove.
                Err(e) if e.code == "not_found" => {}
                Err(e) => return Err(e),
            }
            if shard_idx.ok() != Some(self.inner.sh.control) {
                self.control()
                    .call(Op::MemberIndexRemove {
                        conv_id: c.clone(),
                        user_id: agent.id.clone(),
                    })
                    .await?;
            }
        }
        Ok(convs.len())
    }

    /// Loads which agents are stopped or destroyed, and finishes destroys
    /// a crash interrupted.
    pub(super) async fn load_agent_flags(&self) -> ApiResult<()> {
        let flags = self
            .read(self.control(), |db| {
                Box::pin(async move { db.agent_flags().await })
            })
            .await?;
        let sh = &self.inner.sh;
        for (id, stopped, destroyed) in &flags {
            if *stopped {
                sh.stopped.write().unwrap().insert(id.clone());
            }
            if *destroyed {
                sh.destroyed.write().unwrap().insert(id.clone());
            }
        }
        for (id, _, destroyed) in flags {
            if !destroyed {
                continue;
            }
            let Some(agent) = sh.user(&id) else { continue };
            let by = agent.owner_id.clone().unwrap_or_default();
            self.remove_destroyed(&agent, &by).await?;
        }
        Ok(())
    }
}

impl Hub {
    /// Feedback people gave an agent's messages (docs/03-agents.md §3): totals
    /// and the latest entries, for its owner.
    pub(super) async fn list_feedback(
        &self,
        me: &User,
        agent_id: &str,
        limit: Option<i64>,
    ) -> ApiResult<Value> {
        let agent = self.live_agent(me, agent_id)?;
        let limit = limit.unwrap_or(20);
        if !(1..=100).contains(&limit) {
            return Err(err("bad_request", "limit must be 1..100"));
        }
        let (mut up, mut down, mut items) = (0, 0, vec![]);
        for s in &self.inner.shards {
            let a = agent.id.clone();
            let (u, d, it) = self
                .read(s, move |db| {
                    Box::pin(async move { db.feedback_of(&a, limit).await })
                })
                .await?;
            up += u;
            down += d;
            items.extend(it);
        }
        items.sort_by(|a, b| b["created_at"].as_i64().cmp(&a["created_at"].as_i64()));
        items.truncate(limit as usize);
        Ok(json!({"agent_id": agent.id, "up": up, "down": down, "items": items}))
    }
}
