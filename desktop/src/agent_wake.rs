//! Wake controls never launch a harness or run a pairing command. The bundled CLI owns wake state.
use crate::{App, TextEdit, edit, setup_guide::Guide};
use std::{path::{Path, PathBuf}, process::Stdio, sync::mpsc};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Harness { Neoism, Claude, Codex }

impl Harness {
    pub fn name(self) -> &'static str {
        match self { Self::Neoism => "Neoism", Self::Claude => "Claude Code", Self::Codex => "Codex" }
    }
    fn arg(self) -> &'static str {
        match self { Self::Neoism => "neoism", Self::Claude => "claude", Self::Codex => "codex" }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action { Prepare, Create, Status, On, Off, Test }
impl Action {
    fn arg(self) -> &'static str {
        match self { Self::Prepare | Self::Create => "prepare", Self::Status => "status", Self::On => "on", Self::Off => "off", Self::Test => "test" }
    }
}

pub struct Panel {
    key: String,
    pub harness: Harness,
    pub invite: TextEdit,
    pub dedicated: bool,
    pub manual: bool,
    pub details: bool,
    pub command: Option<String>,
    pub status: Option<String>,
    pub test: Option<String>,
    pub notice: Option<String>,
    pub confirming: Option<Action>,
    pending: Option<(Action, mpsc::Receiver<Result<String, String>>)>,
}

impl Panel {
    fn new(key: String, harness: Harness) -> Self {
        Self { key, harness, invite: edit(""), dedicated: false, manual: false, details: false, command: None, status: None, test: None, notice: None, confirming: None, pending: None }
    }
    pub fn busy(&self) -> bool { self.pending.is_some() }
    pub fn matches_board(&self, key: &str) -> bool { self.key == key }
    pub fn poll(&mut self) -> bool {
        let Some((action, rx)) = &self.pending else { return false };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(_) => Err("No result received. Check status before retrying.".into()),
        };
        self.notice = None;
        match (action, result) {
            (Action::Test, Ok(msg) | Err(msg)) => self.test = Some(msg),
            (_, Err(msg)) => self.notice = Some(msg),
            (Action::Prepare | Action::Create, Ok(command)) => self.command = Some(command),
            (_, Ok(msg)) => {
                if paired(&msg).is_some() { self.command = None; }
                self.status = Some(msg);
            },
        }
        self.pending = None;
        true
    }
}

pub fn paired(status: &str) -> Option<bool> {
    match status.split([' ', '.']).next()? { "On" => Some(true), "Off" => Some(false), _ => None }
}

fn refresh(app: &mut App, harness: Harness) {
    if app.fixture { app.agent_wake.as_mut().unwrap().status = Some("Not paired".into()); }
    else { let key = Guide::new(app).key; start(app, &key, harness, Action::Status); }
}

pub fn open(app: &mut App) {
    app.setup_choices = false;
    app.home_copied = None;
    let harness = match app.setup_agent {
        crate::setup_guide::Agent::ClaudeCode => Harness::Claude,
        crate::setup_guide::Agent::Codex => Harness::Codex,
        crate::setup_guide::Agent::Neoism => Harness::Neoism,
        crate::setup_guide::Agent::ClaudeDesktop => {app.agent_wake=None; return;},
    };
    app.agent_wake = Some(Panel::new(Guide::new(app).key, harness));
    refresh(app, harness);
}

pub fn select(app: &mut App, harness: Harness) {
    if let Some(panel) = &mut app.agent_wake && !panel.busy() && panel.harness != harness {
        *panel = Panel::new(panel.key.clone(), harness);
        app.setup_agent = match harness { Harness::Neoism=>crate::setup_guide::Agent::Neoism, Harness::Claude=>crate::setup_guide::Agent::ClaudeCode, Harness::Codex=>crate::setup_guide::Agent::Codex };
        app.home_copied = None;
        refresh(app, harness);
    }
}

pub fn selected(app: &App, key: &str, harness: Harness) -> bool {
    app.setup_open && !key.is_empty()
        && app.agent_wake.as_ref().is_some_and(|p| p.key == key && p.harness == harness && !p.busy()) && Guide::new(app).key == key
}

