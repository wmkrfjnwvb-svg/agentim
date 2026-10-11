//! An owner's emergency stop, resume and destroy of an agent (docs/09
//! §11.3). The control database records the decision first (from then on
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
    async fn convs_of(&self, user_id: &str) -> ApiResult<Vec<String>> {
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
        self.control()
            .call(Op::SetStopped {
                agent_id: agent.id.clone(),
                on: true,
                by: me.id.clone(),
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
                by: me.id.clone(),
                reason: note.clone(),
            })
            .await?;
        }
        Ok(
            json!({"agent_id": agent.id, "stopped": true, "conversations": convs.len(), "disconnected": kicked}),
        )
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
