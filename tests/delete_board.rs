use neochat::{board::{now, random_token}, client::Client, protocol::*, router, storage::Connection, Board};
use std::time::Duration;

const PROJECT: &str = "a_b";
const CHILD: &str = "a_b/deep/task";
const OTHER: &str = "axb/deep/task";

fn post(channel: &str, text: &str) -> Action {
    Action::Post { channel: channel.into(), text: text.into(), task: false, needs: vec![], assignee: None, labels: Default::default() }
}
fn invite(actor: &str, scopes: &[&str]) -> InviteRequest {
    InviteRequest { actor: actor.into(), channels: scopes.iter().map(|c| c.to_string()).collect(), expires_at: None }
}

#[test]
fn root_only_metadata_only_and_last_channel_history_are_deletable() {
    let (host,admin) = (random_token(),random_token());
    let board = Board::open(":memory:",&host,&admin).unwrap();
    let create = |a| board.action(&host,None,None,a).unwrap();
    create(post("bare","no descendants"));
    create(Action::CreateArea {board:"metadata".into(),name:"only".into()});
    create(post("historical/temp","only channel"));
    create(Action::DeleteChannel {channel:"historical/temp".into()});
    for p in ["bare","metadata","historical"] {
        let writer = board.create_invite(&admin,invite(p,&[&format!("{p}/*")])).unwrap();
        board.delete_board(&admin,p).unwrap();
        assert_eq!(board.action(&writer.token,None,None,post(p,"stale")).unwrap_err().0,401);
        assert_eq!(board.delete_board(&admin,p).unwrap_err().0,404);
    }
    assert_eq!(board.snapshot(&host).unwrap().channels,["general"]);
    assert_eq!(board.organization(&host).unwrap(),Organization::default());
}

