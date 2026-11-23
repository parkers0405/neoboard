use serde_json::json;
use crate::{connection::Options, saved_boards::BoardProfile};
use sha2::{Digest, Sha256};
use std::{env, path::{Path, PathBuf}, sync::{LazyLock, mpsc}};

#[path = "setup_catalog.rs"]
mod catalog;

/// Drawer-only reference. Never change the connection worker, preferences or active project.
#[derive(Clone, PartialEq)]
pub struct Target { pub profile: BoardProfile, pub local: Option<PathBuf> }

impl Target {
    fn from_row(app: &crate::App, project: &str, profile: Option<&BoardProfile>) -> Self {
        let profile = profile.cloned().unwrap_or_else(|| BoardProfile { project: project.into(), invite: app.options.invite.clone(), endpoint: app.options.endpoint.clone() });
        let local = if profile.invite.is_none() && profile.endpoint.is_none() {
            neochat::client::local_data_dir(app.options.data_dir.as_deref()).ok().and_then(|p| std::path::absolute(p).ok())
        } else { None };
        Self { profile, local }
    }
    pub fn label(&self, app: &crate::App) -> String {
        let location = if self.local.is_some() { "Local".into() } else if let Some(i) = app.saved_boards.entries().iter().position(|p| p == &self.profile) { self.profile.location(i) } else { "Current server".into() };
        format!("{} · {location}", self.profile.project)
    }
    fn options(&self) -> Options {
        Options { invite: self.profile.invite.clone(), endpoint: self.profile.endpoint.clone(), data_dir: self.local.clone(), ..Default::default() }
    }
}

pub fn choices(app: &crate::App) -> Vec<Target> {
    app.board_rows().into_iter().map(|(id, saved, _)| Target::from_row(app, id, saved.map(|(_, p)| p))).collect()
}

fn active(app: &crate::App) -> Target {
    let profile = app.saved_boards.entries().iter().find(|p| p.project == app.project && p.on_server(&app.options));
    Target::from_row(app, &app.project, profile)
}

pub fn target(app: &crate::App) -> Option<Target> {
    if app.onboarding.switching { return None; }
    let target = match app.setup_target.as_ref().filter(|_| app.setup_open) {
        Some(pinned) => choices(app).into_iter().find(|t| t == pinned)?,
        None => active(app), // onboarding's guide uses its newly joined connection
    };
    (neochat::valid_project(&target.profile.project) && (target.local.is_some() || target.profile.invite.as_deref().is_some_and(Path::is_absolute))
        && !(app.status == "access denied" && target.profile.on_server(&app.options))).then_some(target)
}

pub fn choose(app: &mut crate::App, target: Target) {
    app.setup_target = Some(target);
    app.setup_board_menu = false;
    app.home_copied = None;
    app.agent_wake = None;
}

pub fn copy_allowed(app: &crate::App, shown: Option<&Target>) -> bool {
    let Some(current) = target(app) else { return false };
    Some(&current) == shown && current.profile.invite.as_ref().is_none_or(|path|
        neochat::invite_file::InviteFile::read(path).is_ok_and(|grant| check_scope(&grant, &current.profile.project, current.profile.endpoint.as_deref()).is_ok()))
}

pub fn open(app: &mut crate::App) {
    choose(app, active(app));
    app.setup_choices = false;
    app.setup_open = true;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Agent {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
    #[default]
    Neoism,
}

impl Agent {
    pub const ALL: [Self; 4] = [Self::ClaudeCode, Self::ClaudeDesktop, Self::Codex, Self::Neoism];

    pub fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::ClaudeDesktop => "Claude Desktop",
            Self::Codex => "Codex",
            Self::Neoism => "Neoism",
        }
    }
}

pub struct Guide {
    agent: Agent,
    pub key: String,
    pub install: Option<Install>,
    pub intro: String,
    pub instruction: String,
    pub snippet: String,
    pub question: String,
}

