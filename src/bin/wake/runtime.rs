//! Synchronous, single-attempt dispatch. Bindings (including the workspace, CLI,
//! its runtime profile and helper PATH) must come from a trusted operator, not a task.

use reqwest::{blocking::Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Take, Write},
    path::{Path, PathBuf},
    process::{ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Target {
    pub harness: String,
    pub session: String,
    pub workspace: PathBuf,
    pub executable: Option<PathBuf>,
    pub origin: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Accepted,
    Busy,
    Uncertain,
}

/// Claude Code only. A `--print` turn cannot answer permission prompts and the
/// user's own MCP entries may act as someone else, so attach the paired actor's
/// grant under a fixed name and pre-approve exactly `allow`, nothing broader.
pub struct Access {
    pub adapter: PathBuf,
    pub grant: PathBuf,
    pub allow: Vec<String>,
}

pub const CONNECTION: &str = "neoboard-wake";

const LIMIT: u64 = 4 * 1024 * 1024;
const SHORT: Duration = Duration::from_secs(10);
const TURN: Duration = Duration::from_secs(300);

fn uuid(s: &str) -> bool {
    s.len() == 36 && s.bytes().enumerate().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => c == b'-',
        _ => c.is_ascii_digit() || (b'a'..=b'f').contains(&c),
    })
}

// Check the original spelling, not a URL parser's normalized host/path. In
// particular don't accept integer IPs, dot segments, userinfo or backslashes.
fn loopback(origin: &str) -> bool {
    let Some(authority) = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) else { return false };
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    let Some(suffix) = authority.strip_prefix("127.0.0.1").or_else(|| authority.strip_prefix("[::1]")) else { return false };
    suffix.is_empty() || suffix.strip_prefix(':').is_some_and(|p| {
        !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && matches!(p.parse::<u16>(), Ok(1..))
    })
}

fn check_path(path: &Path, executable: bool) -> Result<(), String> {
    if !path.is_absolute() { return Err("expected an absolute runtime path".into()); }
    let meta = path.metadata().map_err(|_| "runtime path unavailable")?;
    if executable && !meta.is_file() || !executable && !meta.is_dir() {
        return Err("invalid runtime path type".into());
    }
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        if (meta.uid() != 0 && meta.uid() != unsafe { libc::geteuid() }) ||
            meta.mode() & 0o022 != 0 || executable && meta.mode() & 0o111 == 0 {
            return Err("runtime path untrusted or not executable".into());
        }
    }
    // Never route a prompt through cmd.exe / a batch file.
    #[cfg(windows)]
    if executable && !path.extension().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case("exe")) {
        return Err("expected a native .exe runtime".into());
    }
    Ok(())
}

pub fn validate(t: &Target) -> Result<(), String> {
    check_path(&t.workspace, false)?;
    match t.harness.as_str() {
        "neoism" => {
            if t.executable.is_some() || !t.origin.as_deref().is_some_and(loopback) {
                return Err("expected a numeric loopback runtime origin and no executable".into());
            }
            if !t.session.strip_prefix("ses_").is_some_and(|id| (1..=128).contains(&id.len()) &&
                id.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))) {
                return Err("expected a concrete Neoism session ID".into());
            }
        }
        "codex" | "claude" => {
            if t.origin.is_some() || !uuid(&t.session) { return Err("expected an exact session UUID and no origin".into()); }
            check_path(t.executable.as_deref().ok_or("missing runtime executable")?, true)?;
        }
        _ => return Err("unsupported wake harness".into()),
    }
    Ok(())
}

fn bounded(reader: impl Read, limit: u64) -> Result<Vec<u8>, ()> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes).map_err(|_| ())?;
    (bytes.len() as u64 <= limit).then_some(bytes).ok_or(())
}

