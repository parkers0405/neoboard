use axum::http::StatusCode;
use neochat::{protocol::*, Board};

const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ADMIN: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn invite(board: &Board, actor: &str, channel: &str) -> Invite {
    board.create_invite(ADMIN, InviteRequest {
        actor: actor.into(), channels: vec![channel.into()], expires_at: None,
    }).unwrap()
}

fn act(board: &Board, token: &str, action: Action) -> Outcome {
    board.action(token, None, None, action).unwrap()
}

fn area(board: &str, name: &str) -> Action {
    Action::CreateArea { board: board.into(), name: name.into() }
}

fn task(channel: &str, area: Option<&str>, parent: Option<u64>, global: bool) -> Action {
    Action::PostTask {
        channel: channel.into(), text: "task".into(), labels: Default::default(),
        needs: vec![], assignee: None, area: area.map(str::to_owned), parent, global,
    }
}

fn update(id: u64, area: Option<&str>, parent: Option<u64>) -> Action {
    Action::UpdateTask {
        id, text: "updated".into(), labels: Default::default(), needs: vec![],
        assignee: None, area: area.map(str::to_owned), parent,
    }
}

fn denied(board: &Board, token: &str, action: Action, status: StatusCode) {
    let before = board.snapshot(TOKEN).unwrap();
    let org = board.organization(TOKEN).unwrap();
    assert_eq!(board.action(token, None, None, action).unwrap_err().0, status);
    assert_eq!(board.snapshot(TOKEN).unwrap(), before);
    assert_eq!(board.organization(TOKEN).unwrap(), org);
}

#[test]
fn areas_and_one_level_same_channel_subtasks_preserve_legacy_labels() {
    let board = Board::open(":memory:", TOKEN, ADMIN).unwrap();
    let alice = invite(&board, "alice", "work/tasks");
    let peer = invite(&board, "peer", "work/private");
    let outside = invite(&board, "outside", "work-other/*");
    let before = board.snapshot(TOKEN).unwrap().cursor;
    let created = act(&board, &alice.token, area("work", "Desktop UI"));
    assert_eq!(act(&board, &alice.token, area("work", "Desktop UI")).cursor, created.cursor);
    assert_eq!(board.events(&peer.token, before).unwrap()[0].kind, EventKind::OrganizationChanged { board: "work".into() });
    assert!(board.events(&outside.token, before).unwrap().is_empty());
    act(&board, TOKEN, area("work", "Backend"));
    act(&board, TOKEN, area("work-other", "Other board"));
    denied(&board, &alice.token, area("work-other", "Hidden"), StatusCode::FORBIDDEN);
    denied(&board, TOKEN, area("work/tasks", "Invalid"), StatusCode::BAD_REQUEST);
    denied(&board, TOKEN, area("work", "   "), StatusCode::BAD_REQUEST);
    denied(&board, TOKEN, task("work/tasks", Some("Other board"), None, false), StatusCode::BAD_REQUEST);

    let root = act(&board, &alice.token, task("work/tasks", Some("Desktop UI"), None, false)).id.unwrap();
    let child = act(&board, &alice.token, task("work/tasks", Some("Backend"), Some(root), false)).id.unwrap();
    let standalone = act(&board, &alice.token, task("work/tasks", None, None, false)).id.unwrap();
    let legacy = act(&board, &alice.token, Action::Post {
        channel: "work/tasks".into(), text: "legacy".into(), labels: [("area".into(), "Unregistered".into())].into(),
        task: true, needs: vec![], assignee: None,
    }).id.unwrap();
    let org = board.organization(&alice.token).unwrap();
    assert_eq!(org.areas["work"], ["Backend", "Desktop UI"]);
    assert_eq!(org.areas.len(), 1);
    assert_eq!(org.tasks[&child].parent, Some(root));
    assert_eq!(org.tasks[&child].area.as_deref(), Some("Backend"));
    assert_eq!(org.tasks[&legacy].area.as_deref(), Some("Unregistered"));
    assert_eq!(board.snapshot(&alice.token).unwrap().messages.iter().find(|m| m.id == child).unwrap().parent, None);
    assert!(board.organization(&peer.token).unwrap().tasks.is_empty());

    denied(&board, &peer.token, task("work/private", None, Some(root), false), StatusCode::NOT_FOUND);
    denied(&board, TOKEN, task("work/private", None, Some(root), false), StatusCode::BAD_REQUEST);
    denied(&board, &alice.token, task("work/tasks", None, Some(child), false), StatusCode::BAD_REQUEST);
    denied(&board, &alice.token, update(root, None, Some(child)), StatusCode::BAD_REQUEST);
    denied(&board, &alice.token, update(root, None, Some(standalone)), StatusCode::BAD_REQUEST);
    denied(&board, &alice.token, update(root, None, Some(root)), StatusCode::BAD_REQUEST);
    denied(&board, TOKEN, update(child, None, None), StatusCode::FORBIDDEN);
    denied(&board, &alice.token, Action::DeleteThread { id: root }, StatusCode::CONFLICT);

    // Legacy edits preserve structured placement, and structured edits can clear it.
    act(&board, &alice.token, Action::EditTask { id: child, text: "legacy edit".into(), labels: Default::default(), needs: vec![], assignee: None });
    assert_eq!(board.organization(TOKEN).unwrap().tasks[&child].parent, Some(root));
    let before_update = board.snapshot(TOKEN).unwrap().cursor;
    act(&board, &alice.token, update(child, None, None));
    assert_eq!(board.organization(TOKEN).unwrap().tasks[&child], TaskPlacement::default());
    assert!(matches!(&board.events(&alice.token, before_update).unwrap()[0].kind, EventKind::Message { message } if message.id == child));
    act(&board, &alice.token, Action::DeleteThread { id: root });
    assert!(!board.organization(TOKEN).unwrap().tasks.contains_key(&root));
}

#[test]
fn board_wide_tasks_keep_independent_actor_acknowledgements_without_widening_channel_access() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let board = Board::open(&path, TOKEN, ADMIN).unwrap();
    let alice = invite(&board, "alice", "work/tasks");
    let bob = invite(&board, "bob", "work/ops");
    let outsider = invite(&board, "outsider", "work-other/*");
    let root_post = act(&board, TOKEN, task("work", None, None, false)).id.unwrap();
    act(&board, &alice.token, task("work/tasks", None, None, false));
    denied(&board, &outsider.token, task("work", None, None, true), StatusCode::FORBIDDEN);
    let start = board.snapshot(&bob.token).unwrap().cursor;
    let id = act(&board, &alice.token, task("work/tasks", None, None, true)).id.unwrap();
    let snapshot = board.snapshot(&bob.token).unwrap();
    assert!(snapshot.channels.is_empty());
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.messages[0].channel, "work");
    assert_eq!(snapshot.messages[0].id, id);
    assert!(matches!(&board.events(&bob.token, start).unwrap()[0].kind, EventKind::Message { message } if message.id == id));
    assert!(board.events(&outsider.token, start).unwrap().is_empty());
    assert!(board.organization(&outsider.token).unwrap().tasks.is_empty());
    denied(&board, &bob.token, Action::AcknowledgeTask { id: root_post, done: true }, StatusCode::NOT_FOUND);
    denied(&board, TOKEN, Action::AcknowledgeTask { id: root_post, done: true }, StatusCode::BAD_REQUEST);
    denied(&board, &outsider.token, Action::AcknowledgeTask { id, done: true }, StatusCode::NOT_FOUND);
    denied(&board, &bob.token, Action::Claim { id, lease_secs: 30 }, StatusCode::CONFLICT);
    denied(&board, &alice.token, Action::Assign { id, assignee: Some("bob".into()) }, StatusCode::BAD_REQUEST);
    denied(&board, &alice.token, update(id, Some("Area"), None), StatusCode::BAD_REQUEST);
    denied(&board, &alice.token, task("work", None, Some(id), true), StatusCode::BAD_REQUEST);
    denied(&board, TOKEN, task("work", None, Some(id), false), StatusCode::BAD_REQUEST);
    denied(&board, TOKEN, Action::EditTask { id: root_post, text: "dependency".into(), labels: Default::default(), needs: vec![id], assignee: None }, StatusCode::BAD_REQUEST);
    denied(&board, &alice.token, Action::SetLabels { id, labels: [("area".into(), "Area".into())].into() }, StatusCode::BAD_REQUEST);

    let ack = |done| Action::AcknowledgeTask { id, done };
    let first = board.action(&alice.token, Some("phone"), None, ack(true)).unwrap();
    let repeat = board.action(&alice.token, Some("other-device"), None, ack(true)).unwrap();
    assert_eq!(first.cursor, repeat.cursor);
    let second = act(&board, &bob.token, ack(true));
    let events = board.events(&alice.token, first.cursor).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].cursor, second.cursor);
    assert_eq!(events[0].actor, "bob");
    assert!(matches!(&events[0].kind, EventKind::Message { message } if message.id == id));
    assert_eq!(board.organization(TOKEN).unwrap().tasks[&id].done_by, ["alice", "bob"]);
    act(&board, &alice.token, ack(false));
    act(&board, &alice.token, update(id, None, None));
    let reply = act(&board, &bob.token, Action::Reply { thread: id, text: "global reply".into() }).id.unwrap();
    act(&board, &bob.token, Action::React { id: reply, reaction: "yes".into(), remove: false });
    assert_eq!(board.reactions(&alice.token).unwrap()[0].id, reply);
    assert!(board.reactions(&outsider.token).unwrap().is_empty());

    drop(board);
    let board = Board::open(path, TOKEN, ADMIN).unwrap();
    assert_eq!(board.organization(&bob.token).unwrap().tasks[&id].done_by, ["bob"]);
    let snapshot = board.snapshot(&bob.token).unwrap();
    let global = &snapshot.messages[0];
    assert_eq!(global.task.as_ref().unwrap().state, TaskState::Ready);
    assert_eq!(global.task.as_ref().unwrap().finished_by, None);
    act(&board, &alice.token, Action::DeleteThread { id });
    assert!(board.organization(&bob.token).unwrap().tasks.is_empty());
    assert!(board.snapshot(&bob.token).unwrap().messages.is_empty());
    assert!(matches!(&board.events(&bob.token, snapshot.cursor).unwrap()[0].kind, EventKind::ThreadDeleted { id: deleted, .. } if *deleted == id));
}