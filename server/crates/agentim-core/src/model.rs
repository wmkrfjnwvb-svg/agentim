//! Wire protocol and shared data types.
//!
//! Every client frame is a JSON object `{"op": "...", "rid": <optional u64>, ...}`.
//! Server frames are JSON objects with a `type` field. Responses to a request
//! echo its `rid`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const TENANT_DEFAULT: &str = "default";
/// The app's system identity (open API `"as": "system"`, docs/en/design/open-api.md).
pub const SYSTEM_ID: &str = "u_system";
pub const SYSTEM_NAME: &str = "_system";

/// Stands in for the system user where a membership is required: it may
/// manage any conversation of the app without being a member.
pub fn system_member() -> Member {
    Member {
        user_id: SYSTEM_ID.into(),
        name: SYSTEM_NAME.into(),
        kind: UserKind::System,
        owner_id: None,
        role: "owner".into(),
        trigger: Trigger::Mention,
        approval: false,
        approval_rules: None,
        required_approval: None,
        join_seq: 0,
        visible_from: 1,
        mute_mode: "none".into(),
        mute_until: None,
        topics: vec![],
        silenced_until: None,
    }
}
pub const UNIT_DEFAULT: &str = "default";

/// What one person may do in their client, over the app's defaults
/// (`[clients.allow]`, docs/en/design/access.md); None follows the default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_groups: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_direct: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_members: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub find_users: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_agents: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recall_minutes: Option<i64>,
}

impl Overrides {
    pub fn is_empty(&self) -> bool {
        *self == Overrides::default()
    }

    /// The person's permissions: these overrides over the defaults.
    pub fn over(&self, d: &crate::config::AllowConfig) -> crate::config::AllowConfig {
        crate::config::AllowConfig {
            create_groups: self.create_groups.unwrap_or(d.create_groups),
            create_direct: self.create_direct.unwrap_or(d.create_direct),
            add_members: self.add_members.unwrap_or(d.add_members),
            find_users: self.find_users.unwrap_or(d.find_users),
            create_agents: self.create_agents.unwrap_or(d.create_agents),
            edit_minutes: self.edit_minutes.or(d.edit_minutes),
            recall_minutes: self.recall_minutes.or(d.recall_minutes),
            direct_to_non_friends: d.direct_to_non_friends,
        }
    }

    /// Applies a change: each key set to a value overrides, set to null
    /// follows the default again; `null` for the whole object clears all.
    pub fn apply(&self, change: &serde_json::Value) -> Result<Overrides, String> {
        if change.is_null() {
            return Ok(Overrides::default());
        }
        let obj = change
            .as_object()
            .ok_or("permissions is an object or null")?;
        let mut cur = serde_json::to_value(self).map_err(|e| e.to_string())?;
        for (k, v) in obj {
            if v.is_null() {
                cur.as_object_mut().unwrap().remove(k);
            } else {
                cur[k] = v.clone();
            }
        }
        let o: Overrides = serde_json::from_value(cur).map_err(|e| {
            format!("permissions: {e} (keys: create_groups, create_direct, add_members, find_users, create_agents as booleans; edit_minutes, recall_minutes as 0..525600)")
        })?;
        if [o.edit_minutes, o.recall_minutes]
            .iter()
            .any(|m| m.is_some_and(|m| !(0..=525_600).contains(&m)))
        {
            return Err("edit_minutes and recall_minutes are 0..525600".into());
        }
        Ok(o)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserKind {
    Human,
    Agent,
    System,
}

impl UserKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UserKind::Human => "human",
            UserKind::Agent => "agent",
            UserKind::System => "system",
        }
    }
    pub fn parse(s: &str) -> UserKind {
        match s {
            "agent" => UserKind::Agent,
            "system" => UserKind::System,
            _ => UserKind::Human,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub name: String,
    pub kind: UserKind,
    pub owner_id: Option<String>,
    /// Organisation unit; decides which database new conversations go to.
    pub unit_id: String,
    /// A team agent (belongs to a group or the organisation; its owner only
    /// maintains it) rather than a person's assistant (docs/en/design/agents.md).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub team: bool,
}

/// How an agent member is triggered in a conversation, which also bounds what
/// it can read (see docs/en/design/agents.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// Receives (and is woken by) every visible event.
    All,
    /// Receives only events that mention it, reply to it, or are its own.
    Mention,
    /// `mention` plus messages whose envelope names it in `to` (the default
    /// for agents in groups, docs/en/design/agents.md).
    Addressed,
    /// `addressed` plus messages on a topic it subscribes to.
    Topics,
    /// Receives everything, is never woken (monitoring, audit, recording).
    Observe,
    /// Receives everything, woken only when given the floor; speaks only
    /// while holding it (docs/en/design/agents.md).
    Coordinated,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::All => "all",
            Trigger::Mention => "mention",
            Trigger::Addressed => "addressed",
            Trigger::Topics => "topics",
            Trigger::Observe => "observe",
            Trigger::Coordinated => "coordinated",
        }
    }
    pub fn parse(s: &str) -> Trigger {
        match s {
            "all" => Trigger::All,
            "addressed" => Trigger::Addressed,
            "topics" => Trigger::Topics,
            "observe" => Trigger::Observe,
            "coordinated" => Trigger::Coordinated,
            _ => Trigger::Mention,
        }
    }
}

/// Approval rules of an agent in a conversation (docs/en/design/agents.md).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Approval {
    /// `off`, `all` or `rules`.
    pub mode: String,
    /// `files`, `links`, `mentions`, `cards`, `first_message`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    /// A message with one of these topics needs approval.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,
    /// `owner`, `managers` or user ids (names are resolved when set).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approvers: Vec<String>,
    /// An agent that reviews first (approve or escalate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    /// After the reviewer approves, a person still has to.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub human_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_secs: Option<i64>,
}

impl Approval {
    pub fn all() -> Approval {
        Approval {
            mode: "all".into(),
            ..Default::default()
        }
    }
    pub fn on(&self) -> bool {
        self.mode != "off"
    }
}

