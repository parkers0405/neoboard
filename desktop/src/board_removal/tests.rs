use super::*;

fn runtime(app: App) -> rune::Runtime<App> {
    let mut rt = rune::Runtime::new(app, view::root, (850., 600.), rune::Env::new(true, 1.));
    for _ in 0..5 { rt.frame(1./60.); }
    rt
}

fn click(rt: &mut rune::Runtime<App>, key: &str) {
    for _ in 0..20 { rt.frame(1./60.); }
    let (x,y) = rt.find(key).unwrap_or_else(|| panic!("missing {key}"));
    rt.pointer_move(x,y); rt.pointer_down(x,y); rt.pointer_up(x,y); rt.frame(1./60.);
}

#[test]
fn host_requires_exact_name_and_preserves_other_connections() {
    let mut app = App::new();
    fixture::load(&mut app, false);
    let name = app.project.clone();
    app.saved_boards.remember(name.clone(), None, None);
    app.saved_boards.remember(name.clone(), Some("/fixture/guest".into()), None);
    app.status = "live".into(); app.project_menu = true;
    let (commands, rx) = mpsc::sync_channel(2); app.commands = Some(commands);
    let (updates, inbox) = mpsc::sync_channel(2); app.updates = Some(inbox);
    let mut rt = runtime(app);
    click(&mut rt, "remove-saved-board-0");
    click(&mut rt, "delete-board-name"); rt.paste("wrong"); rt.frame(0.);
    click(&mut rt, "confirm-remove-board"); click(&mut rt, "cancel-remove-board");
    assert!(rx.try_recv().is_err() && !rt.state.snapshot.messages.is_empty());
    for ok in [false, true] {
        if rt.state.dialog.is_none() { click(&mut rt, "projects-toggle"); click(&mut rt, "remove-saved-board-0"); }
        assert!(rt.state.removal.as_ref().unwrap().typed.text.is_empty());
        click(&mut rt, "delete-board-name"); rt.paste(&name); rt.frame(0.);
        rt.update(|a,_| { a.reply_labels.insert(1, edit("keep=me")); });
        click(&mut rt, "confirm-remove-board"); assert!(rx.try_recv().is_err());
        rt.update(|a,_| a.reply_labels.clear());
        click(&mut rt, "confirm-remove-board");
        assert!(matches!(rx.try_recv(), Ok(Command::DeleteBoard(id)) if id == name));
        rt.update(|a,_| a.close_dialog()); // Sent requests can't be cancelled or repeated.
        assert!(rt.state.removal.as_ref().unwrap().sending);
        click(&mut rt, "confirm-remove-board"); assert!(rx.try_recv().is_err());
        updates.send(Update::BoardDeleted(name.clone(), ok)).unwrap();
        rt.update(|a,tx| { poll(a, 0., tx); });
        if !ok { assert_eq!(rt.state.saved_boards.entries().len(), 2); assert!(!rt.state.snapshot.messages.is_empty()); }
    }
    assert!(rt.state.dialog.is_none() && rt.state.project.is_empty() && rt.state.thread.is_none());
    assert!(rt.state.snapshot.messages.is_empty() && rt.state.positions.is_empty() && rt.state.attachments.is_empty());
    let prefs = rt.state.preferences();
    assert!(prefs.project.is_empty() && prefs.invite.is_none());
    assert_eq!(prefs.board_profiles.entries().len(), 1);
    assert!(prefs.board_profiles.entries()[0].invite.is_some());
    rt.update(|a,_| a.fixture = false);
    assert!(!rt.state.onboarding_visible(), "remaining saved connections must be reachable");
}

#[test]
fn guest_forgets_only_selected_reference_even_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(); fixture::load(&mut app, false);
    let name = app.project.clone();
    app.saved_boards.remember(name.clone(), None, None);
    for host in ["east", "west"] {
        let path = dir.path().join(host);
        std::fs::write(&path, host).unwrap();
        app.saved_boards.remember(name.clone(), Some(path), Some(format!("https://{host}.invalid/")));
    }
    app.options.invite = Some(dir.path().join("west")); app.options.project = Some(name.clone());
    app.project_menu = true;
    let (commands, rx) = mpsc::sync_channel(2); app.commands = Some(commands);
    let (updates, inbox) = mpsc::sync_channel(2); app.updates = Some(inbox);
    let mut rt = runtime(app);
    rt.update(|a,_| a.draft = edit("unsent"));
    click(&mut rt, "remove-saved-board-1"); assert!(rt.find("delete-board-name").is_none());
    click(&mut rt, "confirm-remove-board");
    assert_eq!(rt.state.options.invite, Some(dir.path().join("west")));
    assert_eq!(rt.state.draft.text, "unsent"); assert_eq!(rt.state.saved_boards.entries().len(), 2);
    click(&mut rt, "projects-toggle"); click(&mut rt, "remove-saved-board-1");
    click(&mut rt, "confirm-remove-board");
    assert!(rt.state.options.invite.is_some() && !rt.state.snapshot.messages.is_empty());
    rt.update(|a,_| a.draft.clear());
    click(&mut rt, "confirm-remove-board");
    assert!(rt.state.snapshot.messages.is_empty() && rt.state.options.invite.is_none());
    assert!(rt.state.updates.is_none() && updates.try_send(Update::Status("live".into())).is_err());
    assert!(rx.try_recv().is_err());
    let path = dir.path().join("preferences.json"); rt.state.preferences().write(&path).unwrap();
    let mut restarted = App::new(); restarted.restore_preferences(Preferences::load(&path).unwrap());
    assert!(restarted.project.is_empty() && restarted.options.invite.is_none());
    assert_eq!(restarted.saved_boards.entries(), &[BoardProfile { project: name, invite: None, endpoint: None }]);
    for host in ["east", "west"] { assert_eq!(std::fs::read_to_string(dir.path().join(host)).unwrap(), host); }
}

#[test]
fn access_denied_purges_live_data_without_dropping_unsent_text() {
    let mut app = App::new(); fixture::saved_boards(&mut app); app.project_menu = false;
    app.board_cache = Some(app.snapshot.clone()); app.open_thread(1); app.reply = edit("unsent reply");
    app.draft = edit("unsent post");
    let (updates, inbox) = mpsc::sync_channel(2); app.updates = Some(inbox);
    let mut rt = runtime(app);
    updates.send(Update::AccessDenied).unwrap(); rt.update(|a,tx| { poll(a, 0., tx); }); rt.frame(0.);
    let a = &rt.state;
    assert!(a.board_cache.is_none() && a.snapshot.messages.is_empty() && a.snapshot.channels.is_empty());
    assert!(a.schedules.is_empty() && a.attachments.is_empty() && a.reactions.is_empty());
    assert!(a.thread.is_none() && rt.find("thread-panel").is_none() && a.status != "live");
    assert_eq!(a.reply.text, "unsent reply"); assert_eq!(a.draft.text, "unsent post");
    assert_eq!(a.saved_boards.entries().len(), 3);
    assert_eq!(a.local_drafts_text(), "Post: unsent post\n\nReply: unsent reply");
    click(&mut rt, "copy-local-drafts"); assert_eq!(rt.state.home_copied, Some("local-drafts"));
}