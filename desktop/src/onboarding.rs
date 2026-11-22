use super::*;

#[derive(Default)]
pub struct Share {
    pub request: u64,
    pub operation: Option<connection::HostOp>,
    pub confirm_stop: bool,
    pub pulse: f32,
    pub board: String,
    pub busy: bool,
    pub result: Option<Result<connection::HostState, String>>,
    pub copied: bool,
    pub password: crate::host_password::Panel,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub enum Step { #[default] Name, Board, Connect }

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum JoinMethod { #[default] Invite, Password }

// The fragment names the board; it is never part of the endpoint sent to the client.
fn password_target(address: &str, slug: &str) -> Result<(String, String), &'static str> {
    let address = address.trim();
    let slug = slug.trim();
    let (base, fragment) = match address.split_once('#') {
        None => (address, None),
        Some((base, fragment)) => (base, Some(fragment.strip_prefix("neoboard=")
            .filter(|id| neochat::valid_project(id)).ok_or("Use a server address or a #neoboard=BOARD link. For a private invite, choose Private invite.")?)),
    };
    let endpoint = neochat::invite_file::https_endpoint(base)?;
    if !slug.is_empty() && fragment.is_some_and(|id| id != slug) { return Err("The board slug must match the board in the server link."); }
    let project = fragment.unwrap_or(if slug.is_empty() { "general" } else { slug });
    if !neochat::valid_project(project) { return Err("Enter a valid board slug (letters, digits, dashes, underscores or dots)."); }
    Ok((endpoint, project.to_owned()))
}

pub struct Onboarding {
    pub step: Step,
    pub name: TextEdit,
    pub board: TextEdit,
    pub invite: TextEdit,
    pub method: JoinMethod,
    pub server: TextEdit,
    pub slug: TextEdit,
    pub password: TextEdit,
    pub joining: bool,
    pub adding: bool,
    pub busy: bool,
    pub switching: bool,
    pub error: Option<String>,
}

impl Default for Onboarding {
    fn default() -> Self {
        Self { step: Step::Name, name: edit(""), board: edit(""), invite: edit("").secure(), joining: false,
            method: Default::default(), server: edit(""), slug: edit(""), password: edit("").secure(),
            adding: false, busy: false, switching: false, error: None }
    }
}

impl Onboarding {
    pub fn clear_join_fields(&mut self) {
        self.invite.clear(); self.password.clear(); self.server.clear(); self.slug.clear();
        self.error = None;
    }

    pub fn cancel_form(&mut self) {
        if self.busy || self.step != Step::Board { return; }
        if self.adding { *self = Self::default(); }
        else { *self = Self { name: std::mem::replace(&mut self.name, edit("")), ..Self::default() }; }
    }
}

impl App {
    pub fn open_share(&mut self) {
        self.open_board_share(&self.project.clone());
    }

    pub fn can_share_board(&self, id: &str) -> bool {
        !id.is_empty() && !self.remote() && self.projects().contains(id)
    }

    pub fn open_board_share(&mut self, id: &str) {
        if !self.can_share_board(id) && !(self.remote() && self.project == id) { return; }
        self.share = Share { board: id.into(), request: self.share.request + 1, ..Default::default() };
        self.project_menu = false;
        self.dialog = Some(Dialog::Share);
        self.host_request(connection::HostOp::Inspect);
    }

    pub fn host_request(&mut self, op: connection::HostOp) {
        use connection::HostOp::*;
        if self.share.busy || !self.can_share_board(&self.share.board) { return; }
        if self.status != "live" { return; }
        let state = self.share.result.as_ref().and_then(|r| r.as_ref().ok());
        if op == Stop && !(self.share.confirm_stop && state.is_some_and(|s| s.route != neochat::sharing::RouteState::Stopped)) { return; }
        if op == Create && (state.is_none() || self.host_link().is_some()) { return; }
        self.share.confirm_stop = false;
        self.share.request += 1;
        self.share.operation = Some(op);
        let command = Command::Host { id: self.share.board.clone(), request: self.share.request, operation: op };
        self.share.copied = false;
        self.share.result = None;
        if self.commands.as_ref().is_some_and(|tx| tx.try_send(command).is_ok()) {
            self.share.busy = true;
        } else {
            self.share.result = Some(Err("The local board connection isn't ready yet.".into()));
        }
    }
    pub fn host_link(&self) -> Option<&str> {
        if self.status != "live" || !self.can_share_board(&self.share.board) { return None; }
        let state = self.share.result.as_ref()?.as_ref().ok()?;
        let link = state.link.as_deref()?;
        (state.route == neochat::sharing::RouteState::Connected && neochat::invite_file::InviteFile::from_link(link).ok()?.project.as_deref() == Some(&self.share.board)).then_some(link)
    }

    pub fn disconnect_guest(&mut self) {
        if !self.remote() { return; }
        if self.pending || self.onboarding.busy { self.error = Some("Wait for the pending change before disconnecting.".into()); return; }
        // Drop the worker and its update stream. In-flight reads cannot update this session.
        self.commands = None;
        self.updates = None;
        self.status = "disconnected".into();
    }

    pub fn reconnect_guest(&mut self) {
        if !self.remote() || self.status != "disconnected" { return; }
        if let Some(wake) = self.connection_wake.clone() {
            let (commands, updates) = connection::start(self.options.clone(), wake);
            self.commands = Some(commands);
            self.updates = Some(updates);
            self.status = "connecting".into();
        }
    }
    pub fn remote(&self) -> bool { self.options.endpoint.is_some() || self.options.invite.is_some() }

    pub fn projects(&self) -> std::collections::BTreeSet<&str> {
        let board = self.board_cache.as_ref().unwrap_or(&self.snapshot);
        projects::available(board, self.remote()).into_iter()
            .filter(|id| self.options.project.as_deref().is_none_or(|locked| locked == *id)).collect()
    }

    pub fn resolve_project(&mut self) {
        if !self.onboarding.busy && !self.onboarding.adding && self.onboarding.step == Step::Name {
            let projects = self.projects();
            if !projects.contains(self.project.as_str()) {
                self.project = projects.first().copied().unwrap_or_default().to_owned();
            }
        }
    }

    pub fn switch_saved_board(&mut self, profile: &saved_boards::BoardProfile) {
        self.project_menu = false;
        if self.onboarding.busy { return; }
        if self.options.endpoint.is_none() && profile.invite == self.options.invite && self.projects().contains(profile.project.as_str()) {
            self.select_project(&profile.project);
            return;
        }
        if self.has_unsent_work() {
            self.error = Some("Finish or clear your drafts before switching servers.".into());
            return;
        }
        if self.commands.as_ref().is_some_and(|tx| tx.try_send(Command::SwitchBoard { id: profile.project.clone(), path: profile.invite.clone() }).is_ok()) {
            self.onboarding = Onboarding { busy: true, switching: true, ..Default::default() };
        } else {
            self.error = Some("The board connection isn't ready yet. Try again shortly.".into());
        }
    }

    fn has_unsent_work(&self) -> bool {
        self.pending || !self.draft.text.trim().is_empty() || self.compose_file_open
            || !self.reply.text.trim().is_empty() || self.reply_drafts.values().any(|d| !d.text.trim().is_empty())
    }

    pub fn default_channel(&mut self) {
        let general = projects::channel(&self.project, "general");
        let channel = if self.snapshot.channels.contains(&general) { general }
            else { self.snapshot.channels.first().cloned().unwrap_or_default() };
        self.channel = edit(&channel);
    }

    pub fn onboarding_visible(&self) -> bool {
        self.onboarding.adding || self.onboarding.switching || self.onboarding.step == Step::Connect || (!self.fixture && self.project.is_empty())
    }

    pub fn begin_add_board(&mut self) {
        self.project_menu = false;
        self.onboarding = Onboarding { adding: true, step: Step::Board, joining: self.remote(),
            name: edit(self.me.as_ref().and_then(|m| m.display_name.as_deref()).unwrap_or(&self.local_name)), ..Default::default() };
    }

    pub fn submit_board_setup(&mut self) {
        if self.onboarding.busy { return; }
        let name = self.onboarding.name.text.trim().to_owned();
        if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
            self.onboarding.error = Some("Enter a display name (1 to 80 characters).".into());
            return;
        }
        let command = if self.onboarding.joining {
            if self.has_unsent_work() {
                self.onboarding.error = Some("Finish or clear your drafts before joining another server.".into());
                return;
            }
            let o = &mut self.onboarding;
            let invite = match o.method {
                JoinMethod::Invite => match neochat::invite_file::InviteFile::from_link(o.invite.text.trim()) {
                    Ok(invite) => connection::JoinSource::Link(invite),
                    Err(error) => { o.error = Some(error); return; }
                },
                JoinMethod::Password => {
                    let (endpoint, project) = match password_target(&o.server.text, &o.slug.text) {
                        Ok(target) => target,
                        Err(error) => { o.error = Some(error.into()); return; }
                    };
                    if o.password.text.is_empty() { o.error = Some("Enter the board password.".into()); return; }
                    let password = connection::JoinPassword::new(std::mem::take(&mut o.password.text));
                    o.password = edit("").secure();
                    connection::JoinSource::Password { endpoint, project, password }
                }
            };
            Command::JoinBoard { invite, name }
        } else {
            let id = self.onboarding.board.text.split_whitespace().collect::<Vec<_>>().join("-");
            if !neochat::valid_project(&id) {
                self.onboarding.error = Some("Use 1 to 80 letters, digits, spaces, dashes, underscores, or dots.".into());
                return;
            }
            if self.status != "live" || self.remote() || self.pending || !self.draft.text.trim().is_empty() || self.compose_file_open {
                self.onboarding.error = Some("Connect to your local board and finish any open draft before creating another board.".into());
                return;
            }
            Command::CreateBoard { id, name }
        };
        self.onboarding.error = None;
        if self.commands.as_ref().is_some_and(|tx| tx.try_send(command).is_ok()) {
            self.onboarding.invite.clear();
            self.onboarding.busy = true;
        } else {
            self.onboarding.error = Some("The board connection isn't ready yet. Try again shortly.".into());
        }
    }

    pub fn finish_board_setup(&mut self, result: Result<connection::BoardSetup, String>) {
        self.onboarding.busy = false;
        let switching = std::mem::take(&mut self.onboarding.switching);
        match result {
            Ok((id, mut snapshot, member, hosted)) => {
                self.onboarding.clear_join_fields();
                if hosted.is_some() || switching {
                    self.organization = Default::default();
                    if self.options.endpoint.is_none() {
                        for id in self.projects().into_iter().map(str::to_owned).collect::<Vec<_>>() {
                            self.saved_boards.remember(id, self.options.invite.clone(), None);
                        }
                    }
                    self.options.invite = hosted.as_ref().map(|h| h.path.clone());
                    self.options.endpoint = None;
                    self.options.project = hosted.as_ref().map(|_| id.clone());
                    self.seen.clear(); self.reply_drafts.clear(); self.reply_labels.clear();
                    self.reply_owner = None; self.reply = edit("");
                    self.board_extra = Default::default();
                }
                self.saved_boards.remember(id.clone(), self.options.invite.clone(), hosted.map(|h| h.endpoint));
                snapshot.messages.sort_by_key(|m| m.id);
                self.board_cache = Some(snapshot);
                self.project.clear();
                self.select_project(&id);
                self.me = Some(member);
                self.navigate(Page::Home);
                if switching { self.onboarding = Default::default(); }
                else { self.onboarding.step = Step::Connect; }
            }
            Err(error) => {
                if switching { self.error = Some(error.clone()); self.onboarding.error = Some(error); }
                else { self.onboarding.error = Some(error); }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(rt: &mut rune::Runtime<App>, key: &str) {
        let (x, y) = rt.find(key).unwrap_or_else(|| panic!("missing {key}"));
        rt.pointer_move(x, y); rt.pointer_down(x, y); rt.pointer_up(x, y); rt.frame(1. / 60.);
    }

    #[test]
    fn password_join_targets_are_unambiguous_and_https_only() {
        assert_eq!(password_target("https://boards.invalid/prefix/#neoboard=QA_42", ""),
            Ok(("https://boards.invalid/prefix/".into(), "QA_42".into())));
        for (address, slug, expected) in [
            ("https://host", "", "general"), ("https://host/", " design ", "design"),
            ("https://host/#neoboard=dev", "", "dev"), ("https://host/#neoboard=dev", "dev", "dev"),
        ] {
            assert_eq!(password_target(address, slug), Ok(("https://host/".into(), expected.into())));
        }
        for (address, slug) in [("http://localhost", ""), ("https://user:pass@host", ""), ("https://host/?key=x", ""),
            ("https://host/#neoboard=dev&key=x", ""), ("https://host/#neoboard=dev", "other"),
            ("https://host/#neoboard=", ""), ("https://host", "../other"), ("https://host/#foo", "")] {
            assert!(password_target(address, slug).is_err());
        }
    }

    #[test]
    fn password_join_is_masked_transient_and_cancellable() {
        let mut app = App::new(); fixture::load(&mut app, false); app.begin_add_board();
        let (tx, rx) = mpsc::sync_channel(1); app.commands = Some(tx);
        let mut rt = rune::Runtime::new(app, view::root, (850., 600.), rune::Env::new(true, 1.));
        rt.frame(0.);
        click(&mut rt, "onboarding-join"); click(&mut rt, "method-password");
        const SECRET: &str = " sentinel-secret ";
        rt.update(|a, _| {
            a.onboarding.server.replace_text("https://host/#neoboard=dev");
            a.onboarding.name.replace_text("Sam Lee");
        });
        for _ in 0..60 { rt.frame(1./60.); }
        click(&mut rt, "onboarding-password"); rt.paste(SECRET); rt.frame(0.);
        assert!(rt.state.onboarding.password.text == SECRET);
        assert!(rt.state.onboarding.password.is_secure());
        assert!(!format!("{:?}", rt.state.onboarding.password).contains(SECRET.trim()));
        assert!(!format!("{:?}", rt.elements()).contains(SECRET.trim()));
        assert!(!serde_json::to_string(&rt.state.preferences()).unwrap().contains(SECRET.trim()));
        assert!(!crate::setup_guide::Guide::new(&rt.state).copy_text().contains(SECRET.trim()));
        click(&mut rt, "onboarding-submit");
        assert!(matches!(rx.try_recv(), Ok(Command::JoinBoard { invite: connection::JoinSource::Password { endpoint, project, .. }, name })
            if endpoint == "https://host/" && project == "dev" && name == "Sam Lee"));
        assert!(rt.state.onboarding.busy && rt.state.onboarding.password.text.is_empty());
        rt.update(|a, _| a.finish_board_setup(Err("Join failed".into())));
        for _ in 0..60 { rt.frame(1./60.); }
        rt.update(|a, _| a.onboarding.password.replace_text(SECRET));
        click(&mut rt, "method-invite");
        assert!(rt.state.onboarding.password.text.is_empty() && rt.state.onboarding.server.text.is_empty());
        for _ in 0..60 { rt.frame(1./60.); }
        click(&mut rt, "method-password");
        rt.update(|a, _| a.onboarding.password.replace_text(SECRET));
        for _ in 0..60 { rt.frame(1./60.); }
        click(&mut rt, "onboarding-cancel");
        assert!(!rt.state.onboarding_visible() && rt.state.onboarding.password.text.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn onboarding_modal_leaves_only_the_titlebar_draggable() {
        for adding in [false, true] {
            let mut app = App::new();
            if adding { fixture::load(&mut app, false); app.begin_add_board(); }
            let mut rt = rune::Runtime::new(app, view::root, (850., 600.), rune::Env::new(true, 1.));
            let mut insets = rune::ChromeInsets::default();
            insets.height = view::TITLEBAR_H; insets.leading = 76.;
            rt.set_chrome_insets(insets);
            for _ in 0..5 { rt.frame(1. / 60.); }
            let press = |rt: &mut rune::Runtime<App>, x, y| {
                rt.pointer_move(x, y); rt.pointer_down(x, y);
                let action = rt.take_titlebar_action();
                rt.pointer_up(x, y); rt.frame(1. / 60.);
                action
            };
            assert_eq!(press(&mut rt, 425., insets.height / 2.), Some(rune::TitlebarAction::Drag));
            assert_eq!(press(&mut rt, 12., insets.height + 2.), None);
            if adding {
                click(&mut rt, "projects-toggle");
                assert!(!rt.state.project_menu, "the board behind onboarding stays modal-blocked");
                click(&mut rt, "onboarding-join");
            }
            let field = if adding { "onboarding-invite" } else { "onboarding-name" };
            let (x, y) = rt.find(field).unwrap();
            assert_eq!(press(&mut rt, x, y), None);
            rt.key(rune::KeyEvent { key: Key::Text("sample".into()), mods: Default::default() });
            rt.frame(1. / 60.);
            let onboarding = &rt.state.onboarding;
            assert_eq!((if adding { &onboarding.invite } else { &onboarding.name }).text, "sample");
            if !adding { click(&mut rt, "onboarding-next"); assert!(rt.find("onboarding-board-name").is_some()); }
        }
    }

    #[test]
    fn empty_daemon_requires_onboarding_and_creation_opens_the_agent_step() {
        let mut app = App::new();
        app.board_cache = Some(Snapshot { cursor: 0, channels: vec!["general".into()], messages: vec![] });
        app.resolve_project();
        assert!(app.projects().is_empty());
        app.status = "live".into();
        let (commands, requests) = mpsc::sync_channel(1);
        app.commands = Some(commands);
        let mut rt = rune::Runtime::new(app, view::root, (850., 600.), rune::Env::new(true, 1.));
        rt.frame(1. / 60.);
        assert!(rt.find("sidebar").is_none());
        click(&mut rt, "onboarding-next");
        assert!(rt.state.onboarding.error.is_some());
        rt.update(|a, _| a.onboarding.name = edit("Sam Lee"));
        rt.frame(1. / 60.);
        click(&mut rt, "onboarding-next");
        rt.update(|a, _| a.onboarding.board = edit("Design Team"));
        rt.frame(1. / 60.);
        click(&mut rt, "onboarding-submit");
        let Command::CreateBoard { id, name } = requests.try_recv().unwrap() else { panic!("expected create board"); };
        assert_eq!(id, "Design-Team");
        assert_eq!(name, "Sam Lee");
        rt.update(|a, _| a.finish_board_setup(Ok((id, Snapshot { cursor: 1, channels: vec!["Design-Team/general".into()], messages: vec![] },
            neochat::protocol::Member { actor: "local".into(), display_name: Some(name) }, None))));
        for _ in 0..5 { rt.frame(1. / 60.); }
        click(&mut rt, "onboarding-copy");
        assert_eq!(rt.state.home_copied, Some("onboarding-copy"));
        click(&mut rt, "onboarding-open");
        assert_eq!(rt.state.project, "Design-Team");
        assert!(rt.find("board-onboarding").is_none());
    }

    #[test]
    fn join_accepts_a_private_link_not_a_path_or_tunnel_address() {
        let mut app = App::new();
        app.onboarding.joining = true;
        app.onboarding.name = edit("Sam");
        let (commands, requests) = mpsc::sync_channel(1);
        app.commands = Some(commands);
        for invalid in ["/tmp/guest.neochat", "https://board.example/"] {
            app.onboarding.invite = edit(invalid);
            app.submit_board_setup();
            assert!(app.onboarding.error.is_some() && requests.try_recv().is_err());
        }
        app.onboarding.invite = edit(&format!("https://board.example/#neoboard=design&expires=4102444800&key={}", neochat::board::random_token()));
        app.submit_board_setup();
        assert!(matches!(requests.try_recv(), Ok(Command::JoinBoard { .. })));
    }

    #[test]
    fn sidebar_picker_sits_below_native_chrome_and_opens_add_board() {
        let mut app = App::new();
        fixture::load(&mut app, false);
        let mut rt = rune::Runtime::new(app, view::root, (850., 600.), rune::Env::new(true, 1.));
        let mut insets = rune::ChromeInsets::default();
        insets.leading = 76.; insets.height = view::TITLEBAR_H;
        rt.set_chrome_insets(insets);
        for _ in 0..5 { rt.frame(1. / 60.); }
        let (x, y) = rt.find("projects-toggle").unwrap();
        assert!(x < 199. && y > insets.height && y < insets.height + 45.);
        click(&mut rt, "projects-toggle");
        click(&mut rt, "add-board");
        assert!(rt.find("board-onboarding").is_some());
        assert!(rt.find("onboarding-join").is_some());
    }

    #[test]
    fn hosted_picker_preserves_same_named_connections_after_restart() {
        use neochat::invite_file::InviteFile;
        let dir = tempfile::tempdir().unwrap();
        let prefs_path = dir.path().join("desktop.json");
        let mut app = App::new();
        fixture::load(&mut app, false);
        let id = app.project.clone();
        let (commands, requests) = mpsc::sync_channel(4);
        app.commands = Some(commands.clone());
        let mut rt = rune::Runtime::new(app, view::root, (850., 600.), rune::Env::new(true, 1.));
        let mut paths = Vec::new();
        for (i, host) in ["east", "west"].into_iter().enumerate() {
            for _ in 0..4 { rt.frame(1./60.); }
            click(&mut rt, "projects-toggle");
            click(&mut rt, "add-board");
            if !rt.state.onboarding.joining { click(&mut rt, "onboarding-join"); }
            assert!(rt.find("onboarding-invite").is_some());
            let endpoint = format!("https://{host}.example.invalid/");
            let link = format!("{endpoint}#neoboard={id}&expires=4102444800&key=fixture-only-{host}");
            rt.update(|a,_| a.onboarding.invite.replace_text(link));
            // Switching to Join animates the form; a press during the first frame misses the button.
            for _ in 0..60 { rt.frame(1./60.); }
            click(&mut rt, "onboarding-submit");
            let Command::JoinBoard { invite: connection::JoinSource::Link(invite), .. } = requests.try_recv().unwrap() else { panic!("expected join"); };
            assert_eq!(invite.endpoint, endpoint);
            let path = invite.store(dir.path()).unwrap();
            let snapshot = Snapshot { cursor: i as u64 + 1, channels: vec![format!("{id}/{host}")], messages: vec![] };
            let member = neochat::protocol::Member { actor: "fixture".into(), display_name: None };
            rt.update(|a,_| a.finish_board_setup(Ok((id.clone(), snapshot, member, Some(connection::HostedBoard { path: path.clone(), endpoint })))));
            rt.frame(1./60.);
            click(&mut rt, "onboarding-open");
            paths.push(path);
        }
        rt.state.preferences().write(&prefs_path).unwrap();
        let bytes = std::fs::read(&prefs_path).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("fixture-only"));
        drop(rt);

        let mut app = App::new();
        app.restore_preferences(Preferences::load(&prefs_path).unwrap());
        assert_eq!(app.options.invite.as_ref(), paths.last());
        assert_eq!(app.options.project.as_ref(), Some(&id));
        assert_eq!(app.saved_boards.entries().len(), 3);
        fixture::load(&mut app, false);
        app.commands = Some(commands);
        let mut rt = rune::Runtime::new(app, view::root, (850., 600.), rune::Env::new(true, 1.));
        for (index, expected) in [(1, Some(&paths[0])), (0, None), (2, Some(&paths[1]))] {
            for _ in 0..4 { rt.frame(1./60.); }
            click(&mut rt, "projects-toggle");
            click(&mut rt, &format!("saved-board-{index}"));
            let Command::SwitchBoard { id: target, path } = requests.try_recv().unwrap() else { panic!("expected switch"); };
            assert_eq!(target, id);
            assert_eq!(path.as_ref(), expected);
            let hosted = path.map(|path| connection::HostedBoard { endpoint: InviteFile::read(&path).unwrap().endpoint, path });
            let snapshot = Snapshot { cursor: index, channels: vec![format!("{id}/server-{index}")], messages: vec![] };
            rt.update(|a,_| a.finish_board_setup(Ok((target, snapshot, neochat::protocol::Member { actor:"fixture".into(), display_name:None }, hosted))));
            assert_eq!(rt.state.options.invite.as_ref(), expected);
            assert_eq!(rt.state.channel.text, format!("{id}/server-{index}"));
            assert_eq!(rt.state.saved_boards.entries().len(), 3);
        }
    }

    #[test]
    fn legacy_preferences_keep_active_invite_and_same_named_local_board() {
        let dir = tempfile::tempdir().unwrap();
        let active = dir.path().join("old.neochat");
        let other = dir.path().join("unavailable.neochat");
        let legacy = serde_json::json!({"sidebar":false,"oldest":true,"compact":false,"project":"team","invite":active,
            "saved_boards":{"team":null,"archive":other}});
        let mut app = App::new();
        app.restore_preferences(serde_json::from_value(legacy).unwrap());
        let migrated = serde_json::to_value(app.preferences()).unwrap();
        assert!(migrated.get("saved_boards").is_none());
        let mut restarted = App::new();
        restarted.restore_preferences(serde_json::from_value(migrated).unwrap());
        assert!(!restarted.sidebar && !restarted.compact);
        assert_eq!(restarted.options.invite.as_ref(), Some(&active));
        let entries = restarted.saved_boards.entries();
        assert_eq!(entries.len(), 3);
        for (id, path) in [("team", None), ("team", Some(active)), ("archive", Some(other))] {
            assert!(entries.iter().any(|p| p.project == id && p.invite == path));
        }
    }

    #[test]
    fn explicit_data_directories_have_independent_board_lists() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name| Preferences::path(&Options { data_dir: Some(dir.path().join(name)), ..Default::default() }).unwrap();
        assert_eq!(path("one"), path("one"));
        assert_ne!(path("one"), path("two"));
        assert_ne!(Some(path("one")), preferences_path());
    }

    #[test]
    fn share_targets_the_hovered_board_without_switching_and_collapse_hides_picker() {
        let mut app = App::new();
        fixture::load(&mut app, false);
        let active = app.project.clone();
        app.status = "live".into();
        let mut board = app.snapshot.clone();
        board.channels.push("another/general".into());
        app.board_cache = Some(board);
        let (commands, requests) = mpsc::sync_channel(1);
        app.commands = Some(commands);
        let mut rt = rune::Runtime::new(app, view::root, (850., 600.), rune::Env::new(true, 1.));
        for _ in 0..5 { rt.frame(1. / 60.); }
        assert!(rt.find("share-board-another").is_none());
        click(&mut rt, "projects-toggle");
        click(&mut rt, "share-board-another");
        assert_eq!(rt.state.project, active);
        assert_eq!(rt.state.share.board, "another");
        let Ok(Command::Host { id, operation: connection::HostOp::Inspect, .. }) = requests.try_recv() else { panic!("expected status lookup"); };
        assert_eq!(id, "another");
        assert!(rt.find("create-share-link").is_none());
        rt.update(|a, _| {
            a.share.busy = false;
            a.share.result = Some(Ok(connection::HostState { route: neochat::sharing::RouteState::Connected, link: None, endpoint: None }));
        });
        rt.frame(1. / 60.);
        click(&mut rt, "create-share-link");
        assert!(matches!(requests.try_recv(), Ok(Command::Host { id, operation: connection::HostOp::Create, .. }) if id == "another"));
        rt.update(|a, _| a.dialog = None);
        rt.frame(1. / 60.);
        click(&mut rt, "collapse-sidebar");
        assert!(rt.find("projects-toggle").is_none());
        assert!(rt.find("share-board-another").is_none());
    }

    #[test]
    fn guest_panel_disconnects_only_this_session_and_keeps_saved_board() {
        let mut app = App::new();
        fixture::load(&mut app, false);
        let path = std::path::PathBuf::from("/fixture/never-opened.neochat");
        app.options.invite = Some(path.clone());
        app.saved_boards.remember(app.project.clone(), Some(path.clone()), None);
        app.status = "live".into();
        let (commands, requests) = mpsc::sync_channel(4);
        app.commands = Some(commands);
        app.open_share();
        let mut rt = rune::Runtime::new(app, view::root, (850., 600.), rune::Env::new(true, 1.));
        rt.frame(1. / 60.);
        assert!(rt.find("guest-status").is_some());
        for key in ["disconnect-host", "create-share-link", "copy-share-link", "manage-password", "copy-password-address"] { assert!(rt.find(key).is_none()); }
        click(&mut rt, "disconnect-guest");
        assert_eq!(rt.state.status, "disconnected");
        assert!(rt.find("reconnect-guest").is_some());
        assert_eq!(rt.state.saved_boards.entries()[0].invite.as_ref(), Some(&path));
        assert!(matches!(requests.try_recv(), Err(mpsc::TryRecvError::Disconnected)));
    }

    #[test]
    fn host_password_requires_confirmation_and_never_publishes() {
        use crate::host_password::Panel;
        let mut app = App::new(); fixture::load(&mut app, false);
        app.status = "live".into();
        let mut board = app.snapshot.clone(); board.channels.push("another/general".into()); app.board_cache = Some(board);
        app.open_board_share("another");
        app.share.result = Some(Ok(connection::HostState { route: neochat::sharing::RouteState::Stopped, link: None, endpoint: None }));
        let (tx, rx) = mpsc::sync_channel(1); app.commands = Some(tx);
        let mut rt = rune::Runtime::new(app, view::root, (850., 600.), rune::Env::new(true, 1.));
        let press = |rt: &mut rune::Runtime<App>, key| {
            for _ in 0..30 { rt.frame(1./60.); }
            click(rt, key);
        };
        const SECRET: &str = " fixture-166 ";
        press(&mut rt, "manage-password");
        press(&mut rt, "host-new-password"); rt.paste(SECRET); rt.frame(0.);
        press(&mut rt, "review-password");
        assert!(matches!(&rt.state.share.password, Panel::Edit { new, repeat, error: Some(_) } if new.text.is_empty() && repeat.text.is_empty()));
        for key in ["host-new-password", "host-repeat-password"] {
            press(&mut rt, key); rt.paste(SECRET); rt.frame(0.);
        }
        assert!(matches!(&rt.state.share.password, Panel::Edit { new, repeat, .. } if new.is_secure() && repeat.is_secure() && new.text == SECRET && repeat.text == SECRET));
        assert!(!format!("{:?}", rt.elements()).contains(SECRET.trim()));
        assert!(!serde_json::to_string(&rt.state.preferences()).unwrap().contains(SECRET.trim()));
        press(&mut rt, "review-password");
        assert!(matches!(rt.state.share.password, Panel::ConfirmSet(_)) && rx.try_recv().is_err());
        press(&mut rt, "confirm-password");
        let Ok(Command::BoardPassword { request, id, password: Some(_) }) = rx.try_recv() else { panic!("expected only an admin password write"); };
        assert_eq!(id, "another");
        rt.update(|a, _| a.finish_password(request, true));
        assert!(matches!(rt.state.share.password, Panel::Notice(_)));
        assert!(rt.state.password_address().is_none() && !rt.state.share.copied);
        press(&mut rt, "manage-password"); press(&mut rt, "disable-password");
        assert!(rx.try_recv().is_err()); press(&mut rt, "cancel-password");
        assert!(matches!(rt.state.share.password, Panel::Closed) && rx.try_recv().is_err());
        press(&mut rt, "manage-password"); press(&mut rt, "disable-password"); press(&mut rt, "confirm-password");
        assert!(matches!(rx.try_recv(), Ok(Command::BoardPassword { id, password: None, .. }) if id == "another"));
        rt.update(|a, _| a.finish_password(request, true)); // Earlier set cannot complete this disable.
        assert!(matches!(rt.state.share.password, Panel::Saving { disable: true }));
        rt.update(|a, _| a.finish_password(a.share.request, false));
        press(&mut rt, "manage-password"); press(&mut rt, "host-new-password"); rt.paste(SECRET); rt.frame(0.);
        press(&mut rt, "dialog-close");
        assert!(matches!(rt.state.share.password, Panel::Closed) && rt.state.dialog.is_none() && rx.try_recv().is_err());
    }

    #[test]
    fn host_panel_discards_stale_results_recovers_scoped_link_and_confirms_stop() {
        use connection::{HostOp, HostState};
        use neochat::sharing::RouteState::*;
        let mut app = App::new();
        fixture::load(&mut app, false);
        app.status = "live".into();
        let (commands, requests) = mpsc::sync_channel(8);
        let (updates, rx) = mpsc::channel();
        app.commands = Some(commands);
        app.updates = Some(rx);
        app.open_share();
        let first = app.share.request;
        app.open_share();
        let second = app.share.request;
        for _ in 0..2 { assert!(matches!(requests.try_recv(), Ok(Command::Host { operation: HostOp::Inspect, .. }))); }
        let mut rt = rune::Runtime::new(app, view::root, (850., 600.), rune::Env::new(true, 1.));
        let apply = |rt: &mut rune::Runtime<App>, update| {
            updates.send(update).unwrap();
            rt.update(|a, tx| { poll(a, 0., tx); });
            rt.frame(1. / 60.);
        };
        apply(&mut rt, Update::Host(first, Ok(HostState { route: Stopped, link: None, endpoint: None })));
        assert!(rt.state.share.busy);
        let link = format!("https://fixture.invalid/#neoboard={}&expires=4102444800&key=private-fixture-key", rt.state.project);
        apply(&mut rt, Update::Host(second, Ok(HostState { route: Connected, link: Some(link), endpoint: Some("https://fixture.invalid/".into()) })));
        assert_eq!(rt.state.password_address(), Some(format!("https://fixture.invalid/#neoboard={}", rt.state.project)));
        click(&mut rt, "copy-share-link");
        assert!(rt.state.share.copied);
        for _ in 0..8 { rt.frame(1. / 60.); }
        click(&mut rt, "disconnect-host");
        assert!(rt.state.share.confirm_stop, "disconnect must request confirmation");
        for _ in 0..5 { rt.frame(1. / 60.); }
        assert!(requests.try_recv().is_err());
        click(&mut rt, "confirm-stop-host");
        let Ok(Command::Host { operation: HostOp::Stop, request, .. }) = requests.try_recv() else { panic!("expected confirmed stop"); };
        apply(&mut rt, Update::Host(request, Ok(HostState { route: Stopped, link: None, endpoint: None })));
        assert!(rt.find("disconnect-host").is_none());
        assert!(rt.find("copy-share-link").is_none());
        assert!(rt.find("create-share-link").is_some());
        click(&mut rt, "create-share-link");
        let Ok(Command::Host { operation: HostOp::Create, request, .. }) = requests.try_recv() else { panic!("expected explicit create"); };
        let link = format!("https://fixture.invalid/#neoboard={}&expires=4102444800&key=fixture", rt.state.project);
        apply(&mut rt, Update::Host(request, Ok(HostState { route: Connected, link: Some(link), endpoint: None })));
        assert!(rt.state.share.copied);
        apply(&mut rt, Update::Status("offline".into()));
        assert!(rt.find("copy-share-link").is_none());
        assert!(rt.find("disconnect-host").is_none());
        apply(&mut rt, Update::Status("live".into()));
        let Ok(Command::Host { operation: HostOp::Inspect, request, .. }) = requests.try_recv() else { panic!("expected refresh after reconnect"); };
        apply(&mut rt, Update::Host(request, Err("Unsupported by fixture daemon".into())));
        assert!(rt.find("retry-host").is_some());
        assert!(rt.find("create-share-link").is_none());
    }

    #[test]
    fn first_run_onboarding_scales_with_app_zoom() {
        let mut rt = rune::Runtime::new(App::new(), view::root, (900., 650.), rune::Env::new(true, 1.));
        let mut insets = rune::ChromeInsets::default();
        insets.leading = 76.; insets.height = view::TITLEBAR_H;
        rt.set_chrome_insets(insets);
        let width = |rt: &rune::Runtime<App>, key| rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).unwrap().rect[2];
        for _ in 0..10 { rt.frame(1. / 60.); }
        assert!(rt.find("onboarding-name").is_some());
        let panel = width(&rt, "board-onboarding");
        rt.update(|a, tx| {
            a.zoom.key(&rune::KeyEvent { key: Key::Command('='), mods: rune::Mods { cmd: true, ..Default::default() } }, tx);
        });
        for _ in 0..10 { rt.frame(1. / 60.); }
        assert!((width(&rt, "board-onboarding") - panel * 1.1).abs() < 1.);
    }
}