/// `approval` in `set_agent_policy`: the old switch or rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ApprovalArg {
    Switch(bool),
    Rules(Box<Approval>),
}

impl ApprovalArg {
    pub fn rules(self) -> Approval {
        match self {
            ApprovalArg::Switch(true) => Approval::all(),
            ApprovalArg::Switch(false) => Approval {
                mode: "off".into(),
                ..Default::default()
            },
            ApprovalArg::Rules(a) => *a,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub user_id: String,
    pub name: String,
    pub kind: UserKind,
    pub owner_id: Option<String>,
    pub role: String,
    pub trigger: Trigger,
    /// Whether any approval rule is on (either layer): kept for clients.
    pub approval: bool,
    /// The approval rules its owner set (docs/en/design/agents.md); `None` with
    /// `approval` set is the old switch, every message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_rules: Option<Approval>,
    /// What the conversation's managers require of this agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_approval: Option<Approval>,
    pub join_seq: i64,
    /// First event this member may see (docs/en/design/sync-and-clients.md).
    pub visible_from: i64,
    /// `none`, `all` (no notifications) or `mentions` (only when mentioned).
    pub mute_mode: String,
    /// Mute ends at this time (ms); None = until changed.
    pub mute_until: Option<i64>,
    /// Topics an agent in `topics` mode subscribes to.
    pub topics: Vec<String>,
    /// Silenced by moderation in this conversation until this time (ms):
    /// cannot send, edit, stream or signal.
    pub silenced_until: Option<i64>,
}

impl Member {
    /// Whether this member may see an event at all: not before they could
    /// see the log, and not about a message from before then (an edit or
    /// recall of an older message would otherwise reveal it).
    pub fn sees(&self, ev: &Event) -> bool {
        ev.seq >= self.visible_from
            && ev
                .msg
                .as_ref()
                .is_none_or(|m| m.created_seq >= self.visible_from)
            // Reactions and pins name the message they are about (MSG-20, MSG-21).
            && ev.data["msg_seq"]
                .as_i64()
                .is_none_or(|s| s >= self.visible_from)
    }

    /// Whether an event should notify this member (docs/en/design/sync-and-clients.md): never for
    /// their own messages, nor while muted (`mentions` still notifies when
    /// they are mentioned).
    pub fn notified_by(&self, msg: &MsgView, now: i64) -> bool {
        if msg.sender_id == self.user_id {
            return false;
        }
        let muted = self.mute_until.is_none_or(|u| u > now);
        match self.mute_mode.as_str() {
            "all" if muted => false,
            "mentions" if muted => msg.mentions.contains(&self.user_id),
            _ => true,
        }
    }
}

/// Snapshot of a message at a given version. Embedded in every `msg.*` event so
/// that a pulled event range is self-contained.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MsgView {
    pub msg_id: String,
    /// The sender's own id for the message, shown only to the sender (to
    /// merge a message it sent with the copy sync brings back, M17).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_msg_id: Option<String>,
    pub conv_id: String,
    pub created_seq: i64,
    pub sender_id: String,
    pub sender_name: String,
    /// Written by the server from the authenticated identity; cannot be forged.
    pub sender_type: UserKind,
    pub on_behalf_of: Option<String>,
    /// `text` or a namespaced custom kind (docs/en/design/messaging.md);
    /// `content` is then the plain-text fallback.
    pub kind: String,
    pub content: String,
    /// Structured content of a custom kind (a JSON object).
    #[serde(default)]
    pub payload: Option<Value>,
    pub version: i64,
    /// committed | streaming | complete | cancelled | failed | recalled
    pub state: String,
    pub state_reason: Option<String>,
    pub hop: i64,
    pub causation_id: Option<String>,
    pub reply_to: Option<String>,
    pub mentions: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Routing envelope: `to`, `topics`, `intent`, `refs` (docs/en/design/agents.md).
    #[serde(default)]
    pub envelope: Option<Value>,
    /// The message that started the chain this one belongs to (root
    /// budget); None for messages from before v3.
    #[serde(default)]
    pub root_id: Option<String>,
    /// The thread (its root message) this reply belongs to
    /// (docs/en/design/messaging.md, MSG-19).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// A thread reply that also shows in the conversation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub thread_shown: bool,
}

/// Where a message goes in a thread: the root's id, and whether it also
/// shows in the conversation (docs/en/design/messaging.md, MSG-19).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRef {
    pub id: String,
    #[serde(default)]
    pub shown: bool,
}

/// One entry of the main conversation log. `seq` is gap-free per conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub conv_id: String,
    pub seq: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub actor_id: Option<String>,
    pub msg: Option<MsgView>,
    pub data: Value,
    pub created_at: i64,
}

impl Event {
    /// The event as `reader` receives it: the sender's `client_msg_id` is
    /// for the sender only.
    pub fn for_reader(&self, reader: &str) -> std::borrow::Cow<'_, Event> {
        match &self.msg {
            Some(m) if m.client_msg_id.is_some() && m.sender_id != reader => {
                let mut e = self.clone();
                if let Some(m) = e.msg.as_mut() {
                    m.client_msg_id = None;
                }
                std::borrow::Cow::Owned(e)
            }
            _ => std::borrow::Cow::Borrowed(self),
        }
    }
}

/// One entry of a conversation's side channel: content with targeted
/// visibility (whispers, ephemeral messages, approval drafts, guard notices).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SideEvent {
    pub conv_id: String,
    pub side_seq: i64,
    /// Latest main-log seq when this was produced; used to merge both logs.
    pub anchor_seq: i64,
    pub audience: Vec<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub actor_id: Option<String>,
    pub data: Value,
    pub created_at: i64,
}