fn neoism(t: &Target, token: Option<&str>, prompt: Option<&str>) -> Result<Outcome, ()> {
    let token = token.filter(|v| !v.is_empty() && v.len() <= 8192 && v.bytes().all(|b| (33..=126).contains(&b))).ok_or(())?;
    let url = format!("{}/v2/sessions/{}", t.origin.as_deref().ok_or(())?.trim_end_matches('/'), t.session);
    let client = Client::builder().no_proxy().redirect(Policy::none()).retry(reqwest::retry::never())
        .timeout(SHORT).build().map_err(|_| ())?;
    let req = match prompt {
        None => client.get(url),
        Some(text) => client.post(format!("{url}/prompt-if-idle")).json(&json!({"prompt":text})),
    };
    let response = req.bearer_auth(token).send().map_err(|_| ())?;
    let status = response.status().as_u16();
    match (prompt.is_some(), status) {
        (true, 204) => Ok(Outcome::Accepted),
        (true, 412) | (false, 200) => {
            let bytes = bounded(response, if status == 412 { 8192 } else { LIMIT })?;
            let body: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
            if status == 412 && body["code"] == "session.not_idle" { Ok(Outcome::Busy) }
            else if status == 200 && body["id"].as_str() == Some(&t.session) { Ok(Outcome::Accepted) }
            else { Err(()) }
        }
        _ => Err(()),
    }
}

type Records = BufReader<Take<ChildStdout>>;

fn record(out: &mut Records) -> Result<Option<Value>, ()> {
    let mut bytes = Vec::new();
    let n = out.read_until(b'\n', &mut bytes).map_err(|_| ())?;
    if out.get_ref().limit() == 0 { return Err(()); } // total conversation budget, not per line
    if n == 0 { return Ok(None); }
    let msg: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if !msg.is_object() { return Err(()); }
    Ok(Some(msg))
}

fn write_msg(input: &mut ChildStdin, msg: Value) -> Result<(), ()> {
    serde_json::to_writer(&mut *input, &msg).map_err(|_| ())?;
    input.write_all(b"\n").and_then(|_| input.flush()).map_err(|_| ())
}

fn request(input: &mut ChildStdin, out: &mut Records, id: u64, method: &str, params: Value) -> Result<Value, ()> {
    write_msg(input, json!({"id":id, "method":method, "params":params}))?;
    while let Some(mut msg) = record(out)? {
        if msg.get("id").is_none() { continue; }
        // Never answer server-initiated requests, including permission prompts.
        if msg["id"].as_u64() != Some(id) || msg.get("method").is_some() || msg.get("error").is_some() || !msg["result"].is_object() { return Err(()); }
        return Ok(msg["result"].take());
    }
    Err(())
}

fn codex(mut input: ChildStdin, output: ChildStdout, session: &str, prompt: &str) -> Result<(), ()> {
    let mut out = BufReader::new(output.take(LIMIT + 1));
    request(&mut input, &mut out, 1, "initialize", json!({"clientInfo":{"name":"neoboard","version":"1"}}))?;
    write_msg(&mut input, json!({"method":"initialized","params":{}}))?;
    let resume = request(&mut input, &mut out, 2, "thread/resume", json!({"threadId":session}))?;
    if resume["thread"]["id"] != session { return Err(()); }
    let start = request(&mut input, &mut out, 3, "turn/start", json!({"threadId":session,"input":[{"type":"text","text":prompt}]}))?;
    let turn = start["turn"]["id"].as_str().filter(|id| !id.is_empty()).ok_or(())?;
    while let Some(msg) = record(&mut out)? {
        if msg.get("id").is_some() { return Err(()); }
        if msg["method"] == "turn/completed" {
            let p = &msg["params"];
            if p["threadId"] != session || p["turn"]["id"] != turn || p["turn"]["status"] != "completed" { return Err(()); }
            drop(input); // EOF tells the stdio server to exit; drain without answering requests.
            while let Some(msg) = record(&mut out)? {
                if msg.get("id").is_some() { return Err(()); }
            }
            return Ok(());
        }
    }
    Err(())
}

fn runtime_command(t: &Target) -> Result<Command, ()> {
    let mut cmd = Command::new(t.executable.as_deref().ok_or(())?);
    cmd.current_dir(&t.workspace).env_clear();
    // Allowlist rather than guessing all current/future board credential names.
    // Runtime credentials are managed by the selected CLI's own profile.
    // USER is required: without it Claude Code cannot find its macOS keychain login.
    for k in ["HOME", "USER", "LOGNAME", "PATH", "TMPDIR", "TMP", "TEMP", "LANG", "LC_ALL", "USERPROFILE", "APPDATA",
        "LOCALAPPDATA", "SystemRoot", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME",
        "CODEX_HOME", "CLAUDE_CONFIG_DIR"] {
        if let Some(v) = std::env::var_os(k) { cmd.env(k, v); }
    }
    #[cfg(unix)] {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    Ok(cmd)
}