#[test]
fn atomic_namespace_cleanup_including_replay_and_scheduled_followups() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let (host, admin) = (random_token(), random_token());
    let board = Board::open(&path, &host, &admin).unwrap();
    let act = |action| board.action(&host, None, None, action).unwrap();
    let keyed = |key, action| board.action(&host, None, Some(key), action).unwrap();
    for c in [OTHER, "a_b2/deep", "A_b/deep"] { act(post(c,"neighbor")); }
    let neighbor = keyed("neighbor",post(OTHER,"keep"));
    let root = keyed("root",post(PROJECT,"scrub root")).id.unwrap();
    let task = |parent| Action::PostTask { channel: CHILD.into(), text: "scrub task".into(),
        labels: Default::default(), needs: vec![], assignee: None, area: None, parent, global: false };
    let id = act(task(None)).id.unwrap();
    act(task(Some(id)));
    act(Action::Block {id,text:"scrub block".into()});
    let sprint = act(Action::CreateSprint {board:PROJECT.into(),title:"scrub sprint".into(),description:"gone".into()}).id;
    act(Action::SetTaskSprint {id,sprint});
    act(Action::CreateArea {board:PROJECT.into(),name:"area".into()});
    act(Action::SetAreaOwner {board:PROJECT.into(),name:"area".into(),owner:Some("local".into())});
    let global = act(Action::PostTask {channel:CHILD.into(),text:"scrub global".into(),needs:vec![],labels:Default::default(),assignee:None,area:None,parent:None,global:true}).id.unwrap();
    act(Action::AcknowledgeTask {id:global,done:true});
    act(Action::React {id,reaction:"scrub".into(),remove:false});
    let file = board.attach(&host,id,"scrub.file",vec![42]).unwrap();
    let reply = Action::Reply {thread:root,text:"scrub reply".into()};
    keyed("reply",reply.clone());
    act(Action::Schedule {channel:OTHER.into(),text:"survives".into(),labels:Default::default(),publish_at:now()+1000});
    for internal_only in [true,false] {
        act(Action::ScheduleFollowup {thread:root,text:"scrub reminder".into(),labels:Default::default(),publish_at:now()+1000,internal_only});
    }
    let scoped = board.create_invite(&admin,invite("scoped", &["a_b/*"])).unwrap().token;
    let before = board.snapshot(&scoped).unwrap();
    let db = Connection::open(&path).unwrap();
    // Exercise old DBs that saved request plaintext; reinsert historic wire format.
    db.execute("UPDATE idempotency SET request=?1 WHERE key='reply'", [serde_json::to_string(&(None::<String>,&reply)).unwrap()]).unwrap();
    db.execute_batch("CREATE TRIGGER inject_failure BEFORE DELETE ON sprints BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert_eq!(board.delete_board(&admin,PROJECT).unwrap_err().0,500);
    assert_eq!(board.snapshot(&scoped).unwrap(),before);
    assert_eq!(board.download_attachment(&scoped,file.id).unwrap(),[42]);
    assert_eq!(board.schedules(&host).unwrap().len(),3);
    db.execute_batch("DROP TRIGGER inject_failure").unwrap();
    board.delete_board(&admin,PROJECT).unwrap();
    drop(board);
    let board = Board::open(&path,&host,&admin).unwrap();
    let snapshot = board.snapshot(&host).unwrap();
    assert!(snapshot.cursor>before.cursor);
    assert_eq!(snapshot.channels,["A_b/deep","a_b2/deep",OTHER,"general"]);
    assert_eq!(snapshot.messages.len(),4);
    assert_eq!(board.schedules(&host).unwrap().len(),1);
    assert_eq!(board.download_attachment(&host,file.id).unwrap_err().0,404);
    assert_eq!(board.organization(&host).unwrap(),Organization::default());
    assert_eq!(board.action(&host,None,Some("neighbor"),post(OTHER,"keep")).unwrap(),neighbor);
    for (key, a) in [("root",post(PROJECT,"scrub root")),("reply",reply)] {
        assert_eq!(board.action(&host,None,Some(key),a).unwrap_err().0,409);
    }
    assert_eq!(board.delete_board(&admin,PROJECT).unwrap_err().0,404);
    for table in ["reactions","task_blocks","task_placements","task_acknowledgements","attachments","schedule_threads","area_owners","task_sprints"] {
        assert_eq!(db.query_row(&format!("SELECT COUNT(*) FROM {table}"),[],|r|r.get::<_,u64>(0)).unwrap(),0,"{table}");
    }
    assert_eq!(db.query_row("SELECT count(*) FROM events WHERE instr(body,'scrub') OR channel='@board/a_b' OR channel='a_b' OR channel='a_b/deep/task'",[],|r|r.get::<_,u64>(0)).unwrap(),0);
    assert_eq!(db.query_row("SELECT count(*) FROM idempotency WHERE instr(request,'scrub')",[],|r|r.get::<_,u64>(0)).unwrap(),0);
    assert!(db.prepare("PRAGMA foreign_key_check").unwrap().query([]).unwrap().next().unwrap().is_none());
}

