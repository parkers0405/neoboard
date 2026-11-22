use neochat::{client::Client, protocol::TaskState};
#[path = "cli_board/init.rs"]
mod init;
use std::{path::Path, process::{Child, Command, Stdio}, thread, time::{Duration, Instant}};

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(dir: &Path) -> (Daemon, Client) {
    let mut daemon = Daemon(Command::new(env!("CARGO_BIN_EXE_neochatd"))
        .args(["--bind", "127.0.0.1:0", "--data-dir"]).arg(dir)
        .stdout(Stdio::null()).spawn().expect("start daemon"));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(client) = Client::discover(Some(dir), None) {
            if client.events(0, 0).is_ok() { return (daemon, client); }
        }
        assert!(daemon.0.try_wait().unwrap().is_none(), "daemon exited before startup");
        assert!(Instant::now() < deadline, "daemon startup timed out");
        thread::sleep(Duration::from_millis(30));
    }
}

fn cli(dir: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_neochat"))
        .args(["--data-dir"]).arg(dir).args(args)
        .env_remove("NEOCHAT_URL").env_remove("NEOCHAT_TOKEN").env_remove("NEOCHAT_PROFILE")
        .output().expect("run CLI");
    assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn agent_created_work_defaults_to_authenticated_actor_not_framework() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let (_daemon, client) = start(dir);
    for (args, expected) in [
        (vec!["--agent-framework", "Neoism", "tasks", "create", "work", "Mine"], Some("local")),
        (vec!["--agent-framework", "Neoism", "tasks", "create", "work", "Queue", "--unassigned"], None),
    ] {
        let id: u64 = cli(dir, &args).parse().unwrap();
        let task = client.snapshot().unwrap().messages.into_iter().find(|m| m.id == id).unwrap().task.unwrap();
        assert_eq!(task.assignee.as_deref(), expected);
        assert!(task.owner.is_none());
    }
}

#[test]
fn sprint_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let (_daemon, _) = start(dir);
    cli(dir, &["channel", "team/work"]);
    let sprint = cli(dir, &["sprints", "create", "team", "Release", "--description", "Ship it"]);
    let id = cli(dir, &["tasks", "create", "team/work", "Ship", "--sprint", &sprint]);
    let tasks = || cli(dir, &["tasks", "list", "--sprint", &sprint]);
    assert!(tasks().contains("Ship"));
    cli(dir, &["tasks", "set-sprint", &id]);
    assert!(tasks().is_empty());
    cli(dir, &["tasks", "set-sprint", &id, &sprint]);
    cli(dir, &["sprints", "finish", &sprint, "--allow-unfinished"]);
    assert!(cli(dir, &["sprints", "list", "--board", "team"]).contains("[finished] Release"));
}

#[test]
fn explicit_completion_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let (_daemon, client) = start(dir);
    let parent = cli(dir, &["tasks", "create", "work", "parent"]);
    cli(dir, &["tasks", "create", "work", "child", "--parent", &parent]);
    cli(dir, &["tasks", "claim", &parent]);
    cli(dir, &["tasks", "complete", &parent, "accepted", "--allow-unfinished"]);
    let id = parent.parse::<u64>().unwrap();
    assert_eq!(client.snapshot().unwrap().messages.into_iter().find(|m| m.id == id).unwrap().task.unwrap().state, TaskState::Completed);
}

