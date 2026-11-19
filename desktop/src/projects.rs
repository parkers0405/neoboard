use neochat::protocol::Snapshot;
use std::collections::BTreeSet;

pub fn available(board: &Snapshot, invited: bool) -> BTreeSet<&str> {
    let has_general = invited || board.messages.iter().any(|m| m.channel == "general");
    board.channels.iter().filter(|c| c.as_str() != "general" || has_general)
        .filter_map(|c| c.split('/').next()).collect()
}

pub fn contains(project: &str, channel: &str) -> bool {
    channel.split('/').next() == Some(project)
}

pub fn channel(project: &str, name: &str) -> String {
    if contains(project,name) {name.into()} else {format!("{project}/{name}")}
}