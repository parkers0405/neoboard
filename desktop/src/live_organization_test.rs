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
        json!({"id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"desktop-test","version":"1"}}}),
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
    // Barrier: wait for the initial organization fetch, not just the first snapshot.
    client.action(&Action::CreateArea { board: "test".into(), name: "Baseline".into() }).unwrap();
    let mut app = App::new();
    app.options.endpoint = Some(discovery.url);
    app.options.token = discovery.token;
    app.project = "test".into();
    app.navigate(Page::Tasks);
    app.group_by_area = true;
    let (wake_tx, wakes) = mpsc::sync_channel(1);
    setup(&mut app, Arc::new(move || { let _ = wake_tx.try_send(()); }));
    let mut rt = rune::Runtime::new(app, view::root, (1440., 1000.), rune::Env::new(true, 1.));
    until(&mut rt, "initial organization", |rt| rt.state.areas().contains("Baseline"));
    rt.update(|a, _| { a.dialog = Some(Dialog::Filters); a.details_picker = Some("filter-area"); });
    // Model an idle window: each external write must wake it before we drive poll again.
    let write = |name, args| {
        while wakes.try_recv().is_ok() {}
        let result = mcp(dir.path(), name, args);
        wakes.recv_timeout(Duration::from_secs(10)).expect("worker did not wake desktop");
        result
    };
    write("tasks_create_area", json!({"board":"test","name":"Alpha"}));
    until(&mut rt, "empty area in open filter picker", |rt| shown(rt, "Alpha"));
    rt.update(|a, _| { a.dialog = None; a.details_picker = None; });
    let parent = write("tasks_create", json!({"channel":channel,"text":"MCP parent","area":"Alpha"}))["id"].as_u64().unwrap();
    until(&mut rt, "parent in area group", |rt| rt.find("area-group-Alpha").is_some() && shown(rt, "MCP parent"));
    rt.update(|a, _| a.open_thread(parent));
    until(&mut rt, "parent thread before child exists", |rt| shown(rt, "No subtasks yet"));
    let child = write("tasks_create", json!({"channel":channel,"text":"MCP child","area":"Alpha","parent":parent}))["id"].as_u64().unwrap();
    until(&mut rt, "child in already-open parent thread", |rt| shown(rt, &format!("#{child} MCP child")) && shown(rt, "0 of 1 complete"));
}