pub struct Install {
    project: String,
    key: String,
    adapter: String,
    path: PathBuf,
    joined: bool,
    host_number: Option<usize>,
    endpoint: Option<String>,
}

impl Install {
    fn run(self, dir: &Path, approved: Option<&catalog::Replacement>) -> Result<catalog::Outcome, &'static str> {
        if !Path::new(&self.adapter).is_file() { return Err("No changes: install or build the neochat-mcp adapter first."); }
        // The ID suffix also distinguishes grants to same-name boards on the same host.
        let suffix = self.key.rsplit('-').next().unwrap_or(&self.key);
        let board = if self.joined {
            let invite = neochat::invite_file::InviteFile::read(&self.path).map_err(|_| "No changes: the saved invite is unavailable, invalid or expired.")?;
            check_scope(&invite, &self.project, self.endpoint.as_deref())?;
            // Use the picker's public alias, never invite-derived text (including the endpoint).
            let alias = self.host_number.map(|n| format!("Host {n}")).unwrap_or_else(|| "saved invite".into());
            let label = format!("{} ({alias}; {suffix})", self.project);
            catalog::Board::Hosted { label, invite: self.path }
        } else {
            let label = format!("{} (local; {suffix})", self.project);
            catalog::Board::Local { label, data_dir: self.path }
        };
        catalog::install(dir, &self.key, &self.adapter, &board, approved)
    }
}

fn check_scope(invite: &neochat::invite_file::InviteFile, project: &str, endpoint: Option<&str>) -> Result<(), &'static str> {
    if invite.project.as_deref() != Some(project) || endpoint.is_some_and(|e| reqwest::Url::parse(e).ok() != reqwest::Url::parse(&invite.endpoint).ok()) {
        return Err("No changes: the saved membership does not match the selected board and endpoint.");
    }
    Ok(())
}

#[derive(Default)]
pub struct Session {
    key: String,
    inbox: Option<mpsc::Receiver<Result<catalog::Outcome, &'static str>>>,
    replacement: Option<catalog::Replacement>,
    message: &'static str,
}

impl Session {
    pub fn status(&self, key: &str) -> &str { if self.key == key { self.message } else { "" } }
    pub fn replacement(&self, key: &str) -> Option<&catalog::Replacement> { self.replacement.as_ref().filter(|_| self.key == key) }
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.inbox else { return false };
        self.message = match rx.try_recv() {
            Ok(Ok(catalog::Outcome::Done(msg))) | Ok(Err(msg)) => msg,
            Ok(Ok(catalog::Outcome::Confirm(ticket))) => {
                self.replacement = Some(ticket);
                "Not added. Replacing the existing global neoboard binding will disconnect its prior configuration."
            }
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(_) => "Save not confirmed. Check Neoism before retrying.",
        };
        self.inbox = None;
        true
    }
}

fn selected(app: &crate::App, shown_key: &str) -> Option<Install> {
    if !app.setup_open || app.setup_session.inbox.is_some() { return None; }
    Guide::new(app).install.filter(|entry| entry.key == shown_key)
}

pub fn add_board(app: &mut crate::App, key: &str) {
    authorize(app, key, None);
}

pub fn confirm(app: &mut crate::App, key: &str, ticket: &catalog::Replacement) {
    if app.setup_session.replacement(key) == Some(ticket) { authorize(app, key, Some(ticket.clone())); }
}

fn authorize(app: &mut crate::App, shown_key: &str, approved: Option<catalog::Replacement>) {
    let Some(entry) = selected(app, shown_key) else { return };
    let Some(wake) = app.connection_wake.clone() else { return };
    let (send, recv) = mpsc::channel();
    app.setup_session = Session { key: entry.key.clone(), inbox: Some(recv), message: "Registering…", replacement: None };
    std::thread::spawn(move || {
        let result = crate::profile::config_dir().ok_or("No changes: Neoism’s config directory is unavailable.").and_then(|dir| entry.run(&dir, approved.as_ref()));
        let _ = send.send(result);
        wake();
    });
}

