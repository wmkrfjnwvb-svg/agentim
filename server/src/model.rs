//! Wire protocol and shared data types.
//!
//! Every client frame is a JSON object `{"op": "...", "rid": <optional u64>, ...}`.
//! Server frames are JSON objects with a `type` field. Responses to a request
//! echo its `rid`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const TENANT_DEFAULT: &str = "default";
pub const UNIT_DEFAULT: &str = "default";

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

#[derive(Debug, Clone, Serialize)]
pub struct User {
    pub id: String,
    pub name: String,
    pub kind: UserKind,
    pub owner_id: Option<String>,
    /// Organisation unit; decides which database new conversations go to.
    pub unit_id: String,
}

/// How an agent member is triggered in a conversation, which also bounds what
/// it can read (see docs/03-ai-native.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// Receives every visible event.
    All,
    /// Receives only events that mention it, reply to it, or are its own.
    Mention,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::All => "all",
            Trigger::Mention => "mention",
        }
    }
    pub fn parse(s: &str) -> Trigger {
        if s == "all" {
            Trigger::All
        } else {
            Trigger::Mention
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
    pub approval: bool,
    pub join_seq: i64,
}

/// Snapshot of a message at a given version. Embedded in every `msg.*` event so
/// that a pulled event range is self-contained.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MsgView {
    pub msg_id: String,
    pub conv_id: String,
    pub created_seq: i64,
    pub sender_id: String,
    pub sender_name: String,
    /// Written by the server from the authenticated identity; cannot be forged.
    pub sender_type: UserKind,
    pub on_behalf_of: Option<String>,
    /// `text` or a namespaced custom kind (docs/02-architecture.md §2.6);
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

#[derive(Debug, Clone, Deserialize)]
pub struct Envelope {
    pub rid: Option<u64>,
    #[serde(flatten)]
    pub msg: ClientMsg,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Handshake (docs/02-architecture.md §2.5). Every field is optional so
    /// clients that predate negotiation keep working.
    Hello {
        #[serde(default)]
        protocol: Option<Vec<u32>>,
        #[serde(default)]
        client: Option<crate::protocol::ClientInfo>,
        #[serde(default)]
        features: Option<Vec<String>>,
    },
    ListConvs,
    CreateConv {
        kind: String,
        title: String,
        members: Vec<String>,
    },
    AddMember {
        conv_id: String,
        user_id: String,
    },
    /// Owner / admin removes a member; anyone removes an agent they own.
    RemoveMember {
        conv_id: String,
        user_id: String,
    },
    Leave {
        conv_id: String,
    },
    /// Owner only: `admin` or `member`.
    SetRole {
        conv_id: String,
        user_id: String,
        role: String,
    },
    TransferOwner {
        conv_id: String,
        user_id: String,
    },
    /// Owner only: everyone loses access and the content is deleted.
    Dissolve {
        conv_id: String,
    },
    Block {
        user_id: String,
    },
    Unblock {
        user_id: String,
    },
    ListBlocks,
    /// Exact user name; only users the caller may discover are returned.
    FindUser {
        name: String,
    },
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
    },
    Edit {
        msg_id: String,
        text: String,
        /// Custom kinds only: replaces the payload when given.
        #[serde(default)]
        payload: Option<Value>,
    },
    /// Ephemeral event to the online members: no storage, no seq.
    Signal {
        conv_id: String,
        kind: String,
        #[serde(default)]
        data: Option<Value>,
    },
    Recall {
        msg_id: String,
    },
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
    PullSide {
        conv_id: String,
        #[serde(default)]
        after_side_seq: i64,
    },
    Read {
        conv_id: String,
        seq: i64,
    },
    Focus {
        conv_id: Option<String>,
        #[serde(default)]
        as_agent: Option<String>,
    },
    StreamStart {
        conv_id: String,
        client_msg_id: String,
        #[serde(default)]
        reply_to: Option<String>,
        #[serde(default)]
        causation_id: Option<String>,
    },
    StreamAppend {
        msg_id: String,
        idx: u64,
        text: String,
    },
    StreamEnd {
        msg_id: String,
    },
    StreamCancel {
        msg_id: String,
    },
    Whisper {
        conv_id: String,
        agent_id: String,
        text: String,
    },
    Approve {
        draft_id: String,
        #[serde(default)]
        text: Option<String>,
    },
    Reject {
        draft_id: String,
    },
    Takeover {
        conv_id: String,
        agent_id: String,
    },
    Release {
        conv_id: String,
        agent_id: String,
    },
    SetAgentPolicy {
        conv_id: String,
        agent_id: String,
        #[serde(default)]
        approval: Option<bool>,
        #[serde(default)]
        trigger: Option<Trigger>,
    },
    ResetGuard {
        conv_id: String,
        agent_id: String,
    },
    ListDrafts,
    Stats,
    /// This user's devices (one per client token).
    ListDevices,
    /// Revokes one device's token and disconnects it (also this device).
    LogoutDevice {
        device_id: String,
    },
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
            ClientMsg::Focus { .. } => "focus",
            ClientMsg::StreamStart { .. } => "stream_start",
            ClientMsg::StreamAppend { .. } => "stream_append",
            ClientMsg::StreamEnd { .. } => "stream_end",
            ClientMsg::StreamCancel { .. } => "stream_cancel",
            ClientMsg::Whisper { .. } => "whisper",
            ClientMsg::Approve { .. } => "approve",
            ClientMsg::Reject { .. } => "reject",
            ClientMsg::Takeover { .. } => "takeover",
            ClientMsg::Release { .. } => "release",
            ClientMsg::SetAgentPolicy { .. } => "set_agent_policy",
            ClientMsg::ResetGuard { .. } => "reset_guard",
            ClientMsg::ListDrafts => "list_drafts",
            ClientMsg::Stats => "stats",
            ClientMsg::ListDevices => "list_devices",
            ClientMsg::LogoutDevice { .. } => "logout_device",
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
            | ClientMsg::Signal { conv_id, .. }
            | ClientMsg::Pull { conv_id, .. }
            | ClientMsg::PullSide { conv_id, .. }
            | ClientMsg::Read { conv_id, .. }
            | ClientMsg::StreamStart { conv_id, .. }
            | ClientMsg::Whisper { conv_id, .. }
            | ClientMsg::Takeover { conv_id, .. }
            | ClientMsg::Release { conv_id, .. }
            | ClientMsg::SetAgentPolicy { conv_id, .. }
            | ClientMsg::ResetGuard { conv_id, .. } => Some(conv_id),
            ClientMsg::Focus { conv_id, .. } => conv_id.as_deref(),
            _ => None,
        }
    }
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
}

impl ApiError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            code,
            message: message.into(),
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::new("storage", e.to_string())
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
