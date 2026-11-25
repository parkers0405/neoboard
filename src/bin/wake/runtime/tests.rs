use super::*;
use std::{fs, net::TcpListener};
use tempfile::TempDir;

const SID: &str = "00000000-0000-4000-8000-000000000017";
const PROMPT: &str = "Check NeoBoard task #17; $(not-a-shell-command)";

fn local(dir: &TempDir) -> Target {
    Target {
        harness: "neoism".into(),
        session: "ses_fixture-1".into(),
        workspace: dir.path().into(),
        executable: None,
        origin: Some("http://127.0.0.1".into()),
    }
}

#[test]
fn binding_rejects_ambiguous_ids_switches_paths_and_origins() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = local(&dir);
    assert!(validate(&t).is_ok());
    for origin in ["http://localhost", "https://example.org", "http://2130706433", "http://user@127.0.0.1", "http://127.0.0.1:65536", "http://127.0.0.1:0", "http://127.0.0.1?", "http://127.0.0.1/#x", "http://127.0.0.1/foo/..", "http://127.0.0.1\\@example.org"] {
        t.origin = Some(origin.into());
        assert!(validate(&t).is_err());
    }
    t.origin = Some("https://[::1]:443/".into());
    assert!(validate(&t).is_ok());
    for session in ["latest", "ses_", "ses_a/b", "ses_a%2fb", "ses_én", "ses_a\n"] {
        t.session = session.into();
        assert!(validate(&t).is_err());
    }
    t.harness = "codex".into();
    t.origin = None;
    t.executable = Some("codex".into());
    t.session = SID.into();
    assert!(validate(&t).is_err());
    t.executable = Some(std::env::current_exe().unwrap());
    t.session = "--latest".into();
    assert!(validate(&t).is_err());
    t.session = SID.into();
    t.workspace = ".".into();
    assert!(validate(&t).is_err());
}

// A single-use numeric-loopback HTTP fixture. Read the exact request and
// assert on it in the server thread; never connect to an installed runtime.
fn http_fixture(status: &'static str, body: Vec<u8>, post: bool) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(SHORT)).unwrap();
        let mut r = BufReader::new(&stream);
        let mut lines = String::new();
        while !lines.ends_with("\r\n\r\n") {
            assert_ne!(r.read_line(&mut lines).unwrap(), 0);
            assert!(lines.len() <= 8192);
        }
        let path = if post { "POST /v2/sessions/ses_fixture-1/prompt-if-idle" } else { "GET /v2/sessions/ses_fixture-1" };
        assert_eq!(lines.lines().next().unwrap(), format!("{path} HTTP/1.1"));
        assert!(lines.lines().any(|s| s.eq_ignore_ascii_case("authorization: Bearer fake-runtime-token")));
        if post {
            let len = lines.lines().find_map(|s| s.to_ascii_lowercase().strip_prefix("content-length: ").map(|n| n.parse::<usize>().unwrap())).unwrap();
            assert!(len < 1024);
            let mut b = vec![0; len];
            r.read_exact(&mut b).unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&b).unwrap(), json!({"prompt":PROMPT}));
        }
        write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        let _ = stream.write_all(&body); // client may stop early at its byte limit
    });
    (origin, server)
}