fn server_key(project: &str, kind: &str, identity: &str) -> String {
    let mut hash = Sha256::new();
    for part in [project, kind, identity] { hash.update(part.as_bytes()); hash.update([0]); }
    let slug: String = project.chars().take(24).map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    format!("neoboard-{slug}-{}", &format!("{:x}", hash.finalize())[..12])
}

static ADAPTER: LazyLock<String> = LazyLock::new(|| {
    let name = format!("neochat-mcp{}", env::consts::EXE_SUFFIX);
    let Some(exe) = env::current_exe().ok() else { return name };
    let sibling = exe.with_file_name(&name);
    let desktop = Path::new(env!("CARGO_MANIFEST_DIR"));
    if cfg!(debug_assertions) && exe.starts_with(desktop.join("target/debug")) && !sibling.is_file() {
        return desktop.with_file_name("target").join("debug").join(name).display().to_string();
    }
    sibling.display().to_string()
});

impl Guide {
    fn unavailable(agent: Agent) -> Self {
        Self { agent, key: String::new(), install: None, intro: "Choose a local board or a saved membership.".into(), instruction: "This connection is unavailable for setup.".into(), snippet: String::new(), question: "Do not use a different board or credential.".into() }
    }

    pub fn new(app: &crate::App) -> Self {
        let Some(target) = target(app) else { return Self::unavailable(app.setup_agent) };
        let mut guide = Self::build(app.setup_agent, &ADAPTER, &target.options(), &target.profile.project);
        if app.fixture || app.onboarding.switching { guide.install = None; }
        if let Some(entry) = guide.install.as_mut().filter(|e| e.joined) {
            // Match the host alias shown in the board picker without copying its endpoint.
            entry.host_number = app.saved_boards.entries().iter().position(|p| p == &target.profile).map(|i| i + 1);
        }
        guide
    }

    fn build(agent: Agent, adapter: &str, opts: &crate::connection::Options, project: &str) -> Self {
        let joined = opts.invite.is_some();
        let path = opts.invite.as_ref().or_else(|| opts.data_dir.as_ref().filter(|_| opts.endpoint.is_none()));
        let literal = |s: &str| !s.contains(['{', '}']) && !s.chars().any(char::is_control);
        let Some(location) = path.filter(|p| p.is_absolute()).and_then(|p| p.to_str()).filter(|s| literal(s)) else { return Self::unavailable(agent) };
        let args = [if joined { "--invite" } else { "--data-dir" }, location];
        let key = server_key(project, if joined { "invite" } else { "local" }, location);
        let installable = agent == Agent::Neoism && literal(adapter) && Path::new(adapter).is_absolute() && neochat::valid_project(project);
        let mut install = None;
        let intro = if joined { "Adding this entry lets the agent use this saved membership, with its permissions and expiry. If you joined as yourself, this is your personal membership. Add it only if you intend to grant that access." } else { "Adding this entry grants the agent local MCP access to all boards visible to the local credential, not just the selected board." }.to_owned();
        let (location, snippet) = match agent {
            Agent::Codex => {
                // JSON string escapes also work in TOML basic strings; DEL must be escaped in TOML.
                let command = json!(adapter).to_string().replace('\u{7f}', "\\u007f");
                let args = json!(args).to_string().replace('\u{7f}', "\\u007f");
                ("~/.codex/config.toml", format!("[mcp_servers.{key}]\ncommand = {command}\nargs = {args}"))
            }
            Agent::Neoism => {
                if installable {
                    install = Some(Install { key: key.clone(), project: project.to_owned(), adapter: adapter.into(), path: PathBuf::from(location), joined, host_number: None, endpoint: opts.endpoint.clone() });
                }
                let registry = crate::profile::config_dir().filter(|d| d.is_absolute()).map(|d| d.join(catalog::BOARDS)).unwrap_or_else(|| PathBuf::from("<absolute boards file>"));
                let server = catalog::server(adapter, &registry.to_string_lossy());
                let question = format!("Using neoboard, list the channels visible for board {project} (board_id: {key}) without making changes.");
                return Self { agent, key, install, intro, instruction: "Add this board, then reconnect neoboard in Neoism. One global plugin serves all boards you add on this device.".into(), snippet: format!("{:#}", json!({"neoboard":server})), question };
            }
            Agent::ClaudeCode | Agent::ClaudeDesktop => {
                let server = json!({"command":adapter, "args":args});
                let location = if agent == Agent::ClaudeCode {
                    "the project's .mcp.json"
                } else {
                    "claude_desktop_config.json (Settings → Developer → Edit Config)"
                };
                (location, format!("{:#}", json!({"mcpServers":{(&key):server}})))
            }
        };
        let auth = if joined { "Keep the membership file private and on this device." } else { "No workspace selection is needed." };
        Self {
            agent, intro, key: key.clone(), install,
            instruction: format!("Add {key} to {location} without replacing any other entries. {auth} Restart {}.", agent.name()),
            snippet,
            question: format!("Using {key}, list the channels visible for board {project} without making changes."),
        }
    }

