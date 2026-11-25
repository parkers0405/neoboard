use super::*;
use std::{io::{Read as _, Write}, net::TcpListener, thread};

fn board(actor: &'static str, requests: usize) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    (origin, thread::spawn(move || for _ in 0..requests {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0u8; 2048];
        let n = stream.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..n]).contains("authorization: Bearer agent-key") || String::from_utf8_lossy(&request[..n]).contains("Authorization: Bearer agent-key"));
        let body = format!(r#"{{"actor":"{actor}","cursor":0,"items":[]}}"#);
        write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).unwrap();
    }))
}

#[test]
fn a_restarted_local_board_is_followed_only_for_the_same_actor() {
    let dir = tempfile::tempdir().unwrap();
    let grant = dir.path().join("agent.json");
    let old = "http://127.0.0.1:1";
    let write = |endpoint: &str| std::fs::write(&grant, serde_json::json!({"version":1,"project":"team","endpoint":endpoint,
        "token":"agent-key","expires_at":neochat::board::now()+600}).to_string()).unwrap();
    let discovery = |url: &str| std::fs::write(dir.path().join("endpoint.json"), serde_json::json!({"url":url,"token":"host-key"}).to_string()).unwrap();
    let source = |origin: &str| Source { chat_id: "default".into(), board: "board".into(), project: "team".into(), invite: grant.clone(),
        origin: origin.into(), actor: "first".into(), harness: "claude".into(), local: Some(dir.path().into()) };
    write(old);

    discovery(old); // unchanged address: no request is made
    assert_eq!(relocated(&source(old)), None);

    let (moved, server) = board("first", 2);
    discovery(&moved);
    assert_eq!(relocated(&source(old)).as_deref(), Some(moved.as_str()));
    readdress(&grant, &moved).unwrap();
    // Grant already rewritten but the binding not yet: still repaired, and the credential is kept.
    assert_eq!(relocated(&source(old)).as_deref(), Some(moved.as_str()));
    server.join().unwrap();
    assert_eq!(saved_origin(&grant, "team").unwrap(), moved);
    assert_eq!(relocated(&source(&moved)), None);

    write(old);
    let (other, server) = board("someone-else", 1);
    discovery(&other);
    assert_eq!(relocated(&source(old)), None);
    server.join().unwrap();

    discovery("https://board.example/"); // never follow discovery off this machine
    assert_eq!(relocated(&source(old)), None);
    assert_eq!(relocated(&source("https://board.example")), None);
}