#[test]
fn conditional_admission_and_probe_require_exact_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = local(&dir);
    let token = Some("fake-runtime-token");
    for (status, body, expected) in [
        ("204 No Content", vec![], Outcome::Accepted),
        ("412 Precondition Failed", br#"{"code":"session.not_idle"}"#.to_vec(), Outcome::Busy),
        ("412 Precondition Failed", br#"{"code":"other"}"#.to_vec(), Outcome::Uncertain),
        ("412 Precondition Failed", [br#"{"code":"session.not_idle"}"#.as_slice(), &vec![b' '; 8192]].concat(), Outcome::Uncertain),
        ("200 OK", vec![], Outcome::Uncertain),
        ("307 Temporary Redirect\r\nLocation: /must-not-follow", vec![], Outcome::Uncertain),
    ] {
        let (origin, server) = http_fixture(status, body, true);
        t.origin = Some(origin);
        assert_eq!(send(&t, PROMPT, token, None), expected);
        server.join().unwrap();
    }
    for (id, ok) in [("ses_fixture-1", true), ("ses_other", false)] {
        let (origin, server) = http_fixture("200 OK", json!({"id":id}).to_string().into_bytes(), false);
        t.origin = Some(origin);
        assert_eq!(probe(&t, token).is_ok(), ok);
        server.join().unwrap();
    }
    assert!(probe(&t, None).is_err());
}

#[cfg(unix)]
fn fixture(harness: &str) -> (TempDir, Target) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("fake-runtime");
    fs::write(&exe, include_str!("fixture.sh")).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
    let target = Target {
        harness: harness.into(), session: SID.into(), workspace: dir.path().into(),
        executable: Some(exe), origin: None,
    };
    (dir, target)
}

#[cfg(unix)]
#[test]
fn cli_probe_does_not_prompt_and_claude_requires_matching_success_and_exit() {
    let (dir, t) = fixture("claude");
    probe(&t, None).unwrap();
    assert!(!dir.path().join("prompt").exists());
    assert_eq!(send(&t, PROMPT, None, None), Outcome::Accepted);
    assert_eq!(fs::read_to_string(dir.path().join("prompt")).unwrap(), PROMPT);
    for mode in ["wrong", "fail", "exit", "badjson", "large"] {
        fs::write(dir.path().join("mode"), mode).unwrap();
        assert_eq!(send(&t, PROMPT, None, None), Outcome::Uncertain, "{mode}");
    }
    fs::remove_file(dir.path().join("prompt")).unwrap();
    assert_eq!(send(&t, "--permission-mode", None, None), Outcome::Uncertain);
    assert!(!dir.path().join("prompt").exists());
}

#[cfg(unix)]
#[test]
fn codex_resumes_only_the_bound_thread_and_never_answers_server_requests() {
    let (dir, t) = fixture("codex");
    assert_eq!(send(&t, PROMPT, None, None), Outcome::Accepted);
    let msg: Value = serde_json::from_slice(&fs::read(dir.path().join("prompt")).unwrap()).unwrap();
    assert_eq!(msg["params"], json!({"threadId":SID,"input":[{"type":"text","text":PROMPT}]}));
    fs::remove_file(dir.path().join("prompt")).unwrap();
    for mode in ["wrong", "approval-before"] {
        fs::write(dir.path().join("mode"), mode).unwrap();
        assert_eq!(send(&t, PROMPT, None, None), Outcome::Uncertain);
        assert!(!dir.path().join("prompt").exists());
    }
    for mode in ["bad-turn", "bad-thread", "fail", "approval-after", "exit"] {
        fs::write(dir.path().join("mode"), mode).unwrap();
        assert_eq!(send(&t, PROMPT, None, None), Outcome::Uncertain);
        assert!(!dir.path().join("approved").exists());
    }
}

#[cfg(unix)]
#[test]
fn stuck_child_times_out_even_with_open_pipes() {
    let (dir, t) = fixture("codex");
    fs::write(dir.path().join("mode"), "hang").unwrap();
    let start = Instant::now();
    assert!(cli(&t, Some(PROMPT), None, Duration::from_millis(200)).is_err());
    assert!(start.elapsed() < Duration::from_secs(5));
}
// Claude Code reports "Not logged in" on macOS when USER is absent, so the
// cleared runtime environment must still carry the account identity.
#[cfg(unix)]
#[test]
fn runtime_environment_keeps_account_identity_but_not_board_credentials() {
    let (_dir, t) = fixture("claude");
    let cmd = runtime_command(&t).unwrap();
    let passed = |k: &str| cmd.get_envs().any(|(name, value)| name == k && value.is_some());
    for k in ["HOME", "USER", "LOGNAME"] {
        assert_eq!(passed(k), std::env::var_os(k).is_some(), "{k}");
    }
    assert!(std::env::var_os("USER").is_some(), "test needs USER to be meaningful");
    assert!(!passed("NEOCHAT_TOKEN"));
}

// A headless Claude turn gets the paired actor's own connection and only the listed
// approvals; the prompt stays a separate final argument, and Codex gets neither.
#[cfg(unix)]
#[test]
fn claude_turn_attaches_the_agent_grant_and_only_the_listed_approvals() {
    let (dir, t) = fixture("claude");
    let grant = dir.path().join("agent.json");
    let access = |allow: &[&str]| Access { adapter: t.executable.clone().unwrap(), grant: grant.clone(), allow: allow.iter().map(|s| s.to_string()).collect() };
    assert_eq!(send(&t, PROMPT, None, Some(&access(&["mcp__neoboard-wake", "Bash('/x y/neoboard' wake ack --code 1)"]))), Outcome::Accepted);
    assert_eq!(fs::read_to_string(dir.path().join("prompt")).unwrap(), PROMPT);
    let options = fs::read_to_string(dir.path().join("options")).unwrap();
    let lines: Vec<&str> = options.lines().collect();
    assert_eq!(lines[0], "--mcp-config");
    let config: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(config, json!({"mcpServers":{"neoboard-wake":{"command":t.executable,"args":["--agent-invite",grant]}}}));
    assert_eq!(lines[2..], ["--allowedTools", "mcp__neoboard-wake", "Bash('/x y/neoboard' wake ack --code 1)"]);
    fs::remove_file(dir.path().join("prompt")).unwrap();
    for bad in ["--dangerously-skip-permissions", "", "a\nb"] {
        assert_eq!(send(&t, PROMPT, None, Some(&access(&[bad]))), Outcome::Uncertain, "{bad:?}");
    }
    let mut missing = access(&[]);
    missing.adapter = dir.path().join("absent");
    assert_eq!(send(&t, PROMPT, None, Some(&missing)), Outcome::Uncertain);
    assert!(!dir.path().join("prompt").exists());
    let (dir, t) = fixture("codex");
    assert_eq!(send(&t, PROMPT, None, Some(&access(&["mcp__neoboard-wake"]))), Outcome::Accepted);
    assert!(!dir.path().join("options").exists());
}
