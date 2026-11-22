//! Exercise the host CLI against a disposable loopback stub, not a running board.
use axum::{http::{HeaderMap, StatusCode}, routing::{get, post}, Json, Router};
use serde_json::{json, Value};
use std::{io::Write, process::{Command, Output, Stdio}, sync::mpsc};

fn cli(args: &[&str], input: &[u8]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_neochat"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("NEOCHAT_") { command.env_remove(key); }
    }
    let mut child = command.args(args).env("NEOBOARD_PORTABLE_HANDOFF", "1")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn password_cli_set_and_disable_use_admin_credentials_and_redact_responses() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (requests, rx) = mpsc::channel();
    let app = Router::new().route("/v1/events", get(|| async { Json(json!([])) }))
        .route("/v1/admin/board-password", post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let requests = requests.clone();
            async move {
                requests.send((headers["authorization"].to_str().unwrap().to_owned(), body.clone())).unwrap();
                if body["password"].is_null() { (StatusCode::OK, Json(json!({"secret":"private-token"}))) }
                else { (StatusCode::FORBIDDEN, Json(json!({"error":"private-token or private password"}))) }
            }
        }));
    let listener = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    rt.spawn(async move { axum::serve(listener, app).await.unwrap(); });

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("endpoint.json"), json!({"url":url,"token":"member"}).to_string()).unwrap();
    std::fs::write(dir.path().join("admin-token"), "local-admin\n").unwrap();
    let output = cli(&["--data-dir",dir.path().to_str().unwrap(),"board-password","set","team","--password-stdin"], b" private password \r\n");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(String::from_utf8_lossy(&output.stderr).trim(), "neochat: board password request failed (HTTP 403)");
    assert_eq!(rx.try_recv().unwrap(), ("Bearer local-admin".into(), json!({"project":"team","password":" private password "})));

    let output = cli(&["--url", &url, "--token", "explicit-admin", "--json", "board-password", "disable", "team"], b"");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(serde_json::from_slice::<Value>(&output.stdout).unwrap(), json!({"enabled":false}));
    assert_eq!(rx.try_recv().unwrap(), ("Bearer explicit-admin".into(), json!({"project":"team","password":null})));
}

#[test]
fn password_cli_refuses_implicit_stdin_and_secret_arguments() {
    for extra in [None, Some("private-positional"), Some("--password=private-flag")] {
        let mut args = vec!["--url", "http://127.0.0.1:1", "--token", "admin", "board-password", "set", "team"];
        args.extend(extra);
        let output = cli(&args, b"");
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(if extra.is_none() { "--password-stdin" } else { "invalid board-password arguments" }));
        assert!(!error.contains("private"));
    }
}

#[test]
fn init_rejects_remote_admin_unsafe_addresses_and_secret_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let mut flags = vec!["--data-dir", dir.path().to_str().unwrap(), "init", "--board", "team"];
        flags.extend_from_slice(args);
        let output = cli(&flags, b"");
        assert!(!output.status.success() && output.stdout.is_empty());
        let err = String::from_utf8(output.stderr).unwrap();
        assert!(!err.contains("private") && !err.contains("#neoboard="));
        err
    };
    for url in ["http://host/", "https://private@host/", "https://host/?private", "https://host/#private"] {
        assert!(run(&["--public-url", url]).contains("HTTPS"));
    }
    assert!(run(&["--url", "https://host/", "--token", "private"]).contains("local daemon"));
    assert!(run(&["--password=private"]).contains("invalid init arguments"));
}