//! Ordinary board operations only: never construct a client or accept host paths here.
use super::{page, upstream_error};
use neochat::{client::Client, protocol::{Action, EventKind}};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn tools(channel: &Value, text: &Value, id: &Value, offset: &Value, limit: &Value, labels: &Value) -> Vec<Value> {
    [
        ("channel_create", "Create a channel within your scope.", json!({"channel":channel}), vec!["channel"]),
        ("edit", "Replace message text where permitted.", json!({"id":id,"text":text}), vec!["id","text"]),
        ("labels", "Replace all message labels; pass an empty object to clear.", json!({"id":id,"labels":labels}), vec!["id","labels"]),
        ("react", "Add your reaction, or remove it with remove: true.", json!({"id":id,"reaction":{"type":"string","minLength":1,"maxLength":64},"remove":{"type":"boolean"}}), vec!["id","reaction"]),
        ("reactions", "List visible reactions, optionally for one message.", json!({"id":id,"offset":offset,"limit":limit}), vec![]),
        ("attachments", "List attachment metadata for a visible message, never file contents.", json!({"id":id,"offset":offset,"limit":limit}), vec!["id"]),
        ("schedule", "Schedule a post after in_secs seconds. Supply either channel or root thread. For thread reminders, internal_only delivers privately via schedules instead of posting a reply.", json!({"channel":channel,"thread":id,"internal_only":{"type":"boolean"},"text":text,"in_secs":id,"labels":labels}), vec!["text","in_secs"]),
        ("schedules", "List your pending posts and private reminders. Fired private reminders remain until cancel_schedule dismisses them.", json!({"offset":offset,"limit":limit}), vec![]),
        ("cancel_schedule", "Cancel one of your pending posts.", json!({"id":id}), vec!["id"]),
        ("sprints_list", "List visible sprints, optionally for one board.", json!({"board":channel,"offset":offset,"limit":limit}), vec![]),
        ("sprints_create", "Create a sprint on a board.", json!({"board":channel,"title":text,"description":{"type":"string","maxLength":16384}}), vec!["board","title"]),
        ("tasks_set_sprint", "Set a top-level task's sprint; omit sprint to unassign.", json!({"id":id,"sprint":id}), vec!["id"]),
        ("sprints_finish", "Finish a sprint. Set allow_unfinished to leave unfinished tasks unchanged.", json!({"id":id,"allow_unfinished":{"type":"boolean"}}), vec!["id"]),
    ].into_iter().map(|(name,description,properties,required)| json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})).collect()
}

pub(super) fn matches_event(kind: &EventKind, args: &Value) -> bool {
    let thread = args["thread"].as_u64();
    let filter = args["channel"].as_str();
    let (channel, thread_matches) = match kind {
        EventKind::OrganizationChanged { board } => return thread.is_none() && filter.is_none_or(|c| c.split('/').next() == Some(board)),
        EventKind::Message { message } => (&message.channel, thread.is_none_or(|id| message.id == id || message.parent == Some(id))),
        EventKind::Reaction { id, channel, .. } | EventKind::ThreadDeleted { id, channel } => (channel, thread.is_none_or(|n| n == *id)),
        EventKind::ChannelCreated { channel } | EventKind::ChannelDeleted { channel } | EventKind::Scheduled { channel, .. } | EventKind::ScheduleCanceled { channel, .. } | EventKind::SchedulePublished { channel, .. } => (channel, thread.is_none()),
    };
    thread_matches && filter.is_none_or(|c| c == channel)
}

fn list<T: serde::Serialize>(items: impl Iterator<Item=T>, args: &Value, key: &str) -> Result<Value, &'static str> {
    let offset = args["offset"].as_u64().unwrap_or(0);
    let limit = args["limit"].as_u64().unwrap_or(50) as usize;
    let values = items.skip(usize::try_from(offset).unwrap_or(usize::MAX)).take(limit+1).map(|item| json!(item)).collect();
    page(values, limit, json!({"next_offset":offset}), key, "next_offset", |_, i| offset+i as u64+1)
}

pub(super) fn call(c: &Client, name: &str, a: &Value) -> Option<Result<Value, &'static str>> {
    let id = a["id"].as_u64();
    let string = |key: &str| a[key].as_str().unwrap_or_default().to_owned();
    let labels = || serde_json::from_value(a.get("labels").cloned().unwrap_or(json!({}))).map_err(|_| "invalid_arguments");
    let action = match name {
        "sprints_list" => return Some(c.organization().map_err(upstream_error).and_then(|org| list(org.sprints.into_values().filter(|s| a["board"].as_str().is_none_or(|b| b == s.board)), a, "sprints"))),
        "sprints_create" => Action::CreateSprint { board: string("board"), title: string("title"), description: string("description") },
        "tasks_set_sprint" => Action::SetTaskSprint { id: id?, sprint: a["sprint"].as_u64() },
        "sprints_finish" => Action::FinishSprint { id: id?, allow_unfinished: a["allow_unfinished"] == true },
        "channel_create" => Action::CreateChannel { channel: string("channel") },
        "edit" => Action::Edit { id: id?, text: string("text") },
        "labels" => Action::SetLabels { id: id?, labels: match labels() { Ok(v) => v, Err(e) => return Some(Err(e)) } },
        "react" => Action::React { id: id?, reaction: string("reaction"), remove: a["remove"] == true },
        "reactions" => return Some(c.reactions().map_err(upstream_error).and_then(|v| list(v.into_iter().filter(|r| id.is_none_or(|n| n == r.id)), a, name))),
        "attachments" => return Some(c.attachments(id?).map_err(upstream_error).and_then(|v| list(v.into_iter(), a, name))),
        "schedule" => {
            let publish_at = SystemTime::now().duration_since(UNIX_EPOCH).ok().and_then(|d| d.as_secs().checked_add(a["in_secs"].as_u64()?));
            let Some(publish_at) = publish_at.filter(|t| *t <= i64::MAX as u64) else { return Some(Err("invalid_arguments")); };
            let labels = match labels() { Ok(v) => v, Err(e) => return Some(Err(e)) };
            match (a["channel"].as_str(),a["thread"].as_u64()) {
                (Some(channel),None) if a.get("internal_only").is_none() => Action::Schedule { channel:channel.into(),text:string("text"),labels,publish_at },
                (None,Some(thread)) => Action::ScheduleFollowup { thread,text:string("text"),labels,publish_at,internal_only:a["internal_only"] == true },
                _ => return Some(Err("invalid_arguments")),
            }
        }
        "schedules" => return Some(c.schedules().map_err(upstream_error).and_then(|v| list(v.into_iter(), a, name))),
        "cancel_schedule" => Action::CancelSchedule { id: id? },
        _ => return None,
    };
    Some(c.action(&action).map(|v| json!(v)).map_err(upstream_error))
}