/// Provision only on the same local discovery route that supplied the live board list.
/// The CLI rechecks the pinned directory and loopback endpoint before using admin access.
pub fn local_dir(app: &App) -> Option<PathBuf> {
    let target = crate::setup_guide::target(app)?;
    if !(app.fixture || app.status == "live") || !app.can_share_board(&target.profile.project) { return None; }
    target.local.filter(|dir| app.fixture || app.local_discovery.as_ref().is_some_and(|(verified, _)| verified == dir))
}

enum Source {
    Invite { path: PathBuf, membership: Option<PathBuf>, endpoint: Option<String> },
    Local { dir: PathBuf, endpoint: String },
}

fn source(app: &App, action: Action) -> Result<Option<Source>, &'static str> {
    let target = crate::setup_guide::target(app).ok_or("Choose an available board connection.")?;
    if action == Action::Create {
        return local_dir(app).zip(app.local_discovery.as_ref()).map(|(dir, (_, endpoint))| Some(Source::Local { dir, endpoint: endpoint.clone() })).ok_or("Creating an identity requires the selected board on the live local connection.");
    }
    if action != Action::Prepare {
        if !crate::setup_guide::copy_allowed(app, Some(&target)) { return Err("The selected connection is no longer available."); }
        return Ok(None);
    }
    let panel = app.agent_wake.as_ref().ok_or("Open Agent wake first.")?;
    let membership = target.profile.invite;
    let path = membership.clone().filter(|_| !panel.dedicated).unwrap_or_else(|| PathBuf::from(&panel.invite.text));
    if !path.is_absolute() { return Err("Choose a scoped invite file by its full path first."); }
    let endpoint = if let Some(dir) = target.local {
        Some(app.local_discovery.as_ref().filter(|(verified, _)| verified == &dir && !app.remote() && app.status == "live").ok_or("Could not verify the selected local endpoint.")?.1.clone())
    } else { target.profile.endpoint };
    Ok(Some(Source::Invite { path, membership, endpoint }))
}

// Read only the values needed to bind a remote selection. The CLI owns invite format,
// transport (including numeric loopback HTTP), grants and server actor validation.
fn invite_endpoint(path: &Path, project: &str) -> Result<reqwest::Url, &'static str> {
    use std::{fs, io::Read};
    #[derive(serde::Deserialize)]
    struct Scope { project: Option<String>, endpoint: String, expires_at: u64 }
    const INVALID: &str = "Choose a valid, unexpired invite scoped to the selected board.";
    if !path.is_absolute() || !fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) { return Err(INVALID); }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    fs::File::open(path).and_then(|f| f.take(8193).read_to_end(&mut bytes)).map_err(|_| INVALID)?;
    if bytes.len() > 8192 { return Err(INVALID); }
    let s: Scope = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    if s.project.as_deref() != Some(project) || s.expires_at <= neochat::board::now() { return Err(INVALID); }
    reqwest::Url::parse(&s.endpoint).map_err(|_| INVALID)
}

impl Source {
    fn args(&self, cmd: &mut std::process::Command, project: &str) -> Result<(), &'static str> {
        cmd.args(["--project", project]);
        match self {
            Self::Local { dir, endpoint } => { cmd.arg("--local-data-dir").arg(dir).arg("--local-endpoint").arg(endpoint).arg("--create-agent"); }
            Self::Invite { path, membership, endpoint } => {
                if membership.is_some() || endpoint.is_some() {
                    const MISMATCH: &str = "Choose an invite for this board on the same saved endpoint.";
                    let target = invite_endpoint(path, project)?;
                    if let Some(saved) = membership && saved != path && invite_endpoint(saved, project)? != target { return Err(MISMATCH); }
                    if let Some(url) = endpoint && reqwest::Url::parse(url).ok().as_ref() != Some(&target) { return Err(MISMATCH); }
                }
                cmd.arg("--invite").arg(path);
            }
        }
        Ok(())
    }
}

