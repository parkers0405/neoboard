//! Real stdio peers and real HTTP daemon; no mocked MCP or board transport.
use neochat::{client::Client, protocol::{Action, Discovery, InviteRequest}};
use serde_json::{json, Value};
use std::{io::{BufRead, BufReader, Write}, process::{Child, ChildStdin, Command, Stdio}, sync::mpsc, time::{Duration, Instant}};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}

struct Peer {
    _process: Process,
    input: ChildStdin,
    output: mpsc::Receiver<String>,
    id: u64,
}
impl Peer {
    fn command() -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_neochat-mcp"));
        c.env_remove("NEOCHAT_URL").env_remove("NEOCHAT_TOKEN").env_remove("NEOCHAT_DATA_DIR");
        c
    }
    fn new(url: &str, token: &str, name: &str) -> Self {
        let mut command = Self::command();
        command.env("NEOCHAT_URL", url).env("NEOCHAT_TOKEN", token);
        Self::spawn(command, name)
    }
    fn spawn(mut command: Command, name: &str) -> Self {
        let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, output) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() { break; }
            }
        });
        let mut peer = Self { _process: Process(child), input, output, id: 0 };
        let init = peer.rpc("initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":name,"version":"1"}}));
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(init["result"]["capabilities"]["tools"]["listChanged"], false);
        peer.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        peer
    }
    fn send(&mut self, value: Value) {
        writeln!(self.input, "{value}").unwrap();
        self.input.flush().unwrap();
    }
    fn receive(&self) -> Value {
        let line = self.output.recv_timeout(Duration::from_secs(8)).expect("MCP response timed out");
        serde_json::from_str(&line).expect("stdout must contain only newline JSON-RPC")
    }
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        self.send(json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params}));
        let r = self.receive();
        assert_eq!(r["jsonrpc"], "2.0");
        assert_eq!(r["id"], self.id);
        r
    }
    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.rpc("tools/call", json!({"name":name,"arguments":arguments}))["result"].clone()
    }
    fn ok(&mut self, name: &str, arguments: Value) -> Value {
        let r = self.tool(name, arguments);
        assert_eq!(r["isError"], false, "{r}");
        let text: Value = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, r["structuredContent"]);
        text
    }
}