#[test]
fn scoped_operation_grants_on_cli_and_http() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let (_daemon, client) = start(dir);
    let root = cli(dir, &["post","dev/team","visible"]);
    let other = cli(dir, &["post","development/team","secret"]);
    let issue = |actor, options: &[&str]| {
        let mut args = vec!["invite","create",actor,"--channel","dev/*","--json"];
        args.extend_from_slice(options);
        serde_json::from_str::<neochat::protocol::Invite>(&cli(dir,&args)).unwrap()
    };
    let denied = issue("nobody", &["--deny-all"]);
    let guest = issue("review", &["--grant","read","--grant","reply"]);
    let run = |token: &str, args: &[&str]| Command::new(env!("CARGO_BIN_EXE_neochat"))
        .env("NEOCHAT_URL",client.endpoint()).env("NEOCHAT_TOKEN",token).env_remove("NEOCHAT_PROFILE").args(args).output().unwrap();
    let http = reqwest::blocking::Client::new();
    let url = |p: &str| format!("{}/v1/{p}",client.endpoint().trim_end_matches('/'));
    let status = |r: reqwest::blocking::RequestBuilder, t: &str| r.bearer_auth(t).send().unwrap().status().as_u16();
    for p in ["snapshot","events","organization","owned-areas?owner=local","me","members?channel=dev/team","reactions","schedules",&format!("messages/{root}/attachments")] {
        assert_eq!(status(http.get(url(p)),&denied.token),403,"{p}");
        assert_eq!(status(http.get(url(p)),&guest.token),200,"{p}");
    }
    assert_eq!(status(http.get(url("attachments/1")),&denied.token),403);
    assert_eq!(status(http.post(url(&format!("messages/{root}/attachments?name=f"))).body("bytes"),&guest.token),403);
    assert_eq!(status(http.post(url("display-name")).json(&serde_json::json!({"name":"fake"})),&guest.token),403);
    assert!(!run(&denied.token,&["read"]).status.success());
    let read = run(&guest.token,&["read"]);
    assert!(read.status.success());
    let text = String::from_utf8(read.stdout).unwrap();
    assert!(text.contains("visible") && !text.contains("secret"));
    assert!(!run(&guest.token,&["reply",&other,"outside"]).status.success());
    assert!(!run(&guest.token,&["post","dev/team","no"]).status.success());
    let reply = ["reply",&root,"ack","--request-id","retry"];
    let first = run(&guest.token,&reply);
    assert!(first.status.success());
    assert_eq!(run(&guest.token,&reply).stdout,first.stdout);
    let id = guest.id.to_string();
    assert!(!run(&guest.token,&["invite","create","new"]).status.success());
    assert!(!run(&guest.token,&["invite","explain",&id]).status.success());
    assert!(!run(&guest.token,&["invite","revoke-grant",&id,"read"]).status.success());
    cli(dir,&["invite","revoke-grant",&id,"reply"]);
    cli(dir,&["invite","revoke-grant",&id,"reply"]);
    assert!(!run(&guest.token,&reply).status.success()); // not even cached successes
    let info: serde_json::Value = serde_json::from_str(&cli(dir,&["invite","explain",&id])).unwrap();
    assert_eq!(info["grants"],serde_json::json!(["read"]));
    assert_eq!(info["channels"],serde_json::json!(["dev/*"]));
    assert!(info.get("token").is_none());
    assert_eq!(status(http.get(url("snapshot")),&guest.token),200);
    cli(dir,&["invite","revoke",&id]);
    assert_eq!(status(http.get(url("snapshot")),&guest.token),401);
    let expired = issue("expired", &["--grant","read","--grant","reply","--valid-secs","3"]);
    assert!(run(&expired.token,&reply).status.success());
    while neochat::board::now() < expired.expires_at.unwrap() { thread::sleep(Duration::from_millis(50)); }
    assert_eq!(status(http.get(url("snapshot")),&expired.token),401);
    assert!(!run(&expired.token,&reply).status.success());
}

#[test]
fn cli_edits_legacy_task_without_exposing_or_erasing_catalog_metadata() {
    use neochat::protocol::Action;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let (_daemon, client) = start(dir);
    for args in [vec!["tasks","--help"], vec!["tasks","create","--help"], vec!["tasks","update","--help"]] {
        assert!(!cli(dir,&args).to_lowercase().contains("area"));
    }
    client.action(&Action::CreateArea { board:"work".into(), name:"UI".into() }).unwrap();
    let id = client.action(&Action::PostTask { channel:"work/tasks".into(),text:"original".into(),labels:Default::default(),needs:vec![],assignee:None,area:Some("UI".into()),parent:None,global:false }).unwrap().id.unwrap();
    let parent = cli(dir,&["tasks","create","work/tasks","Ship UI"]);
    cli(dir,&["tasks","update",&id.to_string(),"revised","--parent",&parent]);
    let placements: serde_json::Value = serde_json::from_str(&cli(dir,&["tasks","placements"])).unwrap();
    assert_eq!(placements[id.to_string()],serde_json::json!({"parent":parent.parse::<u64>().unwrap(),"global":false,"done_by":[]}));
    assert_eq!(client.organization().unwrap().tasks[&id].area.as_deref(),Some("UI"));
}