    pub fn copy_text(&self) -> String {
        let runtime = match self.agent {
            Agent::ClaudeDesktop => "Claude Desktop supports MCP but not automatic wake. Stop after the read-only check.",
            Agent::Neoism => "Pair the exact Neoism session on its loopback runtime. Supply any required runtime credential privately on the agent machine.",
            Agent::ClaudeCode => "Pair a dedicated Claude Code session. If the terminal cannot identify it, use its exact session ID, never the most recent chat.",
            Agent::Codex => "Pair a dedicated Codex thread using its exact thread ID. If the OS blocks the CLI, stop; do not bypass the warning.",
        };
        let wake = if self.agent == Agent::ClaudeDesktop { runtime.to_string() } else { format!("If I want wake, guide me through Home → Setup → Set up wake. {runtime} Use the agent's own board identity. I choose Copy for agent and paste the pairing prompt into the exact conversation I want to wake, or choose Manual for a terminal command. Then I select Check connection. Do not ask me to paste private codes or tokens here. Pairing leaves wake off. I can choose Turn on to allow mentions and requested task updates to start turns, or Pause to stop future wakes. Do not enable wake or send a test on my behalf.") };
        if self.key.is_empty() { return self.instruction.clone(); }
        format!("1. Connect MCP for {}\n{} {}\nAuthorize this connection only with my explicit approval. Do not switch the current session’s MCP, substitute another board, or use my personal or host access by default. If this exact connection is unavailable, stop.\n\n{}\n\n2. Verify access\n{}\n\n3. Automatic wake (optional)\n{}", self.agent.name(), self.intro, self.instruction, self.snippet, self.question, wake)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::Options;

    #[test]
    fn endpoint_without_a_membership_has_no_actionable_guide() {
        let opts = Options { token: "PRIVATE_SENTINEL".into(), endpoint: Some("https://PRIVATE_SENTINEL.invalid".into()), ..Options::default() };
        assert_eq!(Agent::default(), Agent::Neoism);
        for agent in Agent::ALL {
            let guide = Guide::build(agent, "C:\\Neo Board\\adapter.exe", &opts, "Team");
            let text = guide.copy_text();
            assert!(guide.key.is_empty() && guide.snippet.is_empty() && guide.install.is_none());
            assert!(!text.contains("PRIVATE_SENTINEL") && !text.contains("--data-dir"));
            assert!(text.contains("unavailable"));
        }
    }

    #[test]
    fn joined_board_setup_uses_selected_invite_instead_of_environment() {
        let path = env::temp_dir().join("Board A.neochat");
        let opts = Options { invite: Some(path), token: "PRIVATE_SENTINEL".into(), ..Options::default() };
        for agent in Agent::ALL {
            let guide = Guide::build(agent, "adapter", &opts, "Team");
            if agent == Agent::Neoism {
                assert!(guide.snippet.contains("--boards-file") && !guide.snippet.contains("Board A.neochat"));
            } else { assert!(guide.snippet.contains("--invite") && guide.snippet.contains("Board A.neochat")); }
            assert!(!guide.copy_text().contains("PRIVATE_SENTINEL") && !guide.snippet.contains("NEOCHAT_"));
        }
    }

    #[test]
    fn local_board_uses_a_separate_quoted_directory_argument() {
        let dir = env::temp_dir().join("Board A");
        let opts = Options { data_dir: Some(dir.clone()), ..Options::default() };
        let guide = Guide::build(Agent::ClaudeDesktop, "adapter", &opts, "Team");
        let config: serde_json::Value = serde_json::from_str(&guide.snippet).unwrap();
        assert_eq!(config["mcpServers"][&guide.key]["args"], json!(["--data-dir", dir]));
        assert!(!guide.snippet.contains("TOKEN"));
    }

    #[test]
    fn same_name_boards_share_one_plugin_with_distinct_idempotent_registrations() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let dir = base.join("catalog");
        let adapter = base.join("neochat-mcp");
        std::fs::write(&adapter, "test placeholder; never executed").unwrap();
        let build = |opts: &Options, board| Guide::build(Agent::Neoism, adapter.to_str().unwrap(), opts, board);
        let local = Options { data_dir: Some(base.join("local")), ..Default::default() };
        let invite = neochat::invite_file::InviteFile { version: 1, project: Some("Team".into()), endpoint: "https://unused.invalid/sensitive-path".into(), token: "test-only-marker".into(), expires_at: u64::MAX };
        let hosted = || Options { invite: Some(invite.store(&base).unwrap()), ..Default::default() };
        let opts_a = hosted(); let opts_b = hosted();
        let mut keys = std::collections::BTreeSet::new();
        let mut labels = std::collections::BTreeSet::new();
        for (i, (opts, board)) in [(&local, "Team"), (&local, "Other"), (&opts_a, "Team"), (&opts_b, "Team")].into_iter().enumerate() {
            let guide = build(opts, board);
            assert!(keys.insert(guide.key.clone()));
            let mut entry = guide.install.unwrap();
            if opts.invite.is_some() { entry.host_number = Some(i + 1); }
            entry.run(&dir, None).unwrap();
            let before = std::fs::read(dir.join("mcp.json")).unwrap();
            let profiles = std::fs::read_to_string(dir.join(catalog::BOARDS)).unwrap();
            assert!(!profiles.contains("test-only-marker") && !profiles.contains("sensitive-path") && !profiles.contains("unused.invalid"));
            let map: serde_json::Value = serde_json::from_str(&profiles).unwrap();
            let entry = &map[&guide.key];
            let label = entry["label"].as_str().unwrap();
            assert!(label.contains(board) && labels.insert(label.to_owned()));
            if let Some(path) = &opts.invite {
                assert!(label.contains(&format!("Host {}", i + 1)));
                assert_eq!(entry, &json!({"label":label,"invite":path}));
            } else { assert_eq!(entry, &json!({"label":label,"data_dir":opts.data_dir})); }
            assert_eq!(map.as_object().unwrap().len(), i+1);
            assert!(matches!(build(opts, board).install.unwrap().run(&dir, None), Ok(catalog::Outcome::Done(msg)) if msg.starts_with("Already")));
            assert!(std::fs::read(dir.join("mcp.json")).unwrap() == before);
            assert_eq!(std::fs::read_to_string(dir.join(catalog::BOARDS)).unwrap(), profiles);
        }
        let content = std::fs::read_to_string(dir.join("mcp.json")).unwrap();
        assert!(!content.contains("test-only-marker") && !content.contains("unused.invalid"));
        let catalog: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(catalog.as_object().unwrap().len(), 1);
        assert_eq!(catalog["neoboard"]["command"], json!([adapter,"--boards-file",dir.join(catalog::BOARDS)]));
        assert!(build(&opts_a, "Other").install.unwrap().run(&dir, None).is_err());
    }

