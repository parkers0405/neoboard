use super::*;
use clap::Parser;
use serde_json::{json, Value};

#[derive(Parser)]
struct Cli { #[command(flatten)] wake: Options }

fn options(args: &[&str]) -> Options {
    Cli::try_parse_from(std::iter::once("wake").chain(args.iter().copied())).unwrap().wake
}

fn source(actor: &str) -> Value {
    json!({"board":"board","project":"team","origin":"http://127.0.0.1:1","harness":"neoism",
        "invite":"missing-private-invite", "actor":actor}) // legacy: no chat_id
}

fn binding(actor: &str) -> Value {
    json!({"source":source(actor),"target":{"session":format!("ses_{actor}"),"harness":"neoism",
        "executable":null,"origin":"http://127.0.0.1:1","workspace":"/private-workspace"},
        "enabled":true,"bearer":"private-token","watermark":7,"uncertain":"diagnostic",
        "last":"private-history","test":format!("ack-{actor}")})
}

#[test]
fn scope_keys_and_prepare_requests() {
    let key = |chat: &str| store::binding_key("board", "neoism", chat);
    assert_eq!(key("default").unwrap(), "board:neoism");
    for bad in ["", ".", "..", "a:b", "a/b", "a b", &"a".repeat(129)] { assert!(key(bad).is_err()); }
    let prepare = options(&["prepare","--board-id","board","--harness","neoism","--chat-id","chat-2","--invite","invite","--project","team"]);
    let Command::Prepare { scope, .. } = prepare.command else { unreachable!() };
    assert_eq!(scope.key().unwrap(), "board:neoism:chat-2");

    let legacy = json!({"source":source("old"),"expires":500});
    let mut s: store::State = serde_json::from_value(json!({"bindings":{},"pending":{"legacy":legacy}})).unwrap();
    let mut named: Source = serde_json::from_value(source("old")).unwrap();
    named.chat_id = scope.chat_id;
    assert!(s.prepare("collision".into(), named.clone(), 100).is_err());
    named.actor = "new".into();
    s.prepare("first".into(), named.clone(), 101).unwrap();
    s.prepare("second".into(), named.clone(), 102).unwrap();
    assert_eq!(s.pending.keys().cloned().collect::<Vec<_>>(), ["legacy", "second"]);
    s.bindings.insert(key("default").unwrap(), serde_json::from_value(binding("new")).unwrap());
    assert!(s.prepare("collision".into(), named, 103).is_err());
}

#[test]
fn legacy_and_named_controls_and_worker_use_exact_keys() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    let mut named = binding("second");
    named["source"]["chat_id"] = json!("chat-2");
    let mut other = binding("other");
    other["source"]["board"] = json!("other");
    other["enabled"] = json!(false);
    let rows = json!({"board:neoism":binding("first"), "board:neoism:chat-2":named,"other:neoism":other});
    std::fs::write(dir.join("bindings.json"), json!({"pending":{},"bindings":rows}).to_string()).unwrap();
    let cmd = |args: &[&str]| run_in(options(args), dir.into());
    let scoped = |action, chat: Option<&str>| {
        let mut args = vec![action,"--board-id","board","--harness","neoism"];
        if let Some(id) = chat { args.extend(["--chat-id", id]); }
        cmd(&args)
    };
    scoped("off", None).unwrap(); // sibling remains enabled; no service changes
    scoped("resolve", Some("chat-2")).unwrap();
    assert!(scoped("test", Some("default")).unwrap_err().contains("reserved turn"));
    cmd(&["ack", "--code", "ack-first"]).unwrap();
    cmd(&["run", "--once"]).unwrap(); // named invite is missing: fails locally before network or dispatch
    cmd(&["list", "--board-id", "board"]).unwrap();
    transaction(dir, |s| {
        assert_eq!(s.list("board")?, vec![
            json!({"chat_id":"default","actor":"first","session":"ses_first","harness":"neoism","enabled":false}),
            json!({"chat_id":"chat-2","actor":"second","session":"ses_second","harness":"neoism","enabled":true})]);
        let old = &s.bindings["board:neoism"];
        assert_eq!(old.last, "Test acknowledged by the conversation");
        assert!(old.uncertain.is_some());
        let named = &s.bindings["board:neoism:chat-2"];
        assert!(named.uncertain.is_none() && named.test.is_none());
        assert!(named.last.contains("scoped invite file"));
        assert_eq!(s.bindings["other:neoism"].last, "private-history");
        Ok(())
    }).unwrap();
    scoped("disconnect",Some("default")).unwrap();
    transaction(dir,|s| {
        assert!(!s.bindings.contains_key("board:neoism"));
        assert!(s.bindings["board:neoism:chat-2"].enabled);
        assert!(s.bindings.contains_key("other:neoism")); Ok(())
    }).unwrap();
}

#[test]
fn delivery_notice_names_the_delivery_and_the_harness_connection() {
    let item: neochat::client::InboxItem = serde_json::from_value(json!({"id":9,"cursor":501,"kind":"mention","read":false,
        "thread":201,"message_id":203,"channel":"team/roadmap","actor":"first","author":"local","text":"ignored","created_at":1})).unwrap();
    let mut b: Binding = serde_json::from_value(binding("first")).unwrap();
    let text = worker::notice(&b, &item);
    assert!(text.starts_with("NeoBoard inbox notification 9 for @first on board board (project team). mention in channel team/roadmap, thread #201, message #203. Use your scoped NeoBoard MCP connection for this board only."), "{text}");
    assert!(!text.contains("ignored"));
    b.target.harness = "claude".into();
    assert!(worker::notice(&b, &item).contains("message #203. Use the neoboard-wake MCP connection that NeoBoard attached to this turn for this board only."));
}