fn daemon(dir: &std::path::Path) -> (Process, Discovery) {
    let process = Process(Command::new(env!("CARGO_BIN_EXE_neochatd"))
        .args(["--bind", "127.0.0.1:0", "--data-dir"]).arg(dir)
        .stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(bytes) = std::fs::read(dir.join("endpoint.json")) {
            if let Ok(discovery) = serde_json::from_slice(&bytes) { return (process, discovery); }
        }
        assert!(Instant::now() < deadline, "daemon startup timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn daemon_grants_apply_to_stdio_tools() {
    use neochat::protocol::Grant;
    let tmp = tempfile::tempdir().unwrap();
    let (_daemon, d) = daemon(tmp.path());
    let admin = Client::discover_admin(Some(tmp.path())).unwrap();
    let mut host = Peer::new(&d.url,&d.token,"host");
    let root = host.ok("post",json!({"channel":"work","text":"root"}))["id"].clone();
    let outside = host.ok("post",json!({"channel":"work/child","text":"private"}))["id"].clone();
    for (actor,grants) in [("none",vec![]),("reader",vec![Grant::Read]),("review",vec![Grant::Read,Grant::Reply])] {
        let invite = admin.restricted_invite(&InviteRequest {actor:actor.into(),channels:vec!["work".into()],expires_at:None},&grants).unwrap();
        let mut peer = Peer::new(&d.url,&invite.token,"test");
        let read = peer.tool("read",json!({}));
        assert_eq!(read["isError"],!grants.contains(&Grant::Read));
        assert!(!read.to_string().contains("private"));
        assert_eq!(peer.tool("reply",json!({"thread":root,"text":"review"}))["isError"],!grants.contains(&Grant::Reply));
        assert_eq!(peer.tool("reply",json!({"thread":outside,"text":"no"}))["isError"],true);
        assert_eq!(peer.tool("post",json!({"channel":"work","text":"no"}))["isError"],true);
        admin.revoke_invite(invite.id).unwrap();
        assert_eq!(peer.tool("read",json!({}))["isError"],true);
    }
}

#[test]
fn ordinary_board_parity_over_scoped_stdio() {
    use neochat::protocol::Grant;
    let tmp = tempfile::tempdir().unwrap();
    let (_daemon, d) = daemon(tmp.path());
    let admin = Client::discover_admin(Some(tmp.path())).unwrap();
    let invite = admin.restricted_invite(&InviteRequest {actor:"worker".into(),channels:vec!["work".into()],expires_at:None},
        &[Grant::Read, Grant::CreateChannel, Grant::Post, Grant::Edit, Grant::SetLabels, Grant::React, Grant::Schedule, Grant::CancelSchedule]).unwrap();
    let mut worker = Peer::new(&d.url, &invite.token, "worker");
    let mut host = Peer::new(&d.url, &d.token, "host");
    worker.ok("channel_create", json!({"channel":"work"}));
    assert_eq!(worker.tool("channel_create", json!({"channel":"outside"}))["isError"], true);
    let root = worker.ok("post", json!({"channel":"work","text":"first","labels":{"priority":"low"}}));
    let id = &root["id"];
    let outside = host.ok("post", json!({"channel":"outside","text":"private"}))["id"].clone();
    host.ok("react",json!({"id":outside,"reaction":"private"}));
    assert_eq!(worker.ok("read", json!({"thread":id}))["messages"][0]["labels"], json!({"priority":"low"}));
    worker.ok("edit", json!({"id":id,"text":"second"}));
    worker.ok("labels", json!({"id":id,"labels":{"status":"open"}}));
    host.ok("reply", json!({"thread":id,"text":"reply"}));
    let unrelated = host.ok("post", json!({"channel":"work","text":"other"}));
    let messages = worker.ok("read", json!({"channel":"work","thread":id}));
    assert_eq!(messages["messages"].as_array().unwrap().len(), 2);
    assert_eq!(messages["messages"][0]["text"], "second");
    assert_eq!(messages["messages"][0]["labels"], json!({"status":"open"}));
    let first = worker.ok("wait", json!({"channel":"work","thread":id,"after":root["cursor"],"limit":1}));
    assert_eq!(first["has_more"], true);
    let rest = worker.ok("wait", json!({"channel":"work","thread":id,"after":first["next_cursor"]}));
    assert_eq!(rest["events"].as_array().unwrap().len(), 2);
    assert_eq!(rest["next_cursor"], unrelated["cursor"]);
    let empty = worker.ok("wait", json!({"channel":"outside","after":root["cursor"]}));
    assert_eq!(empty["events"], json!([]));
    assert_eq!(empty["next_cursor"], unrelated["cursor"]);

    worker.ok("react", json!({"id":id,"reaction":"ok"}));
    let reactions = worker.ok("reactions", json!({}));
    assert_eq!(reactions["reactions"].as_array().unwrap().len(), 1);
    assert_eq!(reactions["reactions"][0]["id"], *id);
    worker.ok("react", json!({"id":id,"reaction":"ok","remove":true}));
    assert_eq!(worker.ok("reactions", json!({"id":id}))["reactions"], json!([]));
    let local = Client::new(&d.url,&d.token,None).unwrap();
    let metadata = local.attach(id.as_u64().unwrap(),"note.txt",b"opaque".to_vec()).unwrap();
    assert_eq!(worker.ok("attachments",json!({"id":id}))["attachments"], json!([metadata]));
    assert_eq!(worker.tool("attachments",json!({"id":outside}))["isError"],true);
    assert_eq!(worker.rpc("tools/call",json!({"name":"attachments","arguments":{"id":id,"path":"unused"}}))["error"]["code"],-32602);

    let scheduled = worker.ok("schedule", json!({"channel":"work","text":"later","in_secs":3600,"labels":{"k":"v"}}));
    let scheduled_id = &scheduled["id"];
    let pending = worker.ok("schedules",json!({}));
    assert_eq!(pending["schedules"][0]["id"], *scheduled_id);
    assert_eq!(pending["schedules"][0]["labels"], json!({"k":"v"}));
    assert_eq!(host.ok("schedules",json!({}))["schedules"],json!([]));
    assert_eq!(host.tool("cancel_schedule",json!({"id":scheduled_id}))["isError"],true);
    admin.revoke_grant(invite.id, Grant::React).unwrap();
    assert_eq!(worker.tool("react",json!({"id":id,"reaction":"ok"}))["structuredContent"]["error"]["code"],"forbidden");
    worker.ok("cancel_schedule",json!({"id":scheduled_id}));
    assert_eq!(worker.ok("schedules",json!({}))["schedules"],json!([]));
}

#[test]
fn embedded_docs_over_stdio_without_a_board() {
    let mut peer = Peer::new("http://127.0.0.1:1", "unused", "manual-reader");
    let pages = [
        ("start", include_str!("../docs/start.md")),
        ("boards", include_str!("../docs/boards.md")),
        ("tasks", include_str!("../docs/tasks.md")),
        ("agents", include_str!("../docs/agents.md")),
        ("operations", include_str!("../docs/operations.md")),
    ];
    assert_eq!(peer.ok("docs_list", json!({}))["pages"], json!(pages.map(|(id, _)| id)));
    for (id, body) in pages {
        let mut offset = 0;
        let mut full = String::new();
        loop {
            let page = peer.ok("docs_read", json!({"id":id,"offset":offset,"limit":512}));
            assert_eq!(page["id"], id);
            let text = page["markdown"].as_str().unwrap();
            full.push_str(text);
            assert!(text.chars().count() <= 512);
            if page["next_offset"].is_null() { break; }
            assert_eq!(page["next_offset"], offset + 512);
            offset += 512;
        }
        assert_eq!(full, body);
    }
    let hits = peer.ok("docs_search", json!({"query":"#","limit":1}));
    assert_eq!(hits["hits"].as_array().unwrap().len(), 1);
    assert_eq!(hits["has_more"], true);
    let hit = &hits["hits"][0];
    let (_, body) = pages.iter().find(|(id, _)| hit["id"] == *id).unwrap();
    let line = body.lines().nth(hit["line"].as_u64().unwrap() as usize - 1).unwrap();
    assert_eq!(hit["snippet"], line[line.find('#').unwrap()..].chars().take(160).collect::<String>());
    assert_eq!(peer.ok("docs_search", json!({"query":"__missing_manual_phrase__"}))["hits"], json!([]));
    for (name, args) in [
        ("docs_list", json!({"path":"/"})), ("docs_read", json!({"id":"../README.md"})),
        ("docs_read", json!({"id":"start","limit":4097})), ("docs_search", json!({"query":"x".repeat(129)})),
        ("docs_search", json!({"query":"x","limit":21})),
    ] {
        assert_eq!(peer.rpc("tools/call", json!({"name":name,"arguments":args}))["error"]["code"], -32602);
    }
    assert_eq!(peer.tool("docs_search", json!({"query":"  "}))["isError"], true);
}

#[test]
fn claim_next_waiters_race_after_post_and_replay_winning_request() {
    let dir = tempfile::tempdir().unwrap();
    let (_daemon, discovery) = daemon(dir.path());
    let admin = Client::discover_admin(Some(dir.path())).unwrap();
    let invite = admin.invite(&InviteRequest { actor:"worker".into(), channels:vec!["work".into()], expires_at:None }).unwrap();
    let mut peer = Peer::new(&discovery.url, &invite.token, "worker");
    let board = Client::discover(Some(dir.path()), None).unwrap();
    let post = || board.action(&Action::Post { channel:"work".into(), text:"ready".into(), labels:Default::default(), task:true, needs:vec![], assignee:None }).unwrap().id.unwrap();
    let args = json!({"channel":"work","lease_secs":60,"wait_ms":1800,"request_id":"mcp-wait"});
    let cli = || {
        let out = Command::new(env!("CARGO_BIN_EXE_neochat")).env_remove("NEOCHAT_PROFILE")
            .args(["--url", &discovery.url, "--token", &discovery.token, "--json", "--request-id", "cli-wait",
                "tasks", "claim-next", "work", "--wait-ms", "1800"]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    let start = Instant::now();
    let (mcp, cli_result, id) = std::thread::scope(|s| {
        let mcp = s.spawn(|| peer.ok("tasks_claim_next", args.clone()));
        let shell = s.spawn(cli);
        // Synchronize on both committed first misses, not a guessed startup delay.
        let db = neochat::storage::Connection::open(dir.path().join("board.sqlite3")).unwrap();
        loop {
            let misses: u32 = db.query_row("SELECT COUNT(*) FROM idempotency WHERE key IN ('cli-wait','mcp-wait') AND json_extract(outcome,'$.id') IS NULL", [], |r| r.get(0)).unwrap();
            if misses == 2 { break; }
            assert!(start.elapsed() < Duration::from_secs(1));
            std::thread::sleep(Duration::from_millis(10));
        }
        let id = post();
        (mcp.join().unwrap(), shell.join().unwrap(), id)
    });
    assert!(start.elapsed() >= Duration::from_millis(1750)); // loser waited out the original budget
    assert_eq!([&mcp,&cli_result].iter().filter(|v| v["id"] == id).count(), 1);
    assert_eq!([&mcp,&cli_result].iter().filter(|v| v["id"].is_null()).count(), 1);
    let spare = post();
    let replay = if mcp["id"] == id { peer.ok("tasks_claim_next", args) } else { cli() };
    assert_eq!(replay, if mcp["id"] == id { mcp } else { cli_result });
    assert!(board.snapshot().unwrap().messages.iter().find(|m| m.id==spare).unwrap().task.as_ref().unwrap().owner.is_none());
}

#[test]
fn claim_next_timeout_keeps_one_budget_across_unrelated_wakes() {
    let dir = tempfile::tempdir().unwrap();
    let (_daemon, discovery) = daemon(dir.path());
    let mut peer = Peer::new(&discovery.url, &discovery.token, "worker");
    let board = Client::discover(Some(dir.path()), None).unwrap();
    std::thread::scope(|s| {
        let start = Instant::now();
        s.spawn(|| {
            for n in 0..12 {
                std::thread::sleep(Duration::from_millis(100));
                board.action(&Action::CreateChannel { channel:format!("noise{n}") }).unwrap();
            }
        });
        let miss = peer.ok("tasks_claim_next", json!({"channel":"work","lease_secs":60,"wait_ms":650}));
        assert!(miss["id"].is_null());
        assert!((Duration::from_millis(620)..Duration::from_millis(1150)).contains(&start.elapsed()));
    });
}

#[test]
fn explicit_task_fields_round_trip_and_legacy_text_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let (_daemon, d) = daemon(dir.path());
    let mut peer = Peer::new(&d.url, &d.token, "neoism");
    let catalog = peer.rpc("tools/list", json!({}));
    for name in ["tasks_create", "tasks_update"] {
        let tool = catalog["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == name).unwrap();
        let schema = &tool["inputSchema"];
        assert!(schema["required"].as_array().unwrap().contains(&json!("title")));
        assert_eq!(schema["properties"]["description"]["type"], "string");
        assert!(schema["properties"].get("text").is_none());
        for mut args in [json!({}), json!({"description":"no title"}), json!({"title":"  "}),
            json!({"title":"a\nb"}), json!({"title":"a\r"}), json!({"title":"x".repeat(121)}),
            json!({"title":"ok","description":false}), json!({"title":"ok","description":"x".repeat(16384)}),
            json!({"title":"a","text":"b"}), json!({"text":"a","description":"b"})] {
            if name == "tasks_create" { args["channel"] = json!("work"); } else { args["id"] = json!(1); }
            assert_eq!(peer.rpc("tools/call", json!({"name":name,"arguments":args}))["error"]["code"], -32602);
        }
    }
    let title = "é".repeat(120);
    let body = "Acceptance:\n\n- passes tests";
    let task = peer.ok("tasks_create", json!({"channel":"work","title":title,"description":body}));
    assert_eq!(task["title"], title);
    assert_eq!(task["description"], body);
    let id = &task["id"];
    for name in ["read", "tasks_list"] {
        let msg = &peer.ok(name, json!({"channel":"work"}))["messages"][0];
        assert_eq!(msg["title"], title);
        assert_eq!(msg["description"], body);
        assert_eq!(msg["text"], format!("{title}\n\n{body}"));
    }
    let updated = peer.ok("tasks_update", json!({"id":id,"title":"Smaller task"}));
    assert_eq!(updated["description"], "");
    let event = &peer.ok("wait", json!({"after":task["cursor"]}))["events"][0]["message"];
    assert_eq!(event["title"], "Smaller task");
    assert_eq!(event["description"], "");
    let text = "Long legacy heading".repeat(10) + "\r\nlegacy details";
    let legacy = peer.ok("tasks_update", json!({"id":id,"text":text}));
    assert_eq!(legacy["description"], "legacy details");
    let msg = &peer.ok("read", json!({"channel":"work"}))["messages"][0];
    assert_eq!(msg["text"], text);
    assert_eq!(msg["title"], "Long legacy heading".repeat(10));
}

#[test]
fn sprint_lifecycle_over_stdio() {
    let dir = tempfile::tempdir().unwrap();
    let (_daemon, discovery) = daemon(dir.path());
    let mut peer = Peer::new(&discovery.url, &discovery.token, "neoism");
    peer.ok("channel_create", json!({"channel":"team/work"}));
    let sprint = peer.ok("sprints_create", json!({"board":"team","title":"Release"}))["id"].clone();
    let id = peer.ok("tasks_create", json!({"channel":"team/work","title":"Package","sprint":sprint}))["id"].clone();
    let list = |p: &mut Peer| p.ok("tasks_list", json!({"sprint":sprint}))["messages"].clone();
    assert_eq!(list(&mut peer)[0]["id"], id);
    peer.ok("tasks_set_sprint", json!({"id":id}));
    assert_eq!(list(&mut peer), json!([]));
    peer.ok("tasks_set_sprint", json!({"id":id,"sprint":sprint}));
    assert_eq!(peer.tool("sprints_finish", json!({"id":sprint}))["isError"], true);
    peer.ok("sprints_finish", json!({"id":sprint,"allow_unfinished":true}));
    let rejected = peer.tool("tasks_create", json!({"channel":"team/work","title":"Too late","sprint":sprint}));
    assert_eq!(rejected["isError"], true);
    let tasks = peer.ok("tasks_list", json!({"channel":"team/work"}));
    assert_eq!(tasks["messages"].as_array().unwrap().len(), 1, "failed membership must not leave a task behind");
    let catalog = peer.ok("sprints_list", json!({"board":"team"}));
    assert_eq!(catalog["sprints"][0]["finished"], true);
    assert!(catalog.get("areas").is_none());
    let guest = Client::discover_admin(Some(dir.path())).unwrap().invite(&InviteRequest { actor:"guest".into(), channels:vec!["other/*".into()], expires_at:None }).unwrap();
    let mut guest = Peer::new(&discovery.url, &guest.token, "neoism");
    assert_eq!(guest.ok("sprints_list", json!({}))["sprints"], json!([]));
    assert_eq!(list(&mut guest), json!([]));
}

#[test]
fn explicit_completion_option() {
    let dir = tempfile::tempdir().unwrap();
    let (_daemon, discovery) = daemon(dir.path());
    let mut peer = Peer::new(&discovery.url, &discovery.token, "neoism");
    let id = peer.ok("tasks_create", json!({"channel":"work","text":"parent"}))["id"].as_u64().unwrap();
    peer.ok("tasks_create", json!({"channel":"work","text":"child","parent":id}));
    peer.ok("tasks_claim", json!({"id":id,"lease_secs":60}));
    for args in [json!({"id":id,"text":"done"}), json!({"id":id,"text":"done","allow_unfinished":false})] {
        assert_eq!(peer.tool("tasks_complete", args)["structuredContent"]["error"]["code"], "conflict");
    }
    peer.ok("tasks_complete", json!({"id":id,"text":"done","allow_unfinished":true}));
}

#[test]
fn two_actors_coordinate_over_stdio_with_private_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let (_daemon, discovery) = daemon(dir.path());
    let board = Client::new(&discovery.url, &discovery.token, None).unwrap();
    for channel in ["work", "private"] {
        board.action(&Action::CreateChannel { channel: channel.into() }).unwrap();
    }
    let admin = Client::discover_admin(Some(dir.path())).unwrap();
    let invite = |actor: &str, channels: Vec<String>| admin.invite(&InviteRequest { actor:actor.into(), channels, expires_at:None }).unwrap();
    let alice = invite("alice", vec!["work".into(), "private".into()]);
    let bob = invite("bob", vec!["work".into()]);
    assert_ne!(alice.token, bob.token);
    let mut a = Peer::new(&discovery.url, &alice.token, "claude-code");
    let mut b = Peer::new(&discovery.url, &bob.token, "neoism");
    assert_eq!(a.rpc("ping", json!({}))["result"], json!({}));
    let catalog = a.rpc("tools/list", json!({}));
    let names: Vec<_> = catalog["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["channels","read","post","reply","wait","tasks_list","tasks_create","tasks_claim","tasks_claim_next","tasks_renew","tasks_complete","tasks_fail","members","tasks_assign","tasks_block","tasks_unblock","tasks_cancel","tasks_placements","tasks_update","tasks_acknowledge","docs_list","docs_read","docs_search","channel_create","edit","labels","react","reactions","attachments","schedule","schedules","cancel_schedule","sprints_list","sprints_create","tasks_set_sprint","sprints_finish"]);
    for tool in catalog["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert!(tool["inputSchema"]["properties"].get("area").is_none());
    }

    let task = a.ok("tasks_create", json!({"channel":"work","text":"coordinate two independent actors"}));
    let id = task["id"].as_u64().unwrap();
    let task_cursor = task["cursor"].as_u64().unwrap();
    let listed = b.ok("tasks_list", json!({"channel":"work"}));
    assert_eq!(listed["messages"][0]["author"], "alice");
    Client::new(&discovery.url,&alice.token,None).unwrap().set_display_name("Alice Lee").unwrap();
    let renamed=b.ok("read",json!({"channel":"work"}));
    assert_eq!(renamed["messages"][0]["author"],"Alice Lee");
    assert_eq!(renamed["messages"][0]["author_id"],"alice");
    assert_eq!(listed["messages"][0]["sender"], "mcp:Claude Code");
    assert_eq!(listed["messages"][0]["labels"], json!({}));
    assert_eq!(listed["messages"][0]["id"], id);

    // Both attempt to claim; exactly one owns the lease, and ownership is actor-bound.
    assert_eq!(b.ok("tasks_claim_next", json!({"channel":"work","lease_secs":60,"assignment":"unassigned"}))["id"], id);
    let rejected_claim = a.tool("tasks_claim", json!({"id":id,"lease_secs":60}));
    assert_eq!(rejected_claim["isError"], true);
    assert_eq!(rejected_claim["structuredContent"]["error"]["code"], "conflict");
    assert_eq!(a.tool("tasks_renew", json!({"id":id,"lease_secs":60}))["isError"], true);
    b.ok("tasks_renew", json!({"id":id,"lease_secs":60}));
    let events = a.ok("wait", json!({"after":task_cursor,"wait_ms":0,"limit":1}));
    assert_eq!(events["events"][0]["actor"], "bob");
    assert_eq!(events["events"][0]["message"]["sender"], "mcp:Claude Code");
    assert_eq!(events["events"][0]["message"]["task"]["owner"], "bob");
    assert_eq!(events["has_more"], true);
    let resumed = a.ok("wait", json!({"after":events["next_cursor"],"wait_ms":0}));
    assert!(resumed["next_cursor"].as_u64().unwrap() > events["next_cursor"].as_u64().unwrap());

    let done = b.ok("tasks_complete", json!({"id":id,"text":"result transferred"}));
    let done_events = a.ok("wait", json!({"after":resumed["next_cursor"],"wait_ms":0}));
    assert!(done_events["events"].as_array().unwrap().iter().any(|e| e["message"]["task"]["state"] == "completed"));
    assert_eq!(done_events["next_cursor"], done["cursor"]);
    assert!(done_events["events"].as_array().unwrap().iter().any(|e| e["message"]["text"] == "result transferred" && e["message"]["sender"] == "mcp:Neoism"));
    a.ok("reply", json!({"thread":id,"text":"received"}));
    assert!(b.ok("read", json!({"channel":"work"}))["messages"].as_array().unwrap().iter().any(|m| m["text"] == "received" && m["sender"] == "mcp:Claude Code"));

    let secret = a.ok("post", json!({"channel":"private","text":"PRIVATE_SENTINEL"}));
    let channels = b.ok("channels", json!({}));
    assert_eq!(channels["channels"], json!(["work"]));
    assert!(!b.ok("read", json!({})).to_string().contains("PRIVATE_SENTINEL"));
    assert!(!b.ok("wait", json!({"after":0})).to_string().contains("PRIVATE_SENTINEL"));
    assert!(b.ok("read", json!({"channel":"private"}))["messages"].as_array().unwrap().is_empty());
    assert_eq!(b.tool("reply", json!({"thread":secret["id"],"text":"not allowed"}))["isError"], true);
    assert_eq!(b.tool("post", json!({"channel":"private","text":"not allowed"}))["isError"], true);
    assert_eq!(a.ok("read", json!({"channel":"private"}))["messages"][0]["text"], "PRIVATE_SENTINEL");

    // Start a bounded long poll in one process and transfer an event from the other.
    let cursor = board.snapshot().unwrap().cursor;
    b.id += 1;
    b.send(json!({"jsonrpc":"2.0","id":b.id,"method":"tools/call","params":{"name":"wait","arguments":{"after":cursor,"wait_ms":2000}}}));
    a.ok("post", json!({"channel":"work","text":"wake up peer"}));
    let transfer = b.receive();
    assert!(transfer["result"]["structuredContent"]["events"].as_array().unwrap().iter().any(|e| e["message"]["text"] == "wake up peer"));
    let cursor = transfer["result"]["structuredContent"]["next_cursor"].clone();
    let start = Instant::now();
    assert!(b.ok("wait", json!({"after":cursor,"wait_ms":30}))["events"].as_array().unwrap().is_empty());
    assert!(start.elapsed() < Duration::from_secs(2));

    // Fail is deliberately separate from complete.
    let task = a.ok("tasks_create", json!({"channel":"work","text":"failure path","needs":[],"assignee":"bob"}));
    b.ok("tasks_claim", json!({"id":task["id"],"lease_secs":60}));
    b.ok("tasks_fail", json!({"id":task["id"],"text":"cannot proceed"}));
    assert!(a.ok("tasks_list", json!({}))["messages"].as_array().unwrap().iter().any(|m| m["task"]["state"] == "failed"));
    let task=a.ok("tasks_create",json!({"channel":"work","text":"test release"}));let id=&task["id"];
    a.ok("tasks_assign",json!({"id":id,"assignee":"bob"}));
    a.ok("tasks_block",json!({"id":id,"text":"waiting for review"}));
    assert_eq!(b.ok("tasks_list",json!({"thread":id,"assignee":"bob","state":"blocked"}))["messages"].as_array().unwrap().len(),1);
    a.ok("tasks_unblock",json!({"id":id}));a.ok("tasks_cancel",json!({"id":id}));
    assert_eq!(b.ok("tasks_list",json!({"thread":id,"state":"canceled"}))["messages"].as_array().unwrap().len(),1);
    let global = a.ok("tasks_create", json!({"channel":"work","text":"Read release notes","global":true}))["id"].as_u64().unwrap();
    b.ok("tasks_acknowledge", json!({"id":global,"done":true}));
    assert_eq!(b.ok("tasks_list", json!({"thread":global}))["messages"], json!([]));
    assert_eq!(b.ok("tasks_list", json!({"thread":global,"include_global":true}))["messages"][0]["id"], global);
    let placements = b.ok("tasks_placements", json!({}));
    assert_eq!(placements[global.to_string()], json!({"parent":null,"global":true,"done_by":["bob"]}));

    // Transport errors are JSON-RPC errors; execution failures are MCP tool errors.
    for arguments in [json!({"after":0,"wait_ms":25001}), json!({"after":-1}), json!({"after":0,"limit":101}), json!({"after":0,"command":"sh"})] {
        assert_eq!(a.rpc("tools/call", json!({"name":"wait","arguments":arguments}))["error"]["code"], -32602);
    }
    assert_eq!(a.rpc("tools/call", json!({"name":"admin","arguments":{}}))["error"]["code"], -32602);
    assert_eq!(a.rpc("unknown", json!({}))["error"]["code"], -32601);
    a.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":999}}));
    assert_eq!(a.rpc("ping", json!({}))["result"], json!({}));
    a.input.write_all(b"{invalid\n").unwrap(); a.input.flush().unwrap();
    assert_eq!(a.receive()["error"]["code"], -32700);
    a.input.write_all(&vec![b'x'; 128 * 1024 + 1]).unwrap();
    a.input.write_all(b"\n").unwrap(); a.input.flush().unwrap();
    assert_eq!(a.receive()["error"]["code"], -32700);
    assert_eq!(a.rpc("ping", json!({}))["result"], json!({}));

    // Explicit local discovery works with no admin-token file available.
    std::fs::remove_file(dir.path().join("admin-token")).unwrap();
    let mut command = Peer::command(); command.arg("--data-dir").arg(dir.path());
    let mut local = Peer::spawn(command, "integration-test");
    assert!(local.ok("channels", json!({}))["channels"].as_array().unwrap().iter().any(|c| c == "private"));
}

#[test]
fn explicit_credentials_and_safe_transport_required_without_secret_logging() {
    let empty = tempfile::tempdir().unwrap();
    for (url, token) in [(None, None), (Some("https://example.invalid"), None), (None, Some("SECRET_SENTINEL")), (Some("http://example.invalid"), Some("SECRET_SENTINEL")), (Some("https://user:SECRET_SENTINEL@example.invalid"), Some("SECRET_SENTINEL"))] {
        let mut command = Peer::command();
        command.env("NEOCHAT_DATA_DIR", empty.path());
        if let Some(url) = url { command.env("NEOCHAT_URL",url); }
        if let Some(token) = token { command.env("NEOCHAT_TOKEN",token); }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("SECRET_SENTINEL"));
    }
}