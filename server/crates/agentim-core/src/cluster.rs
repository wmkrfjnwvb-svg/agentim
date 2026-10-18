//! What the core needs from a cluster (docs/11-cluster.md). The
//! implementation lives in the `agentim-cluster` crate; without one the
//! server runs as a single node and nothing here is used.
//!
//! Each database's conversations fall into `shards()` virtual shards by a
//! hash of the conversation id; the control tables are one more shard
//! (`control`), and the database-wide upkeep (clean-up of dedup records,
//! device positions, webhook delivery) one more (`maintenance`). A node
//! writes a shard only while it holds the shard's lease, and every batch
//! checks the leases it relies on in the same transaction (§4.3).

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::logic::Op;
use crate::model::ApiResult;
use crate::registry::Out;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// A held lease: who (`node_id:boot_id`) and which generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub owner: String,
    pub epoch: i64,
}

/// Where a writer command must run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The holder of this conversation's shard.
    Conv(String),
    /// The holder of these shards (taking over several at once).
    Shards(Vec<u32>),
    /// The holder of the control shard (users, tokens, directory...).
    Control,
    /// The holder of the database's upkeep.
    Maintenance,
    /// Any node: monotonic or idempotent writes that no lease protects
    /// (acknowledged positions, last-seen times, audit records).
    Anywhere,
    /// This node only: its own in-memory state (ticks, barriers).
    Local,
}

pub trait Cluster: Send + Sync + 'static {
    /// Virtual shards per database.
    fn shards(&self) -> u32;
    /// The lease this node holds on a shard of database `db`, if any.
    fn held(&self, db: usize, shard: u32) -> Option<Lease>;
    /// The lease a batch already admitted may commit under: also while the
    /// shard is being handed over (no new commands then).
    fn writing(&self, db: usize, shard: u32) -> Option<Lease> {
        self.held(db, shard)
    }
    /// Runs a command on the node holding the shard.
    fn forward(&self, db: usize, shard: u32, op: Op) -> BoxFuture<ApiResult<Value>>;
    /// What a committed batch produced, for the other nodes' connections.
    fn publish(&self, db: usize, outs: &[Out]);
    /// The writer found it no longer holds a shard (its check failed).
    fn lost(&self, db: usize, shard: u32);
    /// The writer's connection failed or a batch got stuck: what it last
    /// wrote is unknown, so every shard of the database is given up and
    /// taken again with a new epoch and reloaded state (docs/11 §4.5).
    fn reset(&self, db: usize);
    /// Whether this node should take clients (Err: why not), for readiness.
    fn ready(&self) -> Result<(), String> {
        Ok(())
    }
}

pub fn control_shard(shards: u32) -> u32 {
    shards
}

pub fn maintenance_shard(shards: u32) -> u32 {
    shards + 1
}

/// The shard of a conversation: FNV-1a of its id, stable across versions
/// and platforms (a change would move every conversation).
pub fn shard_of(conv_id: &str, shards: u32) -> u32 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in conv_id.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (h % u64::from(shards.max(1))) as u32
}

/// The shard a route needs, if a lease protects it.
pub fn shard_for(route: &Route, shards: u32) -> Option<u32> {
    match route {
        Route::Conv(c) => Some(shard_of(c, shards)),
        Route::Control => Some(control_shard(shards)),
        Route::Maintenance => Some(maintenance_shard(shards)),
        Route::Shards(s) => s.first().copied(),
        Route::Anywhere | Route::Local => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shards_are_stable_and_spread() {
        // Fixed values: changing the hash would move every conversation.
        assert_eq!(shard_of("c_0", 256), shard_of("c_0", 256));
        assert_eq!(shard_of("", 256), (0xcbf2_9ce4_8422_2325u64 % 256) as u32);
        let mut counts = vec![0u32; 256];
        for i in 0..25_600 {
            counts[shard_of(&format!("c_{i:08}"), 256) as usize] += 1;
        }
        let (min, max) = (counts.iter().min().unwrap(), counts.iter().max().unwrap());
        assert!(*min > 50 && *max < 160, "{min}..{max}");
        assert_eq!(shard_of("anything", 1), 0);
        assert_eq!(shard_of("anything", 0), 0, "no division by zero");
    }

    #[test]
    fn routes_name_their_shard() {
        assert_eq!(shard_for(&Route::Control, 256), Some(256));
        assert_eq!(shard_for(&Route::Maintenance, 256), Some(257));
        assert_eq!(shard_for(&Route::Anywhere, 256), None);
        assert_eq!(shard_for(&Route::Local, 256), None);
        assert_eq!(shard_for(&Route::Shards(vec![3, 4]), 256), Some(3));
    }
}
