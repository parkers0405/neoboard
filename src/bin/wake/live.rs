//! Deliver a wake into the conversation the user has open, not a hidden second run.
//!
//! Claude Code: an `asyncRewake` hook runs `wake watch` in the background of the
//! paired conversation. It blocks until that conversation's actor has a delivery,
//! prints the notice and exits 2, and Claude Code starts a turn in the open window.
//! Codex needs no watcher: `codex queue` already reaches its shared local daemon.
//! Either way the runtime gets this binding's own board connection installed.
use super::{inbox, quoted_exe, runtime, store::{self, transaction, Binding}, worker};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io::{Read, Write}, path::Path, thread, time::{Duration, Instant}};

const WATCH: &str = " wake watch";
/// Stop on our own well before the hook's timeout so the state says "expired"
/// rather than leaving a killed watcher that looks armed.
const HOOK_TIMEOUT: u64 = 12 * 3600;
const WATCH_FOR: Duration = Duration::from_secs(11 * 3600);

/// One installed connection per binding: several conversations can share a workspace
/// (Claude Code) or a whole CLI profile (Codex) while acting as different actors.
pub fn connection(b: &Binding) -> String {
    let key = format!("{}\0{}\0{}", b.source.board, b.source.harness, b.source.chat_id);
    format!("{}-{}", runtime::CONNECTION, &format!("{:x}", Sha256::digest(key.as_bytes()))[..8])
}

#[derive(Serialize, Deserialize)]
struct Marker { pid: u32, state: String }

fn stem(session: &str) -> String { format!("live-{:x}", Sha256::digest(session.as_bytes())) }

fn alive(pid: u32) -> bool {
    #[cfg(unix)] { pid > 1 && unsafe { libc::kill(pid as i32, 0) } == 0 }
    #[cfg(not(unix))] { let _ = pid; false }
}

fn mark(dir: &Path, stem: &str, pid: u32, state: &str) {
    let write = || -> Option<()> {
        let mut tmp = tempfile::NamedTempFile::new_in(dir).ok()?;
        serde_json::to_writer(&mut tmp, &Marker { pid, state: state.into() }).ok()?;
        tmp.flush().ok()?;
        tmp.persist(dir.join(format!("{stem}.json"))).ok().map(|_| ())
    };
    let _ = write();
}

fn watching(dir: &Path, stem: &str) -> bool {
    let name = format!("{stem}.lock");
    dir.join(&name).exists() && store::lock(dir, &name, false).is_err()
}

/// True while the open conversation will take this binding's deliveries itself: its
/// Claude Code process is running and either a watcher is armed, or one just handed a
/// delivery over and the turn has not ended yet (the Stop hook arms the next watcher).
pub fn active(dir: &Path, b: &Binding) -> bool {
    if !b.live || b.target.harness != "claude" { return false; }
    let stem = stem(&b.target.session);
    let Ok(bytes) = std::fs::read(dir.join(format!("{stem}.json"))) else { return false };
    let Ok(marker) = serde_json::from_slice::<Marker>(&bytes) else { return false };
    alive(marker.pid) && (marker.state == "delivered" || watching(dir, &stem))
}

fn session() -> Option<String> {
    let mut bytes = Vec::new();
    let _ = std::io::stdin().take(64 * 1024).read_to_end(&mut bytes);
    serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| v["session_id"].as_str().map(str::to_owned))
        .or_else(|| std::env::var("CLAUDE_CODE_SESSION_ID").ok()).filter(|s| !s.is_empty())
}

/// What the watcher should hand to the open conversation for this binding, if anything.
fn next(dir: &Path, key: &str) -> Result<Option<String>, String> {
    let b = worker::binding(dir, key)?;
    if !b.live { return Ok(None); }
    if let (true, Some(code)) = (b.live_test, b.test.as_ref()) {
        let mine = transaction(dir, |s| { let cur = s.bindings.get_mut(key).ok_or("Not paired")?;
            let mine = cur.live_test && cur.test.as_ref() == Some(code); cur.live_test = false; Ok(mine) })?;
        if mine { return Ok(Some(worker::test_prompt(&b, &worker::ack_command(code)?))); }
    }
    if !b.enabled || b.uncertain.is_some() { return Ok(None); }
    let Some(item) = inbox::poll(&b.source, b.watermark)?.items.into_iter().next() else { return Ok(None) };
    worker::valid(&item, &b)?;
    let in_scope = item.channel.split('/').next() == Some(&b.source.project);
    let send = transaction(dir, |s| { let cur = s.bindings.get_mut(key).ok_or("Not paired")?;
        if !cur.enabled || cur.watermark != b.watermark || cur.uncertain.is_some() { return Ok(false); }
        cur.watermark = item.id;
        if in_scope { cur.last = format!("Delivery {} handed to the open conversation", item.id); }
        Ok(in_scope) })?;
    Ok(send.then(|| worker::notice(&b, &item)))
}