#[test]
fn explicit_grants_survive_restart_without_bypassing_ownership_or_task_checks() {
    use neochat::protocol::{Action, Grant, InviteRequest};
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let (daemon, _) = start(dir);
    let id = cli(dir,&["post","dev/team","host owned"]).parse().unwrap();
    let admin = Client::discover_admin(Some(dir)).unwrap();
    let invite = admin.restricted_invite(&InviteRequest {actor:"scheduler".into(),channels:vec!["dev/team".into()],expires_at:None}, &[Grant::Read,Grant::Schedule,Grant::Post,Grant::Edit]).unwrap();
    let guest = Client::new(admin.endpoint(),&invite.token,None).unwrap();
    let scheduled = Action::Schedule { channel:"dev/team".into(), text:"pending".into(), publish_at:neochat::board::now()+3600, labels:Default::default() };
    guest.action_with_key(&scheduled,Some("pending")).unwrap();
    assert_eq!(guest.schedules().unwrap().len(),1);
    admin.revoke_grant(invite.id,Grant::Schedule).unwrap();
    drop(daemon);
    let (_daemon, client) = start(dir);
    let guest = Client::new(client.endpoint(),&invite.token,Some("local")).unwrap();
    assert!(guest.schedules().unwrap().is_empty());
    assert!(guest.action_with_key(&scheduled,Some("pending")).is_err());
    assert!(guest.action(&Action::Edit{id,text:"spoof".into()}).is_err());
    assert!(guest.action(&Action::DeleteChannel{channel:"dev/team".into()}).is_err());
    let post = |task| Action::Post { channel:"dev/team".into(),text:"post".into(),task,labels:Default::default(),needs:vec![],assignee:None };
    assert!(guest.action(&post(true)).is_err());
    guest.action(&post(false)).unwrap();
}

#[test]
fn cli_and_client_share_durable_board_with_scoped_guest_access() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let (daemon, client) = start(dir);
    let post = ["post", "dev", "Ready for review", "--label", "state=open", "--request-id", "release-review-1"];
    let root = cli(dir, &post);
    assert_eq!(cli(dir, &post), root);
    cli(dir, &["--as", "coder", "reply", &root, "Reviewing"]);
    cli(dir, &["post", "private", "Not visible to guests"]);
    let task = cli(dir, &["tasks", "create", "dev", "Review the release"]);
    assert_eq!(cli(dir, &["tasks", "claim-next", "dev", "--label", "missing=yes"]), "");
    assert_eq!(cli(dir, &["--as", "coder", "tasks", "claim-next", "dev", "--assignment", "unassigned"]), task);
    let progress = || cli(dir, &["--json", "tasks", "list", "--state", "in_progress"]);
    assert_eq!(progress(), "[]");
    cli(dir, &["--as", "coder", "tasks", "start", &task]);
    let rows: serde_json::Value = serde_json::from_str(&progress()).unwrap();
    assert_eq!(rows[0]["id"], task.parse::<u64>().unwrap());
    assert_eq!(rows[0]["task"]["state"], "in_progress");
    cli(dir, &["--as", "coder", "tasks", "complete", &task, "Approved"]);
    let snapshot = client.snapshot().unwrap();
    assert!(snapshot.messages.iter().any(|m| m.text == "Reviewing" && m.parent == Some(root.parse().unwrap())));
    assert!(snapshot.messages.iter().any(|m| m.id == task.parse::<u64>().unwrap() && m.task.as_ref().unwrap().state == TaskState::Completed));

    let invite = cli(dir, &["invite", "create", "guest", "--channel", "dev", "--json"]);
    let invite: neochat::protocol::Invite = serde_json::from_str(&invite).unwrap();
    let guest = Client::new(client.endpoint(), &invite.token, None).unwrap();
    assert_eq!(guest.snapshot().unwrap().channels, ["dev"]);
    assert!(!serde_json::to_string(&guest.events(0, 0).unwrap()).unwrap().contains("Not visible to guests"));
    cli(dir, &["invite", "revoke", &invite.id.to_string()]);
    assert!(guest.snapshot().is_err());

    drop(daemon);
    let (_daemon, restarted) = start(dir);
    assert_eq!(restarted.snapshot().unwrap(), snapshot);
    assert!(restarted.events(0, 0).unwrap().windows(2).all(|events| events[0].cursor < events[1].cursor));
}

#[test]
fn cli_queue_excludes_global_tasks_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let (_daemon, client) = start(dir);
    let normal = cli(dir, &["tasks", "create", "dev/tasks", "Prepare build"]);
    let global = cli(dir, &["tasks", "create", "dev", "Read release notes", "--global"]);
    cli(dir, &["tasks", "ack", &global]);
    let list = |args: &[&str]| {
        serde_json::from_str::<Vec<neochat::protocol::Message>>(&cli(dir, args)).unwrap()
            .into_iter().map(|m| m.id.to_string()).collect::<Vec<_>>()
    };
    assert_eq!(list(&["--json", "tasks", "list"]), [normal.clone()]);
    assert_eq!(list(&["--json", "tasks", "list", "--include-global"]), [normal, global.clone()]);
    let org = client.organization().unwrap();
    assert_eq!(org.tasks[&global.parse().unwrap()].done_by, ["local"]);
    cli(dir, &["tasks", "ack", &global, "--undo"]);
    assert!(client.organization().unwrap().tasks[&global.parse().unwrap()].done_by.is_empty());
}