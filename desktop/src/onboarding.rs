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
}

#[derive(Clone, Copy, Default, PartialEq)]
pub enum Step { #[default] Name, Board, Connect }

pub struct Onboarding {
    pub step: Step,
    pub name: TextEdit,
    pub board: TextEdit,
    pub invite: TextEdit,
    pub joining: bool,
    pub adding: bool,
    pub busy: bool,
    pub switching: bool,
    pub error: Option<String>,
}

impl Default for Onboarding {
    fn default() -> Self {
        Self { step: Step::Name, name: edit(""), board: edit(""), invite: edit(""), joining: false,
            adding: false, busy: false, switching: false, error: None }
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

    pub fn switch_board(&mut self, id: &str) {
        self.project_menu = false;
        if id == self.project || self.onboarding.busy { return; }
        if self.projects().contains(id) { self.select_project(id); return; }
        let Some(path) = self.saved_boards.get(id).cloned() else { return; };
        if self.has_unsent_work() {
            self.error = Some("Finish or clear your drafts before switching servers.".into());
            return;
        }
        if self.commands.as_ref().is_some_and(|tx| tx.try_send(Command::SwitchBoard { id: id.into(), path }).is_ok()) {
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
            let input = self.onboarding.invite.text.trim();
            let invite = match neochat::invite_file::InviteFile::from_link(input) {
                Ok(invite) => connection::JoinSource::Link(invite),
                Err(error) => { self.onboarding.error = Some(error); return; }
            };
            if self.has_unsent_work() {
                self.onboarding.error = Some("Finish or clear your drafts before joining another server.".into());
                return;
            }
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
            self.onboarding.busy = true;
        } else {
            self.onboarding.error = Some("The board connection isn't ready yet. Try again shortly.".into());
        }
    }

    pub fn finish_board_setup(&mut self, result: Result<connection::BoardSetup, String>) {
        self.onboarding.busy = false;
        let switching = std::mem::take(&mut self.onboarding.switching);
        match result {
            Ok((id, mut snapshot, member, path)) => {
                self.onboarding.invite = edit("");
                if path.is_some() || switching {
                    self.organization = Default::default();
                    for (id, location) in self.projects().into_iter().map(|id| (id.to_owned(), self.options.invite.clone())).collect::<Vec<_>>() {
                        if self.options.endpoint.is_none() { self.saved_boards.insert(id, location); }
                    }
                    self.options.invite = path.clone();
                    self.options.endpoint = None;
                    self.options.project = path.as_ref().map(|_| id.clone());
                    self.seen.clear(); self.reply_drafts.clear(); self.reply_labels.clear();
                    self.reply_owner = None; self.reply = edit("");
                    self.board_extra = Default::default();
                }
                self.saved_boards.insert(id.clone(), path);
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
            a.share.result = Some(Ok(connection::HostState { route: neochat::sharing::RouteState::Connected, link: None }));
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
        app.saved_boards.insert(app.project.clone(), Some(path.clone()));
        app.status = "live".into();
        let (commands, requests) = mpsc::sync_channel(4);
        app.commands = Some(commands);
        app.open_share();
        let mut rt = rune::Runtime::new(app, view::root, (850., 600.), rune::Env::new(true, 1.));
        rt.frame(1. / 60.);
        assert!(rt.find("guest-status").is_some());
        for key in ["disconnect-host", "create-share-link", "copy-share-link"] { assert!(rt.find(key).is_none()); }
        click(&mut rt, "disconnect-guest");
        assert_eq!(rt.state.status, "disconnected");
        assert!(rt.find("reconnect-guest").is_some());
        assert_eq!(rt.state.saved_boards.get(&rt.state.project), Some(&Some(path)));
        assert!(matches!(requests.try_recv(), Err(mpsc::TryRecvError::Disconnected)));
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
        apply(&mut rt, Update::Host(first, Ok(HostState { route: Stopped, link: None })));
        assert!(rt.state.share.busy);
        let link = format!("https://fixture.invalid/#neoboard={}&expires=4102444800&key=private-fixture-key", rt.state.project);
        apply(&mut rt, Update::Host(second, Ok(HostState { route: Connected, link: Some(link) })));
        click(&mut rt, "copy-share-link");
        assert!(rt.state.share.copied);
        for _ in 0..8 { rt.frame(1. / 60.); }
        click(&mut rt, "disconnect-host");
        assert!(rt.state.share.confirm_stop, "disconnect must request confirmation");
        for _ in 0..5 { rt.frame(1. / 60.); }
        assert!(requests.try_recv().is_err());
        click(&mut rt, "confirm-stop-host");
        let Ok(Command::Host { operation: HostOp::Stop, request, .. }) = requests.try_recv() else { panic!("expected confirmed stop"); };
        apply(&mut rt, Update::Host(request, Ok(HostState { route: Stopped, link: None })));
        assert!(rt.find("disconnect-host").is_none());
        assert!(rt.find("copy-share-link").is_none());
        assert!(rt.find("create-share-link").is_some());
        click(&mut rt, "create-share-link");
        let Ok(Command::Host { operation: HostOp::Create, request, .. }) = requests.try_recv() else { panic!("expected explicit create"); };
        let link = format!("https://fixture.invalid/#neoboard={}&expires=4102444800&key=fixture", rt.state.project);
        apply(&mut rt, Update::Host(request, Ok(HostState { route: Connected, link: Some(link) })));
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