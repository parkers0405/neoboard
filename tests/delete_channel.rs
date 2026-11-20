use neochat::{board::{now, random_token, router}, protocol::*, Board};
use neochat::storage::Connection;

const CHANNEL: &str = "team/work";
fn delete(channel: &str) -> Action { Action::DeleteChannel { channel: channel.into() } }
fn task(channel: &str, parent: Option<u64>, global: bool) -> Action {
    Action::PostTask { channel: channel.into(), text: "task".into(), labels: Default::default(),
        needs: parent.into_iter().collect(), assignee: None, area: None, parent, global }
}
fn invite(board: &Board, admin: &str, actor: &str, channels: &[&str]) -> String {
    board.create_invite(admin, InviteRequest { actor: actor.into(), expires_at: None,
        channels: channels.iter().map(|c| c.to_string()).collect() }).unwrap().token
}

#[test]
fn atomic_cleanup_preserves_other_channels_and_board_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.sqlite");
    let (host, admin) = (random_token(), random_token());
    let board = Board::open(&path, &host, &admin).unwrap();
    let act = |a| board.action(&host, None, None, a).unwrap();
    let root = act(task(CHANNEL, None, false)).id.unwrap();
    act(task(CHANNEL, Some(root), false));
    act(Action::Block { id: root, text: "blocked".into() });
    let reply = act(Action::Reply { thread: root, text: "reply".into() }).id.unwrap();
    let file = board.attach(&host, reply, "file", vec![1]).unwrap();
    act(Action::React { id: reply, reaction: "ok".into(), remove: false });
    let schedule = |c: &str| Action::Schedule { channel: c.into(), text: "later".into(),
        labels: Default::default(), publish_at: now()+3600 };
    act(schedule(CHANNEL));
    let saved = act(schedule("team/work/child")).id.unwrap();
    let child = act(task("team/work/child", None, false)).id.unwrap();
    let global = act(task(CHANNEL, None, true)).id.unwrap();
    act(Action::AcknowledgeTask { id: global, done: true });

    let before = board.snapshot(&host).unwrap();
    let events = board.events(&host, 0).unwrap();
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER fail_event BEFORE INSERT ON events WHEN json_extract(NEW.body,'$.type')='channel_deleted' BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert_eq!(board.action(&host, None, Some("once"), delete(CHANNEL)).unwrap_err().0, 500);
    assert_eq!(board.snapshot(&host).unwrap(), before);
    assert_eq!(board.events(&host, 0).unwrap(), events);
    assert_eq!(board.download_attachment(&host, file.id).unwrap(), vec![1]);
    assert_eq!(board.schedules(&host).unwrap().len(), 2);
    db.execute_batch("DROP TRIGGER fail_event").unwrap();
    let outcome = board.action(&host, None, Some("once"), delete(CHANNEL)).unwrap();
    drop(board);
    let board = Board::open(&path, &host, &admin).unwrap();
    assert_eq!(board.action(&host, None, Some("once"), delete(CHANNEL)).unwrap(), outcome);
    let after = board.snapshot(&host).unwrap();
    assert!(!after.channels.iter().any(|c| c == CHANNEL));
    assert_eq!(after.messages.iter().map(|m| m.id).collect::<Vec<_>>(), [child, global]);
    assert_eq!(board.schedules(&host).unwrap().iter().map(|s| s.id).collect::<Vec<_>>(), [saved]);
    assert_eq!(board.download_attachment(&host, file.id).unwrap_err().0, 404);
    assert!(board.reactions(&host).unwrap().is_empty());
    let surviving = board.events(&host, 0).unwrap();
    assert_eq!(surviving.last().unwrap().kind, EventKind::ChannelDeleted { channel: CHANNEL.into() });
    let scoped = invite(&board, &admin, "reader", &[CHANNEL]);
    for event in board.events(&scoped, 0).unwrap() {
        match event.kind {
            EventKind::Message { message } => assert_eq!(message.id, global),
            EventKind::ChannelDeleted { channel } => assert_eq!(channel, CHANNEL),
            stale => panic!("unexpected scoped replay after deletion: {stale:?}"),
        }
    }
    assert!(db.prepare("PRAGMA foreign_key_check").unwrap().query([]).unwrap().next().unwrap().is_none());
    board.action(&host, None, None, Action::CreateChannel { channel: CHANNEL.into() }).unwrap();
    assert_eq!(board.action(&host, None, Some("once"), delete(CHANNEL)).unwrap(), outcome);
    assert!(board.snapshot(&host).unwrap().channels.iter().any(|c| c == CHANNEL));
}

#[tokio::test]
async fn host_only_deletion_reaches_scoped_long_poll() {
    let (host, admin) = (random_token(), random_token());
    let board = Board::open(":memory:", &host, &admin).unwrap();
    let reader = invite(&board, &admin, "reader", &[CHANNEL]);
    let unrestricted = invite(&board, &admin, "guest", &[]);
    let hidden = invite(&board, &admin, "other", &["team/work-extra"]);
    let before = board.action(&reader, None, None, Action::CreateChannel { channel: CHANNEL.into() }).unwrap().cursor;
    for token in [&reader, &unrestricted, &hidden, &admin] {
        assert_eq!(board.action(token, Some("local"), None, delete(CHANNEL)).unwrap_err().0, if token == &admin {401} else {403});
    }
    for name in ["general", "team", "team/general"] {
        assert_eq!(board.action(&host, None, None, delete(name)).unwrap_err().0, 409);
    }
    assert_eq!(board.action(&host, None, None, delete("team/missing")).unwrap_err().0, 404);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = router(board.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let poll = client.get(format!("{base}/events?after={before}&wait_ms=2000")).bearer_auth(reader);
    let poll = tokio::spawn(async move { poll.send().await.unwrap().error_for_status().unwrap().json::<Vec<Event>>().await.unwrap() });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let deleted: Outcome = client.post(format!("{base}/actions")).bearer_auth(host)
        .json(&serde_json::json!({"type":"delete_channel", "channel":CHANNEL})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let events = poll.await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].cursor, deleted.cursor);
    assert_eq!(events[0].kind, EventKind::ChannelDeleted { channel: CHANNEL.into() });
    assert!(board.events(&hidden, before).unwrap().is_empty());
    server.abort();
}