/// Hook body. Exit 2 with the notice on stderr starts a turn; every other outcome is a silent 0.
pub fn watch(dir: &Path) -> i32 {
    // A non-interactive run (including our own background wake of this conversation) has
    // no window to wake, and would wait on this hook instead of finishing.
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    if var("CLAUDE_CODE_SESSION_ATTENDED") == "0" || var("CLAUDE_CODE_ENTRYPOINT").starts_with("sdk") { return 0; }
    let Some(session) = session() else { return 0 };
    let Ok(keys) = transaction(dir, |s| Ok(s.bindings.iter()
        .filter(|(_, b)| b.target.harness == "claude" && b.target.session == session && b.live)
        .map(|(k, _)| k.clone()).collect::<Vec<_>>())) else { return 0 };
    if keys.is_empty() { return 0; }
    let stem = stem(&session);
    // One watcher per conversation: SessionStart and every Stop both try to arm one.
    let Ok(_only) = store::lock(dir, &format!("{stem}.lock"), false) else { return 0 };
    #[cfg(unix)] let parent = std::os::unix::process::parent_id();
    #[cfg(not(unix))] let parent = 0;
    let pid = std::env::var("CLAUDE_PID").ok().and_then(|p| p.parse().ok()).unwrap_or(parent);
    mark(dir, &stem, pid, "armed");
    let deadline = Instant::now() + WATCH_FOR;
    loop {
        if !alive(pid) { return 0; }
        if Instant::now() >= deadline { mark(dir, &stem, pid, "expired"); return 0; }
        for key in &keys {
            match next(dir, key) {
                Ok(Some(text)) => {
                    mark(dir, &stem, pid, "delivered");
                    eprintln!("{text}");
                    return 2;
                }
                Ok(None) => {}
                Err(e) if e == "Not paired" => {}
                Err(e) => { let _ = transaction(dir, |s| { if let Some(b) = s.bindings.get_mut(key) { b.last = e.clone(); } Ok(()) }); }
            }
        }
        thread::sleep(Duration::from_secs(3));
    }
}

fn ours(group: &Value) -> bool {
    group["hooks"].as_array().is_some_and(|hooks| hooks.iter().any(|h| h["command"].as_str().is_some_and(|c| c.ends_with(WATCH))))
}