    #[test]
    fn rune_setup_offers_authorization_only_for_an_installable_selection() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = crate::App::new();
        app.project = "Team".into();
        app.options.data_dir = Some(tmp.path().join("board"));
        app.setup_open = true;
        // No connection worker, config resolver, or install callback is run by rendering.
        let mut rt = rune::Runtime::new(app, crate::view::root, (900., 720.), rune::Env::new(true, 1.));
        let offered = |rt: &mut rune::Runtime<crate::App>| {
            rt.frame(1. / 60.);
            assert_eq!(rt.find("copy-mcp-config").is_some(), rt.state.setup_agent != Agent::Neoism || rt.state.options.endpoint.is_none() || rt.state.options.invite.is_some());
            rt.find("setup-add-neoism").is_some()
        };
        assert!(offered(&mut rt));
        rt.update(|a, _| a.setup_agent = Agent::Codex);
        assert!(!offered(&mut rt));
        rt.update(|a, _| { a.setup_agent = Agent::Neoism; a.options.endpoint = Some("https://unused.invalid".into()); });
        assert!(!offered(&mut rt));
        rt.update(|a, _| a.options.invite = Some(tmp.path().join("saved.neochat")));
        assert!(offered(&mut rt));
        rt.update(|a, _| a.saved_boards.remember("Team".into(), a.options.invite.clone(), None));
        assert_eq!(Guide::new(&rt.state).install.unwrap().host_number, Some(1));
        rt.update(|a, _| a.fixture = true);
        assert!(!offered(&mut rt));
        assert!(rt.state.setup_session.inbox.is_none());
    }

    #[test]
    #[ignore = "manual Rune PNG check; set NEOBOARD_SETUP_SNAPSHOT to the output path"]
    fn render_authorization_drawer() {
        let path = PathBuf::from(env::var_os("NEOBOARD_SETUP_SNAPSHOT").expect("temporary PNG destination"));
        let spec = rune::AppSpec {
            fonts: &[include_bytes!("../assets/fonts/GeistMono-Regular.otf")], families: ("Geist Mono", "Geist Mono"),
            ..rune::AppSpec::new("Setup", (1252., 674.), || {
                let mut app = crate::App::new();
                app.project = "Team".into();
                app.options.data_dir = Some(env::temp_dir().join("test-board"));
                app.snapshot.channels = vec!["Team/general".into(), "Archive/general".into()];
                for host in ["east", "west"] { app.saved_boards.remember("Team".into(), Some(env::temp_dir().join(format!("fixture-{host}.neochat"))), Some(format!("https://{host}.invalid/"))); }
                open(&mut app);
                app.setup_board_menu = true;
                app
            }, crate::view::root)
        };
        let mut args = rune::tooling::Args::default();
        args.snap = Some(path.clone()); args.fps = 60.; args.at = Some(1.); args.scale = Some(1.);
        rune::tooling::headless(rune::App::new(spec), args);
        assert!(path.is_file());
    }

    #[test]
    fn rendered_add_board_starts_registration_without_a_folder_dialog() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = crate::App::new();
        app.project = "Team".into();
        // A missing invite must fail before any catalog I/O. Waking signals the worker ran.
        app.options.invite = Some(tmp.path().join("missing.neochat"));
        let (wake, completed) = mpsc::channel();
        app.connection_wake = Some(std::sync::Arc::new(move || { let _ = wake.send(()); }));
        app.setup_open = true;
        let mut rt = rune::Runtime::new(app, crate::view::root, (1100., 960.), rune::Env::new(true, 1.));
        let key = Guide::new(&rt.state).key;
        rt.frame(1./60.);
        let (x,y) = rt.find("setup-add-neoism").unwrap();
        rt.pointer_move(x,y); rt.pointer_down(x,y); rt.pointer_up(x,y); rt.frame(1./60.);
        assert!(rt.file_dialog().is_none());
        completed.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        rt.update(|a,_| { a.setup_session.poll(); });
        assert!(rt.state.setup_session.status(&key).starts_with("No changes:"));
        rt.update(|a,_| { a.options.invite = Some(tmp.path().join("different.neochat")); add_board(a, &key); });
        assert!(rt.state.setup_session.inbox.is_none()); // stale action ignored
    }

    #[test]
    fn drawer_selects_exact_saved_grant_or_discovered_local_without_switching() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = crate::App::new();
        app.project = "Desktop".into();
        app.options.data_dir = Some(dir.path().into());
        app.snapshot.channels = vec!["Desktop/general".into(), "Team/general".into()];
        let grant = neochat::invite_file::InviteFile { version: 1, project: Some("Team".into()), endpoint: "https://fixture.invalid/".into(), token: "password-exchange-fixture".into(), expires_at: u64::MAX };
        let paths = [grant.store(dir.path()).unwrap(), grant.store(dir.path()).unwrap()];
        for path in &paths { app.saved_boards.remember("Team".into(), Some(path.clone()), Some(grant.endpoint.clone())); }
        let (tx, requests) = mpsc::sync_channel(4);
        app.commands = Some(tx);
        open(&mut app);
        let mut rt = rune::Runtime::new(app, crate::view::root, (1100., 960.), rune::Env::new(true, 1.));
        let click = |rt: &mut rune::Runtime<crate::App>, id: &str| {
            for _ in 0..4 { rt.frame(1./60.); }
            let (x,y) = rt.find(id).unwrap_or_else(|| panic!("missing {id}"));
            rt.pointer_move(x,y); rt.pointer_down(x,y); rt.pointer_up(x,y); rt.frame(1./60.);
        };
        let mut keys = std::collections::BTreeSet::new();
        for (index, path) in [(0, Some(&paths[0])), (1, Some(&paths[1])), (3, None)] {
            click(&mut rt, "setup-board");
            click(&mut rt, &format!("setup-board-{index}"));
            assert!(!rt.state.setup_board_menu && rt.file_dialog().is_none());
            let t = target(&rt.state).unwrap();
            assert_eq!(t.profile.project, "Team");
            assert_eq!(t.profile.invite.as_ref(), path);
            assert!(copy_allowed(&rt.state, Some(&t)));
            let guide = Guide::new(&rt.state);
            assert!(guide.question.contains(&guide.key) && guide.question.contains("without making changes"));
            assert!(keys.insert(guide.key.clone()));
            let entry = selected(&rt.state, &guide.key).unwrap();
            assert_eq!(entry.path.as_path(), path.map_or(dir.path(), |p| p.as_path()));
            assert_eq!(entry.joined, path.is_some());
            assert!(!guide.copy_text().contains(&grant.token));
            assert_eq!(rt.state.project, "Desktop");
            assert!(rt.state.options.invite.is_none() && requests.try_recv().is_err());
        }
        let shown = target(&rt.state).unwrap();
        rt.update(|a,_| { a.options.endpoint = Some("http://127.0.0.1:12345".into()); });
        assert!(!copy_allowed(&rt.state, Some(&shown)) && Guide::new(&rt.state).install.is_none());
        assert!(keys.iter().all(|k| selected(&rt.state,k).is_none()));
        rt.frame(1./60.);
        assert!(rt.find("setup-wake").is_none() && rt.find("copy-setup").is_none());
    }
}