/// Narrows `search` (docs/en/design/agents.md); every field optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
    /// `human` or `agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_type: Option<String>,
    /// Message kind, e.g. `text`, `card`, a custom type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Milliseconds since 1970, inclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// Only messages that cite others (`refs`), or only those that do not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_refs: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Envelope {
    pub rid: Option<u64>,
    /// The caller's W3C `traceparent` (docs/en/design/tracing.md).
    #[serde(default)]
    pub trace: Option<String>,
    #[serde(flatten)]
    pub msg: ClientMsg,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Handshake (docs/en/design/messaging.md). Every field is optional so
    /// clients that predate negotiation keep working.
    Hello {
        #[serde(default)]
        protocol: Option<Vec<u32>>,
        #[serde(default)]
        client: Option<crate::protocol::ClientInfo>,
        #[serde(default)]
        features: Option<Vec<String>>,
    },
    /// The caller's conversations with their last message, unread count,
    /// members and mute state.
    ListConvs,
    /// Creates a conversation: `dm` (exactly one other member), `group` or
    /// `task`; the caller is its owner.
    CreateConv {
        kind: String,
        title: String,
        members: Vec<String>,
        /// Task channels: the member responsible for the task.
        #[serde(default)]
        assignee: Option<String>,
    },
    /// Owner or admin adds a member (an agent owner may add their agent where
    /// allowed).
    AddMember {
        conv_id: String,
        user_id: String,
        /// Earlier messages the new member may see (docs/en/design/sync-and-clients.md); default
        /// from the server configuration.
        #[serde(default)]
        history: Option<i64>,
    },
    /// Owner / admin removes a member; anyone removes an agent they own.
    RemoveMember { conv_id: String, user_id: String },
    /// The caller leaves a group; the owner transfers ownership or dissolves
    /// instead.
    Leave { conv_id: String },
    /// Owner only: `admin` or `member`.
    SetRole {
        conv_id: String,
        user_id: String,
        role: String,
    },
    /// The owner makes another member (a person) the owner; the old owner
    /// becomes an admin.
    TransferOwner { conv_id: String, user_id: String },
    /// Owner only: everyone loses access and the content is deleted.
    Dissolve { conv_id: String },
    /// The caller blocks a user: no direct messages, invitations or new
    /// conversations from them.
    Block { user_id: String },
    /// Lifts a block.
    Unblock { user_id: String },
    /// Users the caller has blocked.
    ListBlocks,
    /// Exact user name; only users the caller may discover are returned.
    FindUser { name: String },
    /// Sends a message. `client_msg_id` makes retries safe: the same id returns
    /// the first result.
    Send {
        conv_id: String,
        client_msg_id: String,
        /// For a custom kind: the plain-text fallback.
        text: String,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        payload: Option<Value>,
        #[serde(default)]
        reply_to: Option<String>,
        #[serde(default)]
        causation_id: Option<String>,
        #[serde(default)]
        on_behalf_of: Option<String>,
        /// Ephemeral: visible only to this user (plus the sender and, for an
        /// agent, its owner). Goes to the side channel, takes no conv_seq.
        #[serde(default)]
        to_user: Option<String>,
        /// Routing envelope (docs/en/design/agents.md): members named, topics, intent,
        /// references.
        #[serde(default)]
        to: Option<Vec<String>>,
        #[serde(default)]
        topics: Option<Vec<String>>,
        #[serde(default)]
        intent: Option<String>,
        #[serde(default)]
        refs: Option<Vec<String>>,
        /// Sources an answer is based on (docs/en/design/agents.md).
        #[serde(default)]
        sources: Option<Vec<Value>>,
        /// Replies in the thread of this root message (MSG-19).
        #[serde(default)]
        thread_id: Option<String>,
        /// A thread reply that also shows in the conversation.
        #[serde(default)]
        also_in_conv: bool,
    },
    /// Edits a message: the sender (or the sending agent's owner), within the
    /// app's edit window.
    Edit {
        msg_id: String,
        text: String,
        /// Custom kinds only: replaces the payload when given.
        #[serde(default)]
        payload: Option<Value>,
        /// The version the edit is based on: refused with `conflict` when
        /// the message has changed since.
        #[serde(default)]
        base_version: Option<i64>,
    },
    /// Ephemeral event to the online members: no storage, no seq.
    Signal {
        conv_id: String,
        kind: String,
        #[serde(default)]
        data: Option<Value>,
    },
    /// Recalls (deletes for everyone) a message: the sender, the agent's owner,
    /// an owner or admin.
    Recall { msg_id: String },
    /// Agents: events of a conversation after a seq (people use `sync`).
    Pull {
        conv_id: String,
        #[serde(default)]
        after_seq: i64,
        #[serde(default)]
        limit: Option<i64>,
        /// Owner observing through one of their agents.
        #[serde(default)]
        as_agent: Option<String>,
    },
    /// Side-channel events (whispers, progress, ephemeral messages) after a
    /// side seq.
    PullSide {
        conv_id: String,
        #[serde(default)]
        after_side_seq: i64,
    },
    /// Marks the conversation read up to `seq` on all of the caller's devices.
    Read { conv_id: String, seq: i64 },
    /// Receive what this device has not acknowledged yet (docs/en/design/sync-and-clients.md).
    Sync {
        #[serde(default)]
        limit: Option<i64>,
    },
    /// Positions this device has stored durably.
    Ack { acks: Vec<AckItem> },
    /// `none`, `all` or `mentions`, optionally until a time (ms).
    SetMute {
        conv_id: String,
        mode: String,
        #[serde(default)]
        until: Option<i64>,
    },
    /// Unread counts of the caller's conversations.
    Unread,
    /// The latest seq of each conversation (a client checks its position;
    /// a tail push lost on the way shows up here, docs/en/design/cluster.md).
    Heads { conv_ids: Vec<String> },
    /// The caller's preferences: whether reading on one device silences the
    /// others.
    SetPrefs {
        #[serde(default)]
        read_silences_others: Option<bool>,
    },
    /// An owner reads a conversation as one of their agents sees it (audited).
    Observe {
        conv_id: String,
        agent_id: String,
        #[serde(default)]
        after_seq: i64,
        #[serde(default)]
        limit: Option<i64>,
    },
    /// The conversation open on screen: its streaming replies arrive as live
    /// deltas.
    Focus {
        conv_id: Option<String>,
        #[serde(default)]
        as_agent: Option<String>,
    },
    /// An agent starts a streamed reply; the message shows at once as
    /// streaming.
    StreamStart {
        conv_id: String,
        client_msg_id: String,
        #[serde(default)]
        reply_to: Option<String>,
        #[serde(default)]
        causation_id: Option<String>,
        /// Streams the reply into this thread (MSG-19).
        #[serde(default)]
        thread_id: Option<String>,
        #[serde(default)]
        also_in_conv: bool,
    },
    /// Appends text to a stream; `idx` numbers the chunks from 0 (out of order
    /// is refused).
    StreamAppend {
        msg_id: String,
        idx: u64,
        text: String,
    },
    /// Ends a stream: the message becomes final.
    StreamEnd { msg_id: String },
    /// Stops a stream: the agent itself, a person in the conversation (Stop) or
    /// the owner.
    StreamCancel { msg_id: String },
    /// An owner whispers to their agent in a conversation: only the agent and
    /// the owner see it.
    Whisper {
        conv_id: String,
        agent_id: String,
        text: String,
    },
    /// Publishes an agent's draft (optionally edited): its owner or an
    /// approver.
    Approve {
        draft_id: String,
        #[serde(default)]
        text: Option<String>,
    },
    /// Discards an agent's draft.
    Reject { draft_id: String },
    /// A reviewing agent hands a draft to the people who approve it
    /// (docs/en/design/agents.md).
    Escalate {
        draft_id: String,
        #[serde(default)]
        note: Option<String>,
    },
    /// An owner takes over their agent in a conversation: the agent stops
    /// acting there.
    Takeover { conv_id: String, agent_id: String },
    /// Ends a takeover: the agent acts again.
    Release { conv_id: String, agent_id: String },
    /// An owner sets how their agent acts in a conversation: approval of
    /// drafts (`true` / `false` or rules, docs/en/design/agents.md), when it wakes, its
    /// topics. The conversation's managers set `required_approval` for any
    /// agent there.
    SetAgentPolicy {
        conv_id: String,
        agent_id: String,
        #[serde(default)]
        approval: Option<ApprovalArg>,
        #[serde(default)]
        required_approval: Option<ApprovalArg>,
        #[serde(default)]
        trigger: Option<Trigger>,
        /// Topics an agent in `topics` mode subscribes to.
        #[serde(default)]
        topics: Option<Vec<String>>,
    },
    /// An owner resets their agent's tripped guardrail (loop breaker) in a
    /// conversation.
    ResetGuard { conv_id: String, agent_id: String },
    /// Drafts waiting for the caller's approval.
    ListDrafts,
    /// Rename a conversation or replace its custom fields (owner, admin).
    UpdateConv {
        conv_id: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        fields: Option<Value>,
        /// Who may pin messages in a group: `members` or `admins` (MSG-21).
        #[serde(default)]
        pin_policy: Option<String>,
    },
    /// Marks a thread read up to `seq` on all of the caller's devices
    /// (MSG-19).
    ReadThread {
        conv_id: String,
        thread_id: String,
        seq: i64,
    },
    /// Follows a thread: its replies notify the caller.
    FollowThread { conv_id: String, thread_id: String },
    /// Stops following a thread.
    UnfollowThread { conv_id: String, thread_id: String },
    /// The threads the caller follows, unread first, then by latest reply
    /// (one conversation, or all of the caller's).
    ListThreads {
        #[serde(default)]
        conv_id: Option<String>,
        /// Only threads whose latest reply is older than this time (ms).
        #[serde(default)]
        before: Option<i64>,
        #[serde(default)]
        limit: Option<i64>,
    },
    /// A thread's root and the replies after a seq.
    PullThread {
        conv_id: String,
        thread_id: String,
        #[serde(default)]
        after_seq: i64,
        #[serde(default)]
        limit: Option<i64>,
    },
    /// Adds a reaction to a message (MSG-20); adding one twice changes
    /// nothing.
    React {
        conv_id: String,
        msg_id: String,
        key: String,
    },
    /// Removes the caller's reaction; removing one that is not there changes
    /// nothing.
    Unreact {
        conv_id: String,
        msg_id: String,
        key: String,
    },
    /// Who reacted to a message with a key, oldest first.
    ListReactions {
        conv_id: String,
        msg_id: String,
        key: String,
        /// Only reactions after this seq.
        #[serde(default)]
        after_seq: i64,
        #[serde(default)]
        limit: Option<i64>,
    },
    /// Pins a message to the conversation (MSG-21); pinning it again changes
    /// nothing.
    Pin { conv_id: String, msg_id: String },
    /// Unpins a message.
    Unpin { conv_id: String, msg_id: String },
    /// The conversation's pinned messages, latest pin first.
    ListPins {
        conv_id: String,
        /// Only pins older than this seq.
        #[serde(default)]
        before_seq: Option<i64>,
        #[serde(default)]
        limit: Option<i64>,
    },
    /// The thread summary, reactions and pin of up to 100 messages of a
    /// conversation, as the caller sees them.
    MsgSummary {
        conv_id: String,
        msg_ids: Vec<String>,
    },
    /// This user's devices (one per client token).
    ListDevices,
    /// Revokes one device's token and disconnects it (also this device).
    LogoutDevice { device_id: String },
    /// A new agent owned by the caller, with its first key
    /// (docs/en/design/agents.md–§2.3).
    CreateAgent { name: String },
    /// The caller's agents and their keys (never the secrets).
    ListAgents,
    /// The app's agent roles (docs/en/design/agents.md): name, description, version.
    ListRoles,
    /// The owner gives one of their agents a role (`null`: none).
    SetAgentRole {
        agent_id: String,
        #[serde(default)]
        role: Option<String>,
    },
    /// An agent's configuration (role, skills, MCP, APIs), as in `welcome`.
    GetConfig,
    /// Asks to be friends (docs/en/design/agents.md); with someone who takes anyone
    /// (the app's agents) it is done at once.
    FriendRequest {
        user_id: String,
        #[serde(default)]
        message: String,
    },
    /// Accepts a request from `user_id`.
    FriendAccept { user_id: String },
    /// Declines a request from `user_id`.
    FriendDecline { user_id: String },
    /// Ends a friendship (or withdraws my waiting request).
    FriendRemove { user_id: String },
    /// Friends and the requests waiting both ways.
    ListFriends,
    /// Who may add me: anyone, confirm (the default) or nobody.
    SetFriendPolicy { policy: String },
    /// The app's agents people can add as friends.
    ListAppAgents,
    /// A new key for one of the caller's agents; `revoke_others` revokes
    /// every other key in the same write.
    AgentKey {
        agent_id: String,
        #[serde(default)]
        revoke_others: bool,
    },
    /// Revokes one key of one of the caller's agents.
    RevokeAgentKey { agent_id: String, device_id: String },
    /// An agent asks a person to approve, answer or choose
    /// (docs/en/design/agents.md).
    RequestHuman {
        conv_id: String,
        kind: String,
        title: String,
        #[serde(default)]
        options: Option<Vec<String>>,
        #[serde(default)]
        context: Option<Value>,
        #[serde(default)]
        assignees: Option<Vec<String>>,
        #[serde(default)]
        ttl_secs: Option<i64>,
        client_req_id: String,
    },
    /// A person decides a request: approve, reject, answer or choose.
    Decide {
        request_id: String,
        decision: String,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        choice: Option<String>,
    },
    /// The asking agent withdraws a pending request.
    CancelRequest { request_id: String },
    /// A request and its outcome; waits up to `wait_secs` (≤ 25) for a
    /// decision.
    GetRequest {
        request_id: String,
        #[serde(default)]
        wait_secs: Option<i64>,
    },
    /// What waits for my decision across conversations: requests, my
    /// agents' drafts and hand-offs (docs/en/design/chat-components.md), oldest first.
    Inbox {
        /// At most this many (default 100, 1..500); `more` says if there were more.
        #[serde(default)]
        limit: Option<i64>,
    },
    /// Moves a task channel to a state (docs/en/design/agents.md).
    UpdateTask {
        conv_id: String,
        #[serde(default)]
        state: Option<String>,
        #[serde(default)]
        assignee: Option<String>,
        #[serde(default)]
        note: Option<String>,
    },
    /// Takes a request (`intent: request`) for oneself; the first wins.
    Claim { msg_id: String },
    /// Gives back a claimed request.
    ReleaseClaim { msg_id: String },
    /// Gives a coordinated agent the floor for a while.
    FloorGrant {
        conv_id: String,
        agent_id: String,
        #[serde(default)]
        ttl_secs: Option<i64>,
    },
    /// Gives back the floor in a coordinated conversation.
    FloorRelease { conv_id: String },
    /// Agents read what they may see of a conversation by seq
    /// (`after_seq` / `before_seq` / `limit`, docs/en/design/agents.md);
    /// people read pages of messages around an anchor (`anchor`, `before`,
    /// `after`, docs/en/design/sync-and-clients.md).
    History {
        conv_id: String,
        #[serde(default)]
        after_seq: Option<i64>,
        #[serde(default)]
        before_seq: Option<i64>,
        #[serde(default)]
        limit: Option<i64>,
        #[serde(default)]
        anchor: Option<Value>,
        #[serde(default)]
        before: Option<i64>,
        #[serde(default)]
        after: Option<i64>,
    },
    /// Application-level heartbeat (docs/en/design/sync-and-clients.md): answers the server time.
    Ping,
    /// An agent's view of a conversation for its model: summaries and recent
    /// messages within `max_tokens`.
    Context {
        conv_id: String,
        #[serde(default)]
        max_tokens: Option<i64>,
        /// Only this thread: its root and replies (MSG-19).
        #[serde(default)]
        thread_id: Option<String>,
    },
    /// An agent searches the messages it may see (optionally one conversation).
    Search {
        query: String,
        #[serde(default)]
        conv_id: Option<String>,
        #[serde(default)]
        limit: Option<i64>,
        #[serde(default, flatten)]
        filter: SearchFilter,
    },
    /// An agent stores a summary of a range of seqs (used by `context`).
    PutSummary {
        conv_id: String,
        from_seq: i64,
        to_seq: i64,
        text: String,
    },
    /// Agent checkpoints (docs/en/design/agents.md).
    StateGet {
        key: String,
        #[serde(default)]
        version: Option<i64>,
    },
    /// Writes a checkpoint; `version` is the version read (0 creates), so
    /// concurrent writes conflict.
    StatePut {
        key: String,
        value: Value,
        version: i64,
    },
    /// Lists checkpoint keys by prefix, in pages.
    StateList {
        #[serde(default)]
        prefix: Option<String>,
        #[serde(default)]
        after: Option<String>,
        #[serde(default)]
        limit: Option<i64>,
    },
    /// Deletes a checkpoint at the version read.
    StateDelete { key: String, version: i64 },
    /// An owner's emergency stop, resume and destroy (docs/en/design/agents.md–§4.4).
    StopAgent {
        agent_id: String,
        #[serde(default)]
        reason: Option<String>,
    },
    /// Resumes a stopped agent.
    ResumeAgent { agent_id: String },
    /// Destroys an agent for good (`confirm` repeats its name): keys revoked,
    /// memberships ended, checkpoints deleted.
    DestroyAgent { agent_id: String, confirm: String },
    /// An agent hands the conversation to a person; that person or the
    /// owner hands it back (docs/en/design/agents.md).
    Handoff {
        conv_id: String,
        #[serde(default)]
        to: Option<String>,
        note: String,
    },
    /// Hands a handed-off conversation back to the agent.
    Handback {
        conv_id: String,
        agent_id: String,
        #[serde(default)]
        note: Option<String>,
    },
    /// An agent's progress report (side channel).
    Progress {
        conv_id: String,
        #[serde(default)]
        msg_id: Option<String>,
        kind: String,
        text: String,
        #[serde(default)]
        data: Option<Value>,
    },
    /// Thumbs up / down / none on an agent's message.
    Feedback {
        msg_id: String,
        #[serde(default)]
        rating: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    /// An owner's view of the feedback on an agent.
    ListFeedback {
        agent_id: String,
        #[serde(default)]
        limit: Option<i64>,
    },
    /// Reports a message, a user or a conversation (docs/en/design/security.md).
    Report {
        target_kind: String,
        target_id: String,
        category: String,
        #[serde(default)]
        text: Option<String>,
    },
    /// The answers to a card so far (docs/en/design/chat-components.md): who answered which block
    /// and when; the values only to the one who answered and the sender.
    CardAnswers { msg_id: String },
    /// A member's response to a card: a button, a picked list row, a filled
    /// form, or an answer to the developer's own block.
    Respond {
        msg_id: String,
        block_id: String,
        #[serde(default)]
        action: Option<String>,
        #[serde(default)]
        values: Option<Value>,
    },
    /// Starts uploading a file to a conversation (docs/en/design/messaging.md): the
    /// addresses to PUT its parts (and thumbnail) to.
    UploadStart {
        conv_id: String,
        name: String,
        size: u64,
        #[serde(default)]
        mime: Option<String>,
        /// Bytes of the thumbnail the client made (images).
        #[serde(default)]
        thumb_size: Option<u64>,
    },
    /// More part addresses, from part `from`.
    UploadParts {
        conv_id: String,
        file_id: String,
        #[serde(default)]
        from: Option<u32>,
    },
    /// All parts are up: checks them and makes the file usable.
    UploadComplete {
        conv_id: String,
        file_id: String,
        #[serde(default)]
        parts: Vec<PartTag>,
    },
    /// A short-lived address to read a file (`thumb` or `original`).
    FileUrl {
        conv_id: String,
        file_id: String,
        #[serde(default)]
        variant: Option<String>,
    },
}