#[tokio::test]
async fn admin_http_revokes_access_across_recreation_and_refreshes_waiters() {
    let (host,admin) = (random_token(),random_token());
    let board = Board::open(":memory:",&host,&admin).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}",listener.local_addr().unwrap());
    let app = router(board.clone());
    let server = tokio::spawn(async move { axum::serve(listener,app).await.unwrap() });
    let http = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
    let password = |p| serde_json::json!({"project":PROJECT,"password":p});
    let init = |p| http.post(format!("{base}/v1/admin/init-board")).bearer_auth(&admin).json(&password(p));
    let join = |p| http.post(format!("{base}/v1/join/password")).json(&password(p));
    assert_eq!(init("old").send().await.unwrap().status(),204);
    let password_member: Invite = join("old").send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let shared = board.create_shared_invite(&admin,invite("shared", &["a_b/*"])).unwrap();
    let joined = board.join(&shared.token).unwrap();
    let exact = board.create_invite(&admin,invite("exact", &[CHILD])).unwrap();
    let mixed = board.create_shared_invite(&admin,invite("mixed", &[CHILD,OTHER])).unwrap();
    let mixed_joined = board.join(&mixed.token).unwrap();
    let wide = board.create_shared_invite(&admin,invite("wide", &[])).unwrap();
    let restricted: Invite = http.post(format!("{base}/v1/admin/restricted-invites")).bearer_auth(&admin)
        .json(&serde_json::json!({"invite":invite("write", &["a_b/*",OTHER]),"grants":["post"]})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let other = board.create_invite(&admin,invite("neighbor", &[OTHER])).unwrap();
    board.action(&mixed.token,None,None,post(CHILD,"private")).unwrap();
    board.action(&other.token,None,None,post(OTHER,"neighbor")).unwrap();
    let delete = |token| http.delete(format!("{base}/v1/admin/boards/{PROJECT}")).bearer_auth(token);
    for token in [&host,&shared.token,&wide.token,&joined.token,&password_member.token] {
        assert_eq!(delete(token).send().await.unwrap().status(),401);
    }
    for (p,status) in [("general",409),("bad*",400),("missing",404)] {
        assert_eq!(http.delete(format!("{base}/v1/admin/boards/{p}")).bearer_auth(&admin).send().await.unwrap().status(),status);
    }
    let before = board.snapshot(&mixed.token).unwrap().cursor;
    let poll = |token| { let req = http.get(format!("{base}/v1/events?after={before}&wait_ms=4000")).bearer_auth(token);
        tokio::spawn(async move { req.send().await.unwrap().status() }) };
    let revoked_poll = poll(&joined.token);
    let mixed_poll = poll(&mixed_joined.token);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let url=base.clone(); let secret=admin.clone();
    tokio::task::spawn_blocking(move || Client::new(&url,&secret,None).unwrap().delete_board(PROJECT)).await.unwrap().unwrap();
    let check_polls = async { assert_eq!(revoked_poll.await.unwrap(),401); assert_eq!(mixed_poll.await.unwrap(),409); };
    tokio::time::timeout(Duration::from_secs(2),check_polls).await.unwrap();

    // No old write credential may auto-create a deleted board, even before re-init.
    assert_eq!(board.action(&host,None,None,post(CHILD,"stale host composer")).unwrap_err().0,409);
    for (token,status) in [(&wide.token,403),(&mixed.token,403),(&restricted.token,403),(&shared.token,401)] {
        assert_eq!(board.action(token,None,None,post(CHILD,"resurrect")).unwrap_err().0,status);
    }
    assert_eq!(init("replacement").send().await.unwrap().status(),204);
    let fresh: Invite = join("replacement").send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let secret = board.action(&fresh.token,None,None,post(CHILD,"only new members")).unwrap().id.unwrap();
    let file = board.attach(&fresh.token,secret,"new",vec![7]).unwrap();
    assert_eq!(join("old").send().await.unwrap().status(),401);
    for token in [&shared.token,&joined.token,&exact.token,&password_member.token] {
        assert_eq!(board.snapshot(token).unwrap_err().0,401);
        assert_eq!(board.action(token,None,None,post(CHILD,"denied")).unwrap_err().0,401);
        assert_eq!(board.join(token).unwrap_err().0,401);
    }
    let rejoined = board.join(&mixed.token).unwrap();
    let narrowed = board.join(&wide.token).unwrap();
    for token in [&mixed.token,&mixed_joined.token,&rejoined.token,&wide.token,&narrowed.token] {
        let snap = board.snapshot(token).unwrap();
        assert!(snap.channels.contains(&OTHER.into()));
        assert!(!snap.channels.iter().any(|c| c.split('/').next()==Some(PROJECT)));
        assert_eq!(board.download_attachment(token,file.id).unwrap_err().0,404);
        assert_eq!(board.action(token,None,None,post(PROJECT,"denied")).unwrap_err().0,403);
        assert_eq!(board.members(token,CHILD).unwrap_err().0,403);
        board.action(token,None,None,post(OTHER,"still allowed")).unwrap();
        assert!(!board.events(token,snap.cursor).unwrap().is_empty());
    }
    board.action(&restricted.token,None,None,post(OTHER,"still allowed")).unwrap();
    assert_eq!(board.snapshot(&restricted.token).unwrap_err().0,403);
    assert_eq!(board.action(&restricted.token,None,None,post(CHILD,"denied")).unwrap_err().0,403);
    assert!(board.events(&other.token,before).unwrap().iter().all(|e| !serde_json::to_string(e).unwrap().contains(PROJECT)));
    server.abort();
}