//! Stable JSON wire contract. All timestamps are Unix seconds; IDs and cursors are unsigned integers.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_MESSAGE_ATTACHMENT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_MESSAGE_ATTACHMENTS: usize = 16;

/// Opaque download-only bytes. Names are display metadata, never filesystem paths.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attachment {
    pub id: u64,
    pub message_id: u64,
    pub name: String,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
/// A point-in-time view, not proof of continuing access. On HTTP 409 from
/// events discard cached board data, refetch snapshot and ancillary state,
/// and resume at the new cursor. On 401 discard the view and reauthenticate.
pub struct Snapshot {
    pub cursor: u64,
    pub channels: Vec<String>,
    pub messages: Vec<Message>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub id: u64,
    pub channel: String,
    pub parent: Option<u64>,
    pub author: String,
    /// Self-reported client metadata, never an authenticated identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    pub text: String,
    pub labels: BTreeMap<String, String>,
    pub created_at: u64,
    pub task: Option<Task>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Task {
    pub state: TaskState,
    pub needs: Vec<u64>,
    pub assignee: Option<String>,
    pub owner: Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub finished_by: Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub result: Option<String>,
    pub expires_at: Option<u64>,
    /// First explicit start of this task or one of its subtasks, independent of lease ownership.
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub started_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState { Ready, Waiting, Claimed, InProgress, Blocked, Completed, Failed, Canceled }

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, clap::ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaimAssignment { #[default] Eligible, Mine, Unassigned }

/// Board namespaces are the first segment of channel paths. Organization is
/// fetched separately from Snapshot and contains only visible tasks/boards.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Organization {
    pub areas: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub area_owners: BTreeMap<String, BTreeMap<String, String>>,
    pub tasks: BTreeMap<u64, TaskPlacement>,
    #[serde(default)]
    pub sprints: BTreeMap<u64, Sprint>,
    #[serde(default)]
    pub task_sprints: BTreeMap<u64, u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Sprint {
    pub id: u64,
    pub board: String,
    pub title: String,
    pub description: String,
    pub finished: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskPlacement {
    pub area: Option<String>,
    /// Task hierarchy, independent of Message::parent (thread replies).
    pub parent: Option<u64>,
    pub global: bool,
    /// Stable actor IDs, not profiles or display names.
    pub done_by: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Post {
        channel: String,
        text: String,
        #[serde(default)] labels: BTreeMap<String, String>,
        #[serde(default)] task: bool,
        #[serde(default)] needs: Vec<u64>,
        #[serde(default)] assignee: Option<String>,
    },
    CreateArea { board: String, name: String },
    CreateSprint { board: String, title: String, description: String },
    SetTaskSprint { id: u64, sprint: Option<u64> },
    FinishSprint { id: u64, allow_unfinished: bool },
    PostSprintTask {
        channel: String, text: String,
        #[serde(default)] labels: BTreeMap<String, String>,
        #[serde(default)] needs: Vec<u64>,
        #[serde(default)] assignee: Option<String>,
        sprint: u64,
    },
    SetAreaOwner { board: String, name: String, owner: Option<String> },
    PostTask {
        channel: String,
        text: String,
        #[serde(default)] labels: BTreeMap<String, String>,
        #[serde(default)] needs: Vec<u64>,
        #[serde(default)] assignee: Option<String>,
        #[serde(default)] area: Option<String>,
        #[serde(default)] parent: Option<u64>,
        #[serde(default)] global: bool,
    },
    /// Replace editable task fields, preserving whether the task is global.
    UpdateTask { id: u64, text: String, labels: BTreeMap<String, String>, needs: Vec<u64>, assignee: Option<String>, area: Option<String>, parent: Option<u64> },
    /// Edit current fields without overwriting legacy placement metadata.
    UpdateTaskFields { id: u64, text: String, labels: BTreeMap<String, String>, needs: Vec<u64>, assignee: Option<String>, parent: Option<u64> },
    AcknowledgeTask { id: u64, done: bool },
    Reply { thread: u64, text: String },
    ReplyWithLabels { thread: u64, text: String, #[serde(default)] labels: BTreeMap<String, String> },
    CreateChannel { channel: String },
    /// Host only. Board namespace roots and their default general channel are protected.
    DeleteChannel { channel: String },
    Claim { id: u64, lease_secs: u64 },
    Start { id: u64 },
    ClaimNext { channel: String, #[serde(default)] labels: BTreeMap<String, String>, #[serde(default)] assignment: ClaimAssignment, lease_secs: u64 },
    Renew { id: u64, lease_secs: u64 },
    /// Creator manages unleased work; a live lease holder may hand off their own
    /// task. A changed assignment releases the lease; the recipient must claim.
    Assign { id: u64, assignee: Option<String> },
    EditTask { id: u64, text: String, labels: BTreeMap<String, String>, needs: Vec<u64>, assignee: Option<String> },
    /// Durable manual block with a required reason, also posted as a reply.
    Block { id: u64, text: String },
    /// Creator or the actor who manually blocked may clear that manual block.
    /// Dependencies are still authoritative and may leave the task blocked.
    Unblock { id: u64 },
    Complete { id: u64, text: String },
    /// Complete the leased task without requiring its subtasks to be completed.
    CompleteAnyway { id: u64, text: String },
    Fail { id: u64, text: String },
    Cancel { id: u64 },
    Edit { id: u64, text: String },
    /// Root creator only; removes the thread unless a task depends on it or is its subtask.
    DeleteThread { id: u64 },
    SetLabels { id: u64, labels: BTreeMap<String, String> },
    React { id: u64, reaction: String, #[serde(default)] remove: bool },
    Schedule { channel: String, text: String, publish_at: u64, #[serde(default)] labels: BTreeMap<String, String> },
    ScheduleFollowup { thread: u64, text: String, publish_at: u64, #[serde(default)] internal_only: bool, #[serde(default)] labels: BTreeMap<String, String> },
    CancelSchedule { id: u64 },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Outcome {
    pub id: Option<u64>,
    pub cursor: u64,
}

/// Apply message upserts by ID, channel creations by name, and reaction events by (ID, actor, reaction).
/// Cursors are globally ordered, not necessarily contiguous for channel-restricted credentials.
/// ThreadDeleted removes the root, its replies and their local attachments/reactions.
/// Historical upserts/reactions for deleted messages are pruned; replay may have gaps.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub cursor: u64,
    pub created_at: u64,
    pub actor: String,
    #[serde(flatten)] pub kind: EventKind,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    Message { message: Message },
    /// Refetch Organization when a board's area or sprint catalog changes. Task placement
    /// and acknowledgement changes instead emit a Message upsert.
    OrganizationChanged { board: String },
    ThreadDeleted { id: u64, channel: String },
    ChannelCreated { channel: String },
    /// Drop the exact channel and all its local content, not descendant channels.
    ChannelDeleted { channel: String },
    Reaction { id: u64, channel: String, reaction: String, remove: bool },
    Scheduled { id: u64, channel: String, publish_at: u64 },
    ScheduleCanceled { id: u64, channel: String },
    SchedulePublished { id: u64, channel: String, message_id: u64 },
}

/// Reaction state is fetched separately to keep the agreed Snapshot shape stable.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reaction {
    pub id: u64,
    pub channel: String,
    pub actor: String,
    pub reaction: String,
}

/// A credential's pending post or retained private reminder.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduledPost {
    pub id: u64,
    pub channel: String,
    pub author: String,
    pub text: String,
    pub labels: BTreeMap<String, String>,
    pub publish_at: u64,
    #[serde(default)] pub thread: Option<u64>,
    #[serde(default)] pub internal_only: bool,
    #[serde(default)] pub fired: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Discovery { pub url: String, pub token: String }

/// An upper bound on operations, never a replacement for resource authorization.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum Grant {
    Notifications,
    Read, Attach, DisplayName, Post, PostTask, Reply, EditTask,
    CreateSprint, FinishSprint,
    #[value(skip)] CreateArea,
    #[value(skip)] SetAreaOwner,
    AcknowledgeTask, CreateChannel, Claim, ClaimNext, Start, Renew, Assign, Block, Unblock,
    Complete, Fail, Cancel, Edit, DeleteThread, SetLabels, React, Schedule, CancelSchedule,
}
impl Action {
    pub fn grant(&self) -> Option<Grant> {
        use Action::*;
        Some(match self {
            DeleteChannel { .. } => return None, // host only
            Post { task:false,.. } => Grant::Post,
            Post { task:true,.. } | PostTask { .. } | PostSprintTask { .. } => Grant::PostTask,
            SetTaskSprint { .. } => Grant::EditTask,
            CreateSprint { .. } => Grant::CreateSprint,
            FinishSprint { .. } => Grant::FinishSprint,
            Reply { .. } | ReplyWithLabels { .. } => Grant::Reply,
            EditTask { .. } | UpdateTask { .. } | UpdateTaskFields { .. } => Grant::EditTask,
            CreateArea { .. } => Grant::CreateArea,
            SetAreaOwner { .. } => Grant::SetAreaOwner,
            AcknowledgeTask { .. } => Grant::AcknowledgeTask,
            CreateChannel { .. } => Grant::CreateChannel,
            Claim { .. } => Grant::Claim,
            ClaimNext { .. } => Grant::ClaimNext,
            Start { .. } => Grant::Start,
            Renew { .. } => Grant::Renew,
            Assign { .. } => Grant::Assign,
            Block { .. } => Grant::Block,
            Unblock { .. } => Grant::Unblock,
            Complete { .. } | CompleteAnyway { .. } => Grant::Complete,
            Fail { .. } => Grant::Fail,
            Cancel { .. } => Grant::Cancel,
            Edit { .. } => Grant::Edit,
            DeleteThread { .. } => Grant::DeleteThread,
            SetLabels { .. } => Grant::SetLabels,
            React { .. } => Grant::React,
            Schedule { .. } | ScheduleFollowup { .. } => Grant::Schedule,
            CancelSchedule { .. } => Grant::CancelSchedule,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InviteRequest {
    pub actor: String,
    /// Empty means all channels; otherwise exact channels plus optional `<project>/*`
    /// scopes covering the project root and all present/future slash descendants.
    /// A project is one validated channel segment (1..80 ASCII letters, digits,
    /// dash, underscore or dot, excluding `.` and `..`); other globs are invalid.
    #[serde(default)] pub channels: Vec<String>,
    pub expires_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Invite {
    pub id: u64,
    /// Returned only at creation. Stored server-side as a SHA-256 digest.
    pub token: String,
    pub actor: String,
    pub channels: Vec<String>,
    pub expires_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiError { pub error: String }

/// An active actor permitted to work in the requested channel. Names are
/// presentation only; assignments and authorization always use `actor`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub actor: String,
    pub display_name: Option<String>,
}

/// `/me` also distinguishes a personal credential from a reusable join link.
/// Older servers omit the flag; automatic device-wide writes must fail closed.
#[derive(Serialize, Deserialize)]
pub struct Membership {
    #[serde(flatten)] pub member: Member,
    #[serde(default)] pub personal: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayNameRequest {
    /// Surrounding whitespace is trimmed; empty resets the name.
    pub name: String,
}

pub fn validate_display_name(name: &str) -> Result<(), &'static str> {
    if name.len() > 256 || name.chars().any(|c| c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')) {
        Err("display name must be at most 256 bytes without control or bidi override characters")
    } else { Ok(()) }
}