/// A part the client uploaded, and the ETag the store answered.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct PartTag {
    pub n: u32,
    #[serde(default)]
    pub etag: String,
}

impl ClientMsg {
    pub fn name(&self) -> &'static str {
        match self {
            ClientMsg::Hello { .. } => "hello",
            ClientMsg::ListConvs => "list_convs",
            ClientMsg::CreateConv { .. } => "create_conv",
            ClientMsg::AddMember { .. } => "add_member",
            ClientMsg::RemoveMember { .. } => "remove_member",
            ClientMsg::Leave { .. } => "leave",
            ClientMsg::SetRole { .. } => "set_role",
            ClientMsg::TransferOwner { .. } => "transfer_owner",
            ClientMsg::Dissolve { .. } => "dissolve",
            ClientMsg::Block { .. } => "block",
            ClientMsg::Unblock { .. } => "unblock",
            ClientMsg::ListBlocks => "list_blocks",
            ClientMsg::FindUser { .. } => "find_user",
            ClientMsg::Send { .. } => "send",
            ClientMsg::Edit { .. } => "edit",
            ClientMsg::Signal { .. } => "signal",
            ClientMsg::Recall { .. } => "recall",
            ClientMsg::Pull { .. } => "pull",
            ClientMsg::PullSide { .. } => "pull_side",
            ClientMsg::Read { .. } => "read",
            ClientMsg::Sync { .. } => "sync",
            ClientMsg::Ack { .. } => "ack",
            ClientMsg::SetMute { .. } => "set_mute",
            ClientMsg::Unread => "unread",
            ClientMsg::Heads { .. } => "heads",
            ClientMsg::SetPrefs { .. } => "set_prefs",
            ClientMsg::Observe { .. } => "observe",
            ClientMsg::Focus { .. } => "focus",
            ClientMsg::StreamStart { .. } => "stream_start",
            ClientMsg::StreamAppend { .. } => "stream_append",
            ClientMsg::StreamEnd { .. } => "stream_end",
            ClientMsg::StreamCancel { .. } => "stream_cancel",
            ClientMsg::Whisper { .. } => "whisper",
            ClientMsg::Approve { .. } => "approve",
            ClientMsg::Reject { .. } => "reject",
            ClientMsg::Escalate { .. } => "escalate",
            ClientMsg::Takeover { .. } => "takeover",
            ClientMsg::Release { .. } => "release",
            ClientMsg::SetAgentPolicy { .. } => "set_agent_policy",
            ClientMsg::ResetGuard { .. } => "reset_guard",
            ClientMsg::ListDrafts => "list_drafts",
            ClientMsg::ListDevices => "list_devices",
            ClientMsg::UpdateConv { .. } => "update_conv",
            ClientMsg::LogoutDevice { .. } => "logout_device",
            ClientMsg::CreateAgent { .. } => "create_agent",
            ClientMsg::ListAgents => "list_agents",
            ClientMsg::ListRoles => "list_roles",
            ClientMsg::SetAgentRole { .. } => "set_agent_role",
            ClientMsg::GetConfig => "get_config",
            ClientMsg::FriendRequest { .. } => "friend_request",
            ClientMsg::FriendAccept { .. } => "friend_accept",
            ClientMsg::FriendDecline { .. } => "friend_decline",
            ClientMsg::FriendRemove { .. } => "friend_remove",
            ClientMsg::ListFriends => "list_friends",
            ClientMsg::SetFriendPolicy { .. } => "set_friend_policy",
            ClientMsg::ListAppAgents => "list_app_agents",
            ClientMsg::AgentKey { .. } => "agent_key",
            ClientMsg::RevokeAgentKey { .. } => "revoke_agent_key",
            ClientMsg::RequestHuman { .. } => "request_human",
            ClientMsg::Decide { .. } => "decide",
            ClientMsg::CancelRequest { .. } => "cancel_request",
            ClientMsg::GetRequest { .. } => "get_request",
            ClientMsg::Inbox { .. } => "inbox",
            ClientMsg::Ping => "ping",
            ClientMsg::UpdateTask { .. } => "update_task",
            ClientMsg::Claim { .. } => "claim",
            ClientMsg::ReleaseClaim { .. } => "release_claim",
            ClientMsg::FloorGrant { .. } => "floor_grant",
            ClientMsg::FloorRelease { .. } => "floor_release",
            ClientMsg::History { .. } => "history",
            ClientMsg::Context { .. } => "context",
            ClientMsg::Search { .. } => "search",
            ClientMsg::PutSummary { .. } => "put_summary",
            ClientMsg::StateGet { .. } => "state_get",
            ClientMsg::StatePut { .. } => "state_put",
            ClientMsg::StateList { .. } => "state_list",
            ClientMsg::StateDelete { .. } => "state_delete",
            ClientMsg::StopAgent { .. } => "stop_agent",
            ClientMsg::ResumeAgent { .. } => "resume_agent",
            ClientMsg::DestroyAgent { .. } => "destroy_agent",
            ClientMsg::Handoff { .. } => "handoff",
            ClientMsg::Handback { .. } => "handback",
            ClientMsg::Respond { .. } => "respond",
            ClientMsg::CardAnswers { .. } => "card_answers",
            ClientMsg::Progress { .. } => "progress",
            ClientMsg::Feedback { .. } => "feedback",
            ClientMsg::ListFeedback { .. } => "list_feedback",
            ClientMsg::Report { .. } => "report",
            ClientMsg::UploadStart { .. } => "upload_start",
            ClientMsg::UploadParts { .. } => "upload_parts",
            ClientMsg::UploadComplete { .. } => "upload_complete",
            ClientMsg::FileUrl { .. } => "file_url",
            ClientMsg::ReadThread { .. } => "read_thread",
            ClientMsg::FollowThread { .. } => "follow_thread",
            ClientMsg::UnfollowThread { .. } => "unfollow_thread",
            ClientMsg::ListThreads { .. } => "list_threads",
            ClientMsg::PullThread { .. } => "pull_thread",
            ClientMsg::React { .. } => "react",
            ClientMsg::Unreact { .. } => "unreact",
            ClientMsg::ListReactions { .. } => "list_reactions",
            ClientMsg::Pin { .. } => "pin",
            ClientMsg::Unpin { .. } => "unpin",
            ClientMsg::ListPins { .. } => "list_pins",
            ClientMsg::MsgSummary { .. } => "msg_summary",
        }
    }

    /// The conversation a request addresses directly, if any.
    pub fn conv_id(&self) -> Option<&str> {
        match self {
            ClientMsg::AddMember { conv_id, .. }
            | ClientMsg::RemoveMember { conv_id, .. }
            | ClientMsg::Leave { conv_id }
            | ClientMsg::SetRole { conv_id, .. }
            | ClientMsg::TransferOwner { conv_id, .. }
            | ClientMsg::Dissolve { conv_id }
            | ClientMsg::Send { conv_id, .. }
            | ClientMsg::RequestHuman { conv_id, .. }
            | ClientMsg::UpdateTask { conv_id, .. }
            | ClientMsg::FloorGrant { conv_id, .. }
            | ClientMsg::FloorRelease { conv_id }
            | ClientMsg::History { conv_id, .. }
            | ClientMsg::Context { conv_id, .. }
            | ClientMsg::PutSummary { conv_id, .. }
            | ClientMsg::Handoff { conv_id, .. }
            | ClientMsg::Handback { conv_id, .. }
            | ClientMsg::Progress { conv_id, .. }
            | ClientMsg::Signal { conv_id, .. }
            | ClientMsg::UpdateConv { conv_id, .. }
            | ClientMsg::Pull { conv_id, .. }
            | ClientMsg::PullSide { conv_id, .. }
            | ClientMsg::Read { conv_id, .. }
            | ClientMsg::SetMute { conv_id, .. }
            | ClientMsg::Observe { conv_id, .. }
            | ClientMsg::StreamStart { conv_id, .. }
            | ClientMsg::Whisper { conv_id, .. }
            | ClientMsg::Takeover { conv_id, .. }
            | ClientMsg::Release { conv_id, .. }
            | ClientMsg::SetAgentPolicy { conv_id, .. }
            | ClientMsg::ResetGuard { conv_id, .. }
            | ClientMsg::UploadStart { conv_id, .. }
            | ClientMsg::UploadParts { conv_id, .. }
            | ClientMsg::UploadComplete { conv_id, .. }
            | ClientMsg::FileUrl { conv_id, .. }
            | ClientMsg::ReadThread { conv_id, .. }
            | ClientMsg::FollowThread { conv_id, .. }
            | ClientMsg::UnfollowThread { conv_id, .. }
            | ClientMsg::PullThread { conv_id, .. }
            | ClientMsg::React { conv_id, .. }
            | ClientMsg::Unreact { conv_id, .. }
            | ClientMsg::ListReactions { conv_id, .. }
            | ClientMsg::Pin { conv_id, .. }
            | ClientMsg::Unpin { conv_id, .. }
            | ClientMsg::ListPins { conv_id, .. }
            | ClientMsg::MsgSummary { conv_id, .. } => Some(conv_id),
            ClientMsg::Focus { conv_id, .. } | ClientMsg::ListThreads { conv_id, .. } => {
                conv_id.as_deref()
            }
            ClientMsg::Hello { .. }
            | ClientMsg::ListConvs
            | ClientMsg::CreateConv { .. }
            | ClientMsg::Block { .. }
            | ClientMsg::Unblock { .. }
            | ClientMsg::ListBlocks
            | ClientMsg::FindUser { .. }
            | ClientMsg::Edit { .. }
            | ClientMsg::Recall { .. }
            | ClientMsg::Sync { .. }
            | ClientMsg::Ack { .. }
            | ClientMsg::Unread
            | ClientMsg::Heads { .. }
            | ClientMsg::SetPrefs { .. }
            | ClientMsg::StreamAppend { .. }
            | ClientMsg::StreamEnd { .. }
            | ClientMsg::StreamCancel { .. }
            | ClientMsg::Approve { .. }
            | ClientMsg::Reject { .. }
            | ClientMsg::Escalate { .. }
            | ClientMsg::ListDrafts
            | ClientMsg::ListDevices
            | ClientMsg::LogoutDevice { .. }
            | ClientMsg::CreateAgent { .. }
            | ClientMsg::ListAgents
            | ClientMsg::ListRoles
            | ClientMsg::SetAgentRole { .. }
            | ClientMsg::GetConfig
            | ClientMsg::FriendRequest { .. }
            | ClientMsg::FriendAccept { .. }
            | ClientMsg::FriendDecline { .. }
            | ClientMsg::FriendRemove { .. }
            | ClientMsg::ListFriends
            | ClientMsg::SetFriendPolicy { .. }
            | ClientMsg::ListAppAgents
            | ClientMsg::AgentKey { .. }
            | ClientMsg::RevokeAgentKey { .. }
            | ClientMsg::Decide { .. }
            | ClientMsg::CancelRequest { .. }
            | ClientMsg::GetRequest { .. }
            | ClientMsg::Inbox { .. }
            | ClientMsg::Claim { .. }
            | ClientMsg::ReleaseClaim { .. }
            | ClientMsg::Ping
            | ClientMsg::Search { .. }
            | ClientMsg::StateGet { .. }
            | ClientMsg::StatePut { .. }
            | ClientMsg::StateList { .. }
            | ClientMsg::StateDelete { .. }
            | ClientMsg::StopAgent { .. }
            | ClientMsg::ResumeAgent { .. }
            | ClientMsg::DestroyAgent { .. }
            | ClientMsg::Feedback { .. }
            | ClientMsg::ListFeedback { .. }
            | ClientMsg::Report { .. }
            | ClientMsg::Respond { .. }
            | ClientMsg::CardAnswers { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AckItem {
    pub conv_id: String,
    #[serde(default)]
    pub seq: i64,
    #[serde(default)]
    pub side_seq: i64,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
    /// For temporary refusals (`rate_limited`, `overloaded`, …): how long
    /// the caller should wait before trying again (docs/en/design/sync-and-clients.md).
    pub retry_after_ms: Option<u64>,
    /// The business rule that refused the request (`rule!`, docs/en/business-rules.md).
    pub rule: Option<&'static str>,
}

impl ApiError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            code,
            message: message.into(),
            retry_after_ms: None,
            rule: None,
        }
    }

    /// A refusal by a named rule; see `rule!`. With `AGENTIM_RULES_HIT` set
    /// (tests), the rule's id is appended to that file, so CI can check that
    /// every rule is triggered by some test.
    pub fn rule(id: &'static str, code: &'static str, message: impl Into<String>) -> Self {
        static HIT: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
            std::sync::OnceLock::new();
        let hit = HIT.get_or_init(|| {
            let path = std::env::var_os("AGENTIM_RULES_HIT")?;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
                .map(std::sync::Mutex::new)
        });
        if let Some(f) = hit {
            use std::io::Write;
            let _ = writeln!(f.lock().unwrap_or_else(|e| e.into_inner()), "{id}");
        }
        ApiError {
            code,
            message: message.into(),
            retry_after_ms: None,
            rule: Some(id),
        }
    }

    pub fn retry_after(mut self, ms: u64) -> Self {
        self.retry_after_ms = Some(ms);
        self
    }

    /// The `error` object of a response frame.
    /// What a client is told: internal failures (the database's own
    /// message names tables and constraints) only say to retry; they are
    /// logged in full where they happen.
    pub fn public(&self) -> ApiError {
        match self.code {
            "storage" | "internal" => ApiError {
                code: self.code,
                message: "the request could not be completed; retry".into(),
                retry_after_ms: self.retry_after_ms,
                rule: None,
            },
            _ => self.clone(),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({"code": self.code, "message": self.message});
        if let Some(ms) = self.retry_after_ms {
            v["retry_after_ms"] = serde_json::json!(ms);
        }
        if let Some(r) = self.rule {
            v["rule"] = serde_json::json!(r);
        }
        v
    }

    /// An error received from another node: its code as one of this
    /// program's codes (codes are a closed set; anything unknown is
    /// `unavailable`, the retryable answer).
    pub fn from_wire(code: &str, message: String) -> Self {
        Self::from_wire_rule(code, message, None)
    }

    /// Like `from_wire`, with the rule the other node named (interned the
    /// same way: rule ids are a closed set too).
    pub fn from_wire_rule(code: &str, message: String, rule: Option<&str>) -> Self {
        let mut e = Self::from_wire_code(code, message);
        e.rule = rule.and_then(intern_rule);
        e
    }

    fn from_wire_code(code: &str, message: String) -> Self {
        static CODES: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
        let mut known = CODES.lock().unwrap_or_else(|e| e.into_inner());
        let code = match known.iter().find(|c| **c == code) {
            Some(c) => *c,
            None if known.len() < 256
                && !code.is_empty()
                && code.len() <= 40
                && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') =>
            {
                let c: &'static str = Box::leak(code.to_string().into_boxed_str());
                known.push(c);
                c
            }
            None => "unavailable",
        };
        ApiError::new(code, message)
    }
}

/// A rule id from another node as a `&'static str` (at most 1024 kept).
fn intern_rule(r: &str) -> Option<&'static str> {
    static RULES: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut known = RULES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(r) = known.iter().find(|k| **k == r) {
        return Some(*r);
    }
    let ok = known.len() < 1024
        && !r.is_empty()
        && r.len() <= 64
        && r.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.');
    ok.then(|| {
        let s: &'static str = Box::leak(r.to_string().into_boxed_str());
        known.push(s);
        s
    })
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::new("storage", e.to_string())
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