/// Merge the watcher hooks and the two narrow approvals into the workspace's private
/// Claude Code settings, leaving everything else in the file as it was.
pub(super) fn claude_settings(existing: Option<&[u8]>, exe: &str, connection: &str) -> Result<Vec<u8>, String> {
    let refused = "its .claude/settings.local.json is not a JSON object";
    let mut doc: Value = match existing { Some(bytes) => serde_json::from_slice(bytes).map_err(|_| refused)?, None => json!({}) };
    if !doc.is_object() { return Err(refused.into()); }
    for section in ["hooks", "permissions"] {
        if doc.get(section).is_some_and(|v| !v.is_object()) { return Err(refused.into()); }
    }
    let hook = json!({"type":"command","command":format!("{exe}{WATCH}"),"asyncRewake":true,"timeout":HOOK_TIMEOUT});
    for (event, matcher) in [("SessionStart", Some("startup|resume|clear|compact")), ("Stop", None)] {
        let mut groups = match doc["hooks"].get(event) { None => vec![], Some(Value::Array(a)) => a.clone(), Some(_) => return Err(refused.into()) };
        groups.retain(|group| !ours(group));
        let mut group = json!({"hooks":[hook]});
        if let Some(m) = matcher { group["matcher"] = json!(m); }
        groups.push(group);
        doc["hooks"][event] = Value::Array(groups);
    }
    let mut allow = match doc["permissions"].get("allow") { None => vec![], Some(Value::Array(a)) => a.clone(), Some(_) => return Err(refused.into()) };
    for rule in [format!("mcp__{connection}"), format!("Bash({exe} wake ack --code *)")] {
        if !allow.iter().any(|v| v.as_str() == Some(&rule)) { allow.push(json!(rule)); }
    }
    doc["permissions"]["allow"] = Value::Array(allow);
    let mut bytes = serde_json::to_vec_pretty(&doc).map_err(|_| "cannot encode settings")?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Pre-approve only this connection's tools, by adding one key to the table Codex just
/// wrote for it. `None` when it is already set. The grant still limits what they can do.
pub(super) fn codex_approval(config: &str, connection: &str) -> Result<Option<String>, String> {
    const KEY: &str = "default_tools_approval_mode";
    let header = format!("[mcp_servers.{connection}]");
    let lines: Vec<&str> = config.split_inclusive('\n').collect();
    let mut found = lines.iter().enumerate().filter(|(_, l)| l.trim() == header);
    let (Some((at, _)), None) = (found.next(), found.next()) else { return Err("the Codex config has no single entry for this connection".into()) };
    let table = lines[at + 1..].iter().take_while(|l| !l.trim_start().starts_with('['));
    if table.clone().any(|l| l.trim_start().starts_with(KEY)) { return Ok(None); }
    let mut out = String::with_capacity(config.len() + 64);
    for (i, line) in lines.iter().enumerate() {
        out.push_str(line);
        if i == at {
            if !line.ends_with('\n') { out.push('\n'); }
            out.push_str(&format!("{KEY} = \"approve\"\n"));
        }
    }
    Ok(Some(out))
}

/// Give the paired runtime this binding's own connection and, for Claude Code, the watcher.
/// Idempotent. The running conversation picks both up the next time it starts.
pub fn install(b: &Binding) -> Result<(), String> {
    let name = connection(b);
    let adapter = worker::adapter()?;
    let (adapter, grant) = (adapter.to_str().ok_or("unsupported install path")?, b.source.invite.to_str().ok_or("unsupported grant path")?);
    match b.target.harness.as_str() {
        "claude" => {
            if cfg!(windows) { return Err("not yet supported on Windows".into()); }
            let exe = quoted_exe()?;
            let dir = b.target.workspace.join(".claude");
            let path = dir.join("settings.local.json");
            let existing = match std::fs::read(&path) { Ok(bytes) => Some(bytes), Err(e) if e.kind() == std::io::ErrorKind::NotFound => None, Err(_) => return Err("cannot read its .claude/settings.local.json".into()) };
            let settings = claude_settings(existing.as_deref(), &exe, &name)?;
            let _ = runtime::configure(&b.target, &["mcp", "remove", "-s", "local", &name]);
            runtime::configure(&b.target, &["mcp", "add", "-s", "local", &name, "--", adapter, "--agent-invite", grant])?;
            if existing.as_deref() != Some(&settings[..]) {
                std::fs::create_dir_all(&dir).map_err(|_| "cannot create its .claude folder")?;
                let mut tmp = tempfile::NamedTempFile::new_in(&dir).map_err(|_| "cannot write its .claude/settings.local.json")?;
                tmp.write_all(&settings).and_then(|_| tmp.flush()).map_err(|_| "cannot write its .claude/settings.local.json")?;
                tmp.persist(&path).map_err(|_| "cannot write its .claude/settings.local.json")?;
            }
            Ok(())
        }
        "codex" => {
            let _ = runtime::configure(&b.target, &["mcp", "remove", &name]);
            runtime::configure(&b.target, &["mcp", "add", &name, "--", adapter, "--agent-invite", grant])?;
            // `codex mcp add` has no flag for this, and a wake with no window open cannot answer a prompt.
            let home = std::env::var_os("CODEX_HOME").map(std::path::PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".codex"))).ok_or("cannot locate the Codex profile")?;
            let path = home.join("config.toml");
            let text = std::fs::read_to_string(&path).map_err(|_| "cannot read the Codex config")?;
            if let Some(next) = codex_approval(&text, &name)? {
                let mut tmp = tempfile::NamedTempFile::new_in(&home).map_err(|_| "cannot write the Codex config")?;
                tmp.write_all(next.as_bytes()).and_then(|_| tmp.flush()).map_err(|_| "cannot write the Codex config")?;
                tmp.persist(&path).map_err(|_| "cannot write the Codex config")?;
            }
            Ok(())
        }
        _ => Err("unsupported runtime".into()),
    }
}

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