fn claude_access(a: &Access) -> Result<(String, &[String]), ()> {
    check_path(&a.adapter, true).map_err(|_| ())?;
    if !a.grant.is_absolute() || a.allow.iter().any(|rule| rule.is_empty() || rule.starts_with('-') || rule.chars().any(char::is_control)) { return Err(()); }
    let (adapter, grant) = (a.adapter.to_str().ok_or(())?, a.grant.to_str().ok_or(())?);
    Ok((json!({"mcpServers":{CONNECTION:{"command":adapter,"args":["--agent-invite",grant]}}}).to_string(), &a.allow))
}

fn cli(t: &Target, prompt: Option<&str>, access: Option<&Access>, timeout: Duration) -> Result<(), ()> {
    let deadline = Instant::now() + timeout;
    let mut cmd = runtime_command(t)?;
    let rpc = prompt.is_some() && t.harness == "codex";
    match prompt {
        None => { cmd.arg("--version"); }
        Some(_) if rpc => { cmd.arg("app-server"); }
        Some(text) => {
            cmd.args(["--resume", &t.session, "--print", "--output-format", "json"]);
            if let Some(access) = access {
                let (config, allow) = claude_access(access)?;
                cmd.arg("--mcp-config").arg(config);
                if !allow.is_empty() { cmd.arg("--allowedTools").args(allow); }
            }
            // Both options above take a list; end it so the prompt is never read as a value.
            cmd.arg("--").arg(text);
        }
    }
    let mut child = cmd.stdin(if rpc { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|_| ())?;
    let input = child.stdin.take();
    let output = child.stdout.take().expect("piped stdout");
    let session = t.session.clone();
    let prompt = prompt.map(str::to_owned);
    // Both reads and writes can block. Supervise from outside the protocol so a
    // wedged app-server (or inherited pipe) cannot hold the wake loop forever.
    let (tx, rx) = mpsc::channel();
    let worker = thread::Builder::new().spawn(move || {
        let result = match (input, prompt) {
            (Some(stdin), Some(text)) => codex(stdin, output, &session, &text),
            (_, Some(_)) => bounded(output, LIMIT).and_then(|bytes| {
                let v: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
                (v["type"] == "result" && v["subtype"] == "success" && v["is_error"] == false && v["session_id"] == session).then_some(()).ok_or(())
            }),
            (_, None) => bounded(output, 8192).and_then(|b| (!b.is_empty()).then_some(()).ok_or(())),
        };
        let _ = tx.send(result);
    });
    if worker.is_ok() && matches!(rx.recv_timeout(deadline.saturating_duration_since(Instant::now())), Ok(Ok(()))) {
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(status)) => return status.success().then_some(()).ok_or(()),
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(_) => break,
            }
        }
    }
    #[cfg(unix)]
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL); } // our unreaped child's private group
    let _ = child.kill();
    let _ = child.wait();
    Err(())
}

pub fn send(t: &Target, prompt: &str, neoism_token: Option<&str>, access: Option<&Access>) -> Outcome {
    if validate(t).is_err() || prompt.trim().is_empty() || prompt.len() > 16 * 1024 || prompt.starts_with('-') || prompt.contains('\0') {
        return Outcome::Uncertain;
    }
    if t.harness == "neoism" { neoism(t, neoism_token, Some(prompt)).unwrap_or(Outcome::Uncertain) }
    else if cli(t, Some(prompt), access.filter(|_| t.harness == "claude"), TURN).is_ok() { Outcome::Accepted }
    else { Outcome::Uncertain }
}

/// For CLIs, confirms only that the bound executable can run in the workspace.
/// For Neoism, requires the authenticated GET to return the exact session ID.
pub fn probe(t: &Target, neoism_token: Option<&str>) -> Result<(), String> {
    validate(t)?;
    if t.harness == "neoism" { neoism(t, neoism_token, None).map(|_| ()) } else { cli(t, None, None, SHORT) }
        .map_err(|_| "runtime probe not confirmed".into())
}

#[cfg(test)]
#[path = "runtime/tests.rs"]
mod tests;