pub fn start(app: &mut App, key: &str, harness: Harness, action: Action) {
    if !selected(app, key, harness) || app.fixture { return; }
    let Some(target) = crate::setup_guide::target(app) else { return };
    let Some(wake) = app.connection_wake.clone() else { return };
    let source = source(app, action);
    let panel = app.agent_wake.as_mut().unwrap();
    let source = match source { Ok(s) => s, Err(msg) => { panel.command = None; panel.confirming = None; panel.notice = Some(msg.into()); return; } };
    if matches!(action, Action::Create | Action::On) && panel.confirming != Some(action) { panel.confirming = Some(action); panel.notice = None; return; }
    panel.confirming = None;
    let project = target.profile.project;
    let key = key.to_owned();
    if source.is_some() {
        panel.command = None;
        app.home_copied = None;
    } else if action == Action::Test { panel.test = None; }
    let (tx, rx) = mpsc::channel();
    panel.pending = Some((action, rx));
    panel.notice = None;
    std::thread::spawn(move || {
        let result = std::env::current_exe().map_err(|_| "Could not locate the bundled CLI.".into()).and_then(|exe| {
            run(&exe.with_file_name(format!("neoboard{}", std::env::consts::EXE_SUFFIX)), &key, &project, source.as_ref(), harness, action)
        });
        let _ = tx.send(result);
        wake();
    });
}

fn run(cli: &Path, key: &str, project: &str, source: Option<&Source>, harness: Harness, action: Action) -> Result<String, String> {
    let mut cmd = crate::background_process::command(cli);
    cmd.args(["wake", action.arg(), "--board-id", key]);
    let preparing = matches!(action, Action::Prepare | Action::Create);
    if preparing {
        source.ok_or("No identity source selected.")?.args(&mut cmd, project)?;
    }
    for var in ["NEOCHAT_URL", "NEOCHAT_TOKEN", "NEOCHAT_DATA_DIR", "NEOCHAT_DAEMON"] { cmd.env_remove(var); }
    let out = cmd.args(["--harness", harness.arg()]).stdin(Stdio::null()).stderr(Stdio::null()).output()
        .map_err(|_| "Could not run the bundled neoboard CLI. Check your installation.")?;
    let text = std::str::from_utf8(&out.stdout).unwrap_or("").trim_end_matches(['\n', '\r']);
    let short = !text.is_empty() && text.len() <= 4096 && !text.chars().any(char::is_control);
    if !out.status.success() {
        // Only control verbs promise user-safe status; prepare errors must never echo credentials.
        return Err(if short && !preparing { format!("Not confirmed: {text}") } else if action == Action::Status { "Status unavailable. Check the bundled CLI and try again.".into() } else { format!("Wake {} was not confirmed. Check status before retrying.", action.arg()) });
    }
    if !short { return Err("Unexpected CLI response. Refresh status before retrying.".into()); }
    if preparing && !pair_command(text, cli) { return Err("No one-time pairing command received. Check your CLI version.".into()); }
    Ok(text.into())
}

fn pair_command(text: &str, cli: &Path) -> bool {
    let Some((program, code)) = text.rsplit_once(" wake pair --code ") else { return false };
    if code.is_empty() || !code.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)) { return false; }
    let quoted = |p: &Path| p.to_str().is_some_and(|s| {
        #[cfg(windows)] let expected = format!("& '{}'", s.replace('\'', "''"));
        #[cfg(not(windows))] let expected = format!("'{}'", s.replace('\'', "'\\''"));
        program == expected
    });
    program == "neoboard" || program == "neoboard.exe" || quoted(cli) || cli.canonicalize().is_ok_and(|p| quoted(&p))
}

pub fn pairing_text(command: &str, h: Harness, manual: bool) -> Option<String> {
    use sha2::{Digest,Sha256};
    let (exe, code)=command.rsplit_once(" wake pair --code ")?;
    // A preview has a different state directory from the installed app. Scope the override to the copied command.
    let prefix=match std::env::var_os("NEOBOARD_WAKE_HOME") {
        None=>String::new(), Some(p)=>{let p=p.to_str().filter(|s|!s.chars().any(char::is_control))?;
            #[cfg(not(windows))] { format!("NEOBOARD_WAKE_HOME='{}' ",p.replace('\'',"'\\''")) }
            #[cfg(windows)] { format!("$env:NEOBOARD_WAKE_HOME='{}'; ",p.replace('\'',"''")) }
        }
    };
    if manual {return Some(format!("{prefix}{command}"));}
    let request=format!("{:x}",Sha256::digest(code.as_bytes()));
    let tool=match h { Harness::Claude=>"this conversation’s local Bash tool", _=>"this conversation’s local shell tool" };
    Some(format!("Pair this exact {} conversation with my pending NeoBoard wake request on this device. Run the command below in {tool}, without supplying a session ID. Never guess or choose the latest chat. If local execution, session identity, or required private runtime configuration is unavailable, stop and explain what is missing. Never request or print credentials, start another chat, send a test, or turn wake on. Report the result so I can check the connection in NeoBoard.\n\n{prefix}{exe} wake pair --request {request}",h.name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_do_not_conflate_pairing_testing_and_enabling() {
        let mut p = Panel::new("board".into(), Harness::Neoism);
        for (op, value) in [(Action::Status, "Off"), (Action::Prepare, "pair here"), (Action::Test, "Requested, not acknowledged")] {
            let (tx, rx) = mpsc::channel();
            p.pending = Some((op, rx));
            assert!(!p.poll());
            tx.send(Ok(value.into())).unwrap();
            assert!(p.poll());
        }
        assert_eq!(p.status.as_deref(), Some("Off"));
        assert_eq!(p.test.as_deref(), Some("Requested, not acknowledged"));
        assert_eq!(p.command.as_deref(), Some("pair here"));
    }

    #[test]
    fn refuse_unexpected_prepare_stdout() {
        let path = Path::new("/unused/neoboard");
        assert!(pair_command("neoboard wake pair --code example", path));
        for s in ["neoboard wake pair --code x --token secret", "TOKEN=secret neoboard wake pair --code c", "not-a-command", "neoboard wake pair --code $(cmd)"] {
            assert!(!pair_command(s, path));
        }
    }

    #[test]
    fn rune_controls_require_consent_and_scope_the_invite_picker() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new();
        app.project = "Team".into();
        app.options.data_dir = Some(dir.path().to_owned());
        app.local_discovery = Some((dir.path().into(), "http://127.0.0.1:23456/".into()));
        app.snapshot.channels = vec![crate::projects::channel("Team", "general")];
        app.status = "live".into();
        app.setup_open = true;
        let mut rt = rune::Runtime::new(app, crate::view::root, (1100., 1080.), rune::Env::new(true, 1.));
        let press = |rt: &mut rune::Runtime<App>, key: &str| {
            for _ in 0..4 { rt.frame(1./60.); }
            let (x, y) = rt.find(key).unwrap();
            rt.pointer_move(x, y); rt.pointer_down(x, y); rt.pointer_up(x, y); rt.frame(1./60.);
        };
        press(&mut rt, "setup-wake");
        rt.update(|a,_| {a.agent_wake.as_mut().unwrap().status=Some("Not paired".into());a.connection_wake=Some(std::sync::Arc::new(|| panic!("must not run a CLI")));});
        assert!(rt.find("wake-on").is_none() && rt.find("wake-test").is_none());
        press(&mut rt, "wake-create");
        assert!(rt.state.agent_wake.as_ref().unwrap().confirming == Some(Action::Create));
        assert!(!rt.state.agent_wake.as_ref().unwrap().busy());
        press(&mut rt, "wake-create-cancel");
        press(&mut rt, "wake-identity-source");
        assert!(rt.find("wake-prepare").is_none());
        #[cfg(feature = "native-picker")]
        {
            press(&mut rt, "wake-choose");
            assert_eq!(rt.file_dialog().unwrap().0, rune::FileDialogKind::Open);
            let path = dir.path().join("guest.neochat");
            rt.resolve_file_dialog(Some(path.clone())).unwrap(); rt.frame(1./60.);
            assert_eq!(Path::new(&rt.state.agent_wake.as_ref().unwrap().invite.text), path);
        }
        rt.update(|a,_| a.agent_wake.as_mut().unwrap().status=Some("Off | @reviewer | test-session".into()));
        press(&mut rt, "wake-on");
        let p = rt.state.agent_wake.as_ref().unwrap();
        assert!(p.confirming == Some(Action::On) && !p.busy());
        let key = p.key.clone();
        rt.update(|a, _| {a.connection_wake=None; select(a, Harness::Codex);});
        assert!(rt.state.agent_wake.as_ref().unwrap().confirming.is_none());
        assert!(!selected(&rt.state, &key, Harness::Neoism));
        rt.update(|a, _| a.options.invite = Some(dir.path().join("other.neochat")));
        assert!(!selected(&rt.state, &key, Harness::Codex));
        rt.frame(1./60.);
        assert!(rt.find("wake-on").is_none());
        rt.update(|a, _| {open(a);a.agent_wake.as_mut().unwrap().status=Some("Not paired".into());});
        press(&mut rt, "wake-identity-source");
        assert!(rt.state.agent_wake.as_ref().unwrap().dedicated);
        #[cfg(feature="native-picker")] assert!(rt.find("wake-choose").is_some());
        #[cfg(not(feature="native-picker"))] assert!(rt.find("wake-invite").is_some());
        assert!(rt.find("wake-create").is_none());
        assert!(source(&rt.state, Action::Create).is_err());
        assert!(source(&rt.state, Action::Prepare).is_err()); // blank override never reuses membership
    }

    #[test]
    fn provision_only_from_the_directory_verified_by_the_live_local_worker() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = App::new();
        a.project = "Team".into(); a.snapshot.channels = vec![crate::projects::channel("Team", "general")]; a.status = "live".into();
        assert!(source(&a, Action::Create).is_err());
        let default = std::path::absolute(neochat::client::local_data_dir(None).unwrap()).unwrap();
        a.local_discovery = Some((default.clone(), "http://127.0.0.1:23456/".into()));
        assert!(matches!(source(&a, Action::Create), Ok(Some(Source::Local { dir, .. })) if dir == default));
        a.options.data_dir = Some(dir.path().into());
        assert!(source(&a, Action::Create).is_err()); // not the worker's directory
        a.local_discovery.as_mut().unwrap().0 = dir.path().into();
        assert!(matches!(source(&a, Action::Create), Ok(Some(Source::Local { dir: d, .. })) if d == dir.path()));
        a.options.endpoint = Some("https://same-name.invalid".into());
        assert!(source(&a, Action::Create).is_err());
        a.options.endpoint = None; a.options.invite = Some(dir.path().join("missing"));
        assert!(source(&a, Action::Create).is_err());
        a.options.invite = None; a.project = "Other".into();
        assert!(source(&a, Action::Create).is_err());
    }

    #[test]
    fn dedicated_invite_bound_to_current_membership_and_saved_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let member = dir.path().join("member"); let agent = dir.path().join("agent");
        let write = |p: &Path, project: &str, endpoint: &str, expiry: u64| std::fs::write(p,
            serde_json::to_vec(&serde_json::json!({"version":1,"project":project,"endpoint":endpoint,"expires_at":expiry,"token":"fixture"})).unwrap()).unwrap();
        let mut a = App::new(); a.project = "Team".into(); a.options.invite = Some(member.clone());
        let origin = "https://board.invalid";
        a.saved_boards.remember(a.project.clone(), a.options.invite.clone(), Some(origin.into()));
        open(&mut a);
        assert!(matches!(source(&a, Action::Prepare), Ok(Some(Source::Invite { path, .. })) if path == member));
        let p = a.agent_wake.as_mut().unwrap(); p.dedicated = true; p.invite = edit(agent.to_str().unwrap());
        let s = source(&a, Action::Prepare).unwrap().unwrap();
        assert!(matches!(&s, Source::Invite {path, ..} if *path == agent));
        let check = || s.args(&mut std::process::Command::new("never-started"), "Team");
        write(&member, "Team", origin, u64::MAX); write(&agent, "Team", "https://BOARD.invalid:443/", u64::MAX);
        assert!(check().is_ok());
        for (project, url, expiry) in [("Team", "https://other.invalid", u64::MAX), ("Other", origin, u64::MAX), ("Team", origin, 1), ("Team", "https://board.invalid:444", u64::MAX)] {
            write(&agent, project, url, expiry); assert!(check().is_err());
        }
        write(&agent, "Team", "https://other.invalid", u64::MAX); write(&member, "Team", "https://other.invalid", u64::MAX);
        assert!(check().is_err()); // changed membership still cannot override stored endpoint
        write(&agent, "Team", origin, u64::MAX); std::fs::remove_file(&member).unwrap(); assert!(check().is_err());
        select(&mut a, Harness::Claude);
        assert!(a.agent_wake.as_ref().unwrap().invite.text.is_empty());
    }

    #[test]
    fn setup_selection_not_active_membership_drives_wake_and_drops_old_consent() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new();
        app.project = "Team".into();
        let project = app.project.clone();
        let endpoint = "https://fixture.invalid/";
        let paths = ["personal", "worker"].map(|name| {
            let grant = neochat::invite_file::InviteFile { version: 1, project: Some(project.clone()), endpoint: endpoint.into(), token: name.into(), expires_at: u64::MAX };
            let path = grant.store(dir.path()).unwrap();
            app.saved_boards.remember(project.clone(), Some(path.clone()), Some(endpoint.into()));
            path
        });
        app.options.invite = Some(paths[0].clone());
        crate::setup_guide::open(&mut app);
        open(&mut app);
        let old = Guide::new(&app).key;
        app.agent_wake.as_mut().unwrap().confirming = Some(Action::Prepare);
        let target = crate::setup_guide::choices(&app).into_iter().find(|t| t.profile.invite.as_ref() == Some(&paths[1])).unwrap();
        crate::setup_guide::choose(&mut app,target);
        assert!(app.agent_wake.is_none());
        open(&mut app);
        assert!(!selected(&app, &old, Harness::Neoism));
        assert!(source(&app,Action::Create).is_err());
        let Some(Source::Invite { path, membership, .. }) = source(&app,Action::Prepare).unwrap() else { panic!("expected selected grant") };
        assert_eq!(path, paths[1]);
        assert_eq!(membership.as_ref(), Some(&paths[1]));
        assert_eq!(app.options.invite.as_ref(), Some(&paths[0]));
        std::fs::remove_file(path).unwrap();
        assert!(source(&app, Action::On).is_err()); // never fall back to the personal grant
    }

    #[cfg(unix)]
    #[test]
    fn disposable_cli_receives_only_contract_args() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("neoboard");
        fs::write(&cli, r#"#!/bin/sh
set -eu
test "$1" = wake && test "$3" = --board-id && test "$4" = board || exit 1
test -z "${NEOCHAT_TOKEN-}${NEOCHAT_URL-}${NEOCHAT_DATA_DIR-}" || exit 1
case "$2" in
prepare) test "$5" = --project && test "$6" = Team || exit 1
if test "$7" = --local-data-dir; then
test "$#" = 13 && test "$8" = "${0%/*}" && test "$9" = --local-endpoint && test "${10}" = http://127.0.0.1:12432/ && test "${11}" = --create-agent && test "${12}" = --harness && test "${13}" = claude || exit 1
else
test "$#" = 10 && test "$7" = --invite && test "$8" = "${0%/*}/file with spaces" && test "$9" = --harness && test "${10}" = claude || exit 1
fi
printf 'neoboard wake pair --code CODE\n' ;;
test|on|off|status) test "$#" = 6 && test "$5" = --harness && test "$6" = claude || exit 1; printf '%s response\n' "$2" ;;
*) exit 1;;
esac
"#).unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        let invite = dir.path().join("file with spaces");
        let grant = neochat::invite_file::InviteFile { version: 1, project: Some("Team".into()), endpoint: "http://127.0.0.1:12432".into(), token: "test-only".into(), expires_at: u64::MAX };
        fs::write(&invite, serde_json::to_vec(&grant).unwrap()).unwrap();
        let invoke = |action, s| run(&cli, "board", "Team", s, Harness::Claude, action);
        let s = Source::Invite { path: invite, membership: None, endpoint: None };
        let local = Source::Local { dir: dir.path().into(), endpoint: "http://127.0.0.1:12432/".into() };
        assert_eq!(invoke(Action::Prepare, Some(&s)).unwrap(), "neoboard wake pair --code CODE");
        assert_eq!(invoke(Action::Create, Some(&local)).unwrap(), "neoboard wake pair --code CODE");
        for op in [Action::Test, Action::On, Action::Off, Action::Status] { assert_eq!(invoke(op, None).unwrap(), format!("{} response", op.arg())); }
    }
}