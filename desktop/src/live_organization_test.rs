//! Opt-in cross-workspace test: build the daemon and MCP adapter in ../target/debug first.
use super::*;
use neochat::{client::Client, protocol::Discovery};
use serde_json::{Value, json};
use std::{io::Write, path::{Path, PathBuf}, process::{Child, Command, Stdio}, sync::Arc};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}

fn binary(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/debug")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

fn mcp(dir: &Path, name: &str, args: Value) -> Value {
    let mut peer = Command::new(binary("neochat-mcp"))
        .env_remove("NEOCHAT_URL").env_remove("NEOCHAT_TOKEN")
        .arg("--data-dir").arg(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut input = peer.stdin.take().unwrap();
    for request in [
        json!({"id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"neoism","version":"1"}}}),
        json!({"method":"notifications/initialized"}),
        json!({"id":2,"method":"tools/call","params":{"name":name,"arguments":args}}),
    ] {
        let mut request = request;
        request["jsonrpc"] = "2.0".into();
        writeln!(input, "{request}").unwrap();
    }
    drop(input);
    let out = peer.wait_with_output().unwrap();
    assert!(out.status.success());
    let reply: Value = serde_json::from_slice(out.stdout.trim_ascii_end().rsplit(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_eq!(reply["id"], 2);
    assert_eq!(reply["result"]["isError"], false, "{name}: {reply}");
    reply["result"]["structuredContent"].clone()
}

fn shown(rt: &rune::Runtime<App>, text: &str) -> bool {
    rt.elements().iter().any(|e| !e.exiting && e.text.as_deref() == Some(text)
        && e.visible[2] > 0. && e.visible[3] > 0. && e.opacity > 0.)
}

fn until(rt: &mut rune::Runtime<App>, description: &str, ready: impl Fn(&rune::Runtime<App>) -> bool) {
    let start = Instant::now();
    loop {
        rt.update(|app, tx| { poll(app, start.elapsed().as_secs_f64(), tx); });
        rt.frame(1./60.);
        if ready(rt) { return; }
        assert!(start.elapsed() < Duration::from_secs(10), "timed out: {description}; status: {}", rt.state.status);
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires current debug neochatd and neochat-mcp binaries"]
fn live_mcp_organization_reaches_open_desktop() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = Process(Command::new(binary("neochatd")).args(["--bind", "127.0.0.1:0", "--data-dir"])
        .arg(dir.path()).stdout(Stdio::null()).spawn().unwrap());
    let start = Instant::now();
    let discovery: Discovery = loop {
        if let Ok(data) = std::fs::read(dir.path().join("endpoint.json"))
            && let Ok(info) = serde_json::from_slice(&data) { break info; }
        assert!(start.elapsed() < Duration::from_secs(10), "daemon startup timeout");
        std::thread::sleep(Duration::from_millis(20));
    };
    let client = Client::new(&discovery.url, &discovery.token, None).unwrap();
    let channel = "test/general";
    client.action(&Action::CreateChannel { channel: channel.into() }).unwrap();
    let mut app = App::new();
    app.options.endpoint = Some(discovery.url);
    app.options.token = discovery.token;
    app.project = "test".into();
    app.navigate(Page::Tasks);
    let (wake_tx, wakes) = mpsc::sync_channel(1);
    setup(&mut app, Arc::new(move || { let _ = wake_tx.try_send(()); }));
    let mut rt = rune::Runtime::new(app, view::root, (1440., 1000.), rune::Env::new(true, 1.));
    until(&mut rt, "connected to local board", |rt| rt.state.snapshot.channels.iter().any(|c| c == channel));
    let click = |rt: &mut rune::Runtime<App>, key: &str| {
        // Layout can move when a live update inserts a row; wait until it wins hit testing.
        until(rt, key, |rt| rt.find(key).is_some_and(|(x,y)| rt.element_at(x,y)
            .is_some_and(|hit| hit.path().split('/').any(|part| part == key))));
        let (x, y) = rt.find(key).unwrap();
        rt.pointer_move(x,y); rt.pointer_down(x,y); rt.pointer_up(x,y); rt.frame(1./60.);
    };
    // Model an idle window: each external write must wake it before we drive poll again.
    let write = |name, args| {
        while wakes.try_recv().is_ok() {}
        let result = mcp(dir.path(), name, args);
        wakes.recv_timeout(Duration::from_secs(10)).expect("worker did not wake desktop");
        result
    };
    let parent = write("tasks_create", json!({"channel":channel,"text":"MCP parent"}))["id"].as_u64().unwrap();
    click(&mut rt, &format!("task-{parent}"));
    until(&mut rt, "parent thread before child exists", |rt| rt.find("add-subtask").is_some());
    let reply = write("reply", json!({"thread":parent,"text":"Which build should I verify?"}))["id"].as_u64().unwrap();
    until(&mut rt, "live reply with framework byline", |rt| rt.state.snapshot.messages.iter().any(|m|m.id==reply && m.sender.as_deref()==Some("mcp:Neoism") && shown(rt,&rt.state.sender_label(m))));
    let child = write("tasks_create", json!({"channel":channel,"text":"MCP child","parent":parent}))["id"].as_u64().unwrap();
    until(&mut rt, "child in open parent panel", |rt| rt.find(&format!("open-subtask-{child}")).is_some() && shown(rt, "0 of 1 complete"));
    assert!(rt.find(&format!("task-{parent}")).is_some());
    assert!(rt.find(&format!("task-{child}")).is_none());
    click(&mut rt, &format!("open-subtask-{child}"));
    until(&mut rt, "child panel", |rt| rt.state.thread == Some(child) && rt.find("parent-task-link").is_some());
    click(&mut rt, "subtask-back");
    until(&mut rt, "returned to parent panel", |rt| rt.state.thread == Some(parent) && shown(rt, "0 of 1 complete"));
    for (slot,dx,dy) in [(0,30.,0.),(1,-30.,0.),(2,0.,-30.)] {
        let before=rt.state.pane_sizes[slot];let (x,y)=rt.find(&format!("pane-divider-{slot}")).unwrap();
        rt.pointer_move(x,y);rt.pointer_down(x,y);rt.pointer_move(x+dx,y+dy);rt.pointer_up(x+dx,y+dy);rt.frame(1./60.);
        assert_ne!(rt.state.pane_sizes[slot],before,"divider {slot}");
    }
}