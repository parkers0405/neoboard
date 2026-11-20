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
fn area_owner_migrates_persists_and_respects_board_boundaries() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("legacy.sqlite");
    // Independent writer: this must be a closed legacy SQLite file, not a file
    // created by the new engine with a legacy-looking schema or extension.
    let fixture = std::process::Command::new("python3").args(["-c", r#"
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
assert db.execute('PRAGMA journal_mode=WAL').fetchone() == ('wal',)
db.executescript("CREATE TABLE areas(board TEXT NOT NULL, name TEXT NOT NULL, PRIMARY KEY(board,name)); INSERT INTO areas VALUES ('work','UI'),('other','UI');")
db.close()
"#]).arg(&path).output().expect("fixture generation requires Python 3 with sqlite3");
    assert!(fixture.status.success(), "{}", String::from_utf8_lossy(&fixture.stderr));
    let board = Board::open(&path, TOKEN, ADMIN).unwrap();
    let legacy: Organization = serde_json::from_str(r#"{"areas":{"work":["UI"]},"tasks":{}}"#).unwrap();
    assert!(legacy.area_owners.is_empty());
    assert!(legacy.sprints.is_empty() && legacy.task_sprints.is_empty());
    let migrated = board.organization(TOKEN).unwrap();
    assert!(migrated.sprints.is_empty() && migrated.task_sprints.is_empty());
    act(&board, TOKEN, sprint("work"));
    assert!(board.organization(TOKEN).unwrap().area_owners.is_empty());
    let member = invite(&board,"designer","work/private");
    let peer = invite(&board,"peer","work/public");
    let outsider = invite(&board,"outsider","other/*");
    let set = |b: &str, name: &str, owner: Option<&str>| Action::SetAreaOwner { board:b.into(), name:name.into(), owner:owner.map(str::to_owned) };
    denied(&board,&outsider.token,set("work","UI",None),StatusCode::FORBIDDEN);
    for actor in ["outsider","unknown","Display Name"] {
        denied(&board,&peer.token,set("work","UI",Some(actor)),StatusCode::BAD_REQUEST);
    }
    denied(&board,&peer.token,set("work","missing",None),StatusCode::BAD_REQUEST);
    act(&board,TOKEN,set("other","UI",Some("local")));
    act(&board,&peer.token,task("work/public",Some("UI"),None,false));
    let before = board.snapshot(TOKEN).unwrap();
    let updated = act(&board,&peer.token,set("work","UI",Some("designer")));
    assert_eq!(act(&board,&peer.token,set("work","UI",Some("designer"))).cursor,updated.cursor);
    assert_eq!(board.snapshot(TOKEN).unwrap().messages,before.messages);
    assert_eq!(board.events(&member.token,before.cursor).unwrap()[0].kind,EventKind::OrganizationChanged { board:"work".into() });
    assert!(board.events(&outsider.token,before.cursor).unwrap().is_empty());
    drop(board);
    let board = Board::open(&path,TOKEN,ADMIN).unwrap();
    let visible = board.organization(&peer.token).unwrap();
    assert_eq!(visible.area_owners.len(),1);
    assert_eq!(visible.area_owners["work"]["UI"],"designer");
    assert_eq!(board.owned_areas(&peer.token,"designer").unwrap()["work"],["UI"]);
    assert!(board.owned_areas(&outsider.token,"designer").unwrap().is_empty());
    act(&board,TOKEN,set("work","UI",Some("local")));
    board.revoke_invite(ADMIN,member.id).unwrap();
    denied(&board,TOKEN,set("work","UI",Some("designer")),StatusCode::BAD_REQUEST);
    let cleared = act(&board,&peer.token,set("work","UI",None));
    assert_eq!(act(&board,&peer.token,set("work","UI",None)).cursor,cleared.cursor);
    assert!(board.organization(&peer.token).unwrap().area_owners.is_empty());
    assert_eq!(board.owned_areas(TOKEN,"local").unwrap(),[("other".to_owned(),vec!["UI".to_owned()])].into());
}

#[test]
fn completion_requires_every_subtask_to_succeed_and_seals_placement() {
    let board = Board::open(":memory:", TOKEN, ADMIN).unwrap();
    let post = |parent| act(&board, TOKEN, task("work/tasks", None, parent, false)).id.unwrap();
    let claim = |id| act(&board, TOKEN, Action::Claim { id, lease_secs: 600 });
    let complete = |id| Action::Complete { id, text: "done".into() };
    let parent = post(None);
    let first = post(Some(parent));
    let second = post(Some(parent));
    claim(parent);
    claim(first);
    act(&board, TOKEN, complete(first));
    denied(&board, TOKEN, complete(parent), StatusCode::CONFLICT);
    claim(second);
    act(&board, TOKEN, complete(second));
    act(&board, TOKEN, complete(parent));

    let other = post(None);
    let moving = post(Some(other));
    denied(&board, TOKEN, task("work/tasks", None, Some(parent), false), StatusCode::CONFLICT);
    denied(&board, TOKEN, update(moving, None, Some(parent)), StatusCode::CONFLICT);
}

fn sprint(board: &str) -> Action {
    Action::CreateSprint { board: board.into(), title: "Release".into(), description: "Ship it".into() }
}

fn sprint_task(channel: &str, sprint: u64) -> Action {
    Action::PostSprintTask { channel: channel.into(), text: "implement".into(), labels: Default::default(), needs: vec![], assignee: None, sprint }
}

#[test]
fn sprint_catalog_and_task_events_have_different_audiences() {
    let board = Board::open(":memory:", TOKEN, ADMIN).unwrap();
    let private = invite(&board, "private", "app/private").token;
    let sibling = invite(&board, "sibling", "app/public").token;
    let outside = invite(&board, "outside", "app-other/*").token;
    let start = board.snapshot(TOKEN).unwrap().cursor;
    let created = act(&board, &private, sprint("app"));
    let id = created.id.unwrap();
    assert_eq!(board.events(&sibling,start).unwrap()[0].kind, EventKind::OrganizationChanged { board:"app".into() });
    let posted = act(&board, &private, sprint_task("app/private", id)).id.unwrap();
    let member = |sprint| Action::SetTaskSprint { id:posted, sprint };
    assert_eq!(board.organization(&private).unwrap().task_sprints[&posted], id);
    assert!(board.organization(&sibling).unwrap().task_sprints.is_empty());
    assert!(board.organization(&outside).unwrap().sprints.is_empty());
    denied(&board,&sibling,member(None),StatusCode::NOT_FOUND);
    denied(&board,TOKEN,member(None),StatusCode::FORBIDDEN);
    denied(&board,&outside,Action::FinishSprint{id,allow_unfinished:true},StatusCode::NOT_FOUND);
    denied(&board,&sibling,Action::FinishSprint{id,allow_unfinished:false},StatusCode::FORBIDDEN);
    denied(&board,&outside,sprint("app"),StatusCode::FORBIDDEN);
    let other = act(&board,&outside,sprint("app-other")).id.unwrap();
    denied(&board,&private,member(Some(other)),StatusCode::BAD_REQUEST);
    denied(&board,TOKEN,sprint_task("app/new-channel",other),StatusCode::BAD_REQUEST);
    let root = act(&board,&private,task("app/private",None,None,false)).id.unwrap();
    denied(&board,&private,update(posted,None,Some(root)),StatusCode::BAD_REQUEST);
    let before = board.snapshot(TOKEN).unwrap().cursor;
    act(&board,&private,member(None));
    assert!(matches!(&board.events(&private,before).unwrap()[0].kind,EventKind::Message{message} if message.id==posted));
    act(&board,&private,update(posted,None,Some(root)));
    denied(&board,&private,member(Some(id)),StatusCode::BAD_REQUEST);
    assert!(board.events(&sibling,created.cursor).unwrap().is_empty());
    assert!(!board.events(&outside,start).unwrap().iter().any(|e| e.actor=="private"));
}

#[test]
fn sprint_progress_is_top_level_persistent_and_finish_does_not_change_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sprints.db");
    let board = Board::open(&path,TOKEN,ADMIN).unwrap();
    let manager = invite(&board,"lead","app/*").token;
    let create = |b: &Board| act(b,&manager,sprint("app")).id.unwrap();
    let id = create(&board);
    let root = act(&board,TOKEN,task("app/work",None,None,false)).id.unwrap();
    act(&board,TOKEN,Action::SetTaskSprint{id:root,sprint:Some(id)});
    let child = act(&board,TOKEN,task("app/work",None,Some(root),false)).id.unwrap();
    let finish = |id,allow_unfinished| Action::FinishSprint{id,allow_unfinished};
    denied(&board,&manager,finish(id,false),StatusCode::CONFLICT);
    act(&board,TOKEN,Action::Claim{id:root,lease_secs:600});
    act(&board,TOKEN,Action::CompleteAnyway{id:root,text:"accepted".into()});
    let done = act(&board,&manager,finish(id,false));
    assert_eq!(act(&board,&manager,finish(id,false)),done);
    denied(&board,TOKEN,sprint_task("app/work",id),StatusCode::CONFLICT);
    let next = create(&board);
    let pending = act(&board,TOKEN,sprint_task("app/work",next)).id.unwrap();
    act(&board,TOKEN,Action::Cancel{id:pending});
    denied(&board,&manager,finish(next,false),StatusCode::CONFLICT);
    let before = board.snapshot(TOKEN).unwrap();
    act(&board,&manager,finish(next,true));
    assert_eq!(board.snapshot(TOKEN).unwrap().messages,before.messages);
    drop(board);
    let board = Board::open(&path,TOKEN,ADMIN).unwrap();
    let org = board.organization(&manager).unwrap();
    assert_eq!(org.task_sprints,[(root,id),(pending,next)].into());
    assert_eq!(org.tasks[&child].parent,Some(root));
    assert!(org.sprints.values().all(|s| s.finished));
    assert_eq!(org.sprints[&id].description,"Ship it");
    act(&board,TOKEN,Action::DeleteThread{id:pending});
    assert!(!board.organization(&manager).unwrap().task_sprints.contains_key(&pending));
}

#[test]
fn explicit_completion_settles_only_the_owned_parent_lease() {
    let board = Board::open(":memory:", TOKEN, ADMIN).unwrap();
    let parent = act(&board, TOKEN, task("work/tasks", None, None, false)).id.unwrap();
    let child = act(&board, TOKEN, task("work/tasks", None, Some(parent), false)).id.unwrap();
    let peer = invite(&board, "peer", "work/tasks");
    act(&board, &peer.token, Action::Claim { id: child, lease_secs: 600 });
    act(&board, TOKEN, Action::Claim { id: parent, lease_secs: 600 });
    let message = |id| board.snapshot(TOKEN).unwrap().messages.into_iter().find(|m| m.id == id).unwrap();
    let before = message(child);
    let override_action = || Action::CompleteAnyway { id: parent, text: "accepted".into() };
    assert_eq!(override_action().grant(), Some(Grant::Complete));
    denied(&board, TOKEN, Action::Complete { id: parent, text: "accepted".into() }, StatusCode::CONFLICT);
    denied(&board, &peer.token, override_action(), StatusCode::CONFLICT);
    act(&board, TOKEN, override_action());
    let t = message(parent).task.unwrap();
    assert_eq!(t.state, TaskState::Completed);
    assert_eq!((t.owner, t.expires_at), (None, None));
    assert_eq!(t.finished_by.as_deref(), Some("local"));
    assert_eq!(t.result.as_deref(), Some("accepted"));
    assert_eq!(message(child), before);
    assert_eq!(board.organization(TOKEN).unwrap().tasks[&child].parent, Some(parent));
    act(&board, &peer.token, update(child, None, Some(parent)));
    act(&board, &peer.token, update(child, None, None));
    denied(&board, &peer.token, update(child, None, Some(parent)), StatusCode::CONFLICT);
}

#[test]
fn failed_and_canceled_subtasks_prevent_success_but_not_failure() {
    let board = Board::open(":memory:", TOKEN, ADMIN).unwrap();
    for cancel in [false, true] {
        let parent = act(&board, TOKEN, task("work/tasks", None, None, false)).id.unwrap();
        let id = act(&board, TOKEN, task("work/tasks", None, Some(parent), false)).id.unwrap();
        if cancel {
            act(&board, TOKEN, Action::Cancel { id });
        } else {
            act(&board, TOKEN, Action::Claim { id, lease_secs: 600 });
            act(&board, TOKEN, Action::Fail { id, text: String::new() });
        }
        act(&board, TOKEN, Action::Claim { id: parent, lease_secs: 600 });
        denied(&board, TOKEN, Action::Complete { id: parent, text: "done".into() }, StatusCode::CONFLICT);
        act(&board, TOKEN, Action::Fail { id: parent, text: String::new() });
    }
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