use super::*;
use neochat::{client::Client, protocol::{Grant, InviteRequest, Member}};
use std::{path::PathBuf, process::{Child, Command as Process, Stdio}, sync::Arc};

fn restart(path: &Path) -> App {
    let mut app = App::new();
    app.personal.path = Some(path.to_owned());
    app.restore_name();
    app
}

#[test]
fn settings_offline_save_reset_and_failed_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("personal-name.json");
    let mut app = restart(&path);
    fixture::load(&mut app, false);
    app.status = "disconnected".into();
    app.me = Some(Member { actor: "guest-id".into(), display_name: Some("Old board name".into()) });
    app.dialog = Some(Dialog::Personal);
    app.open_personal();
    assert_eq!(app.personal_name.text, "Old board name");
    let mut rt = rune::Runtime::new(app, view::root, (900., 674.), rune::Env::new(true, 1.));
    rt.update(|a,_| a.personal_name = edit("  Zoë 王  "));
    for _ in 0..10 { rt.frame(1./60.); }
    let (x,y) = rt.find("save-personal-name").unwrap();
    rt.pointer_move(x,y); rt.pointer_down(x,y); rt.pointer_up(x,y); rt.frame(0.);
    assert_eq!(rt.state.device_label(), "Zoë 王");
    assert_eq!(rt.state.actor_label("guest-id/profile"), "Old board name");
    assert!(rt.state.name_status().contains("not verified"));
    rt.update(|a,_| { a.close_dialog(); a.navigate(Page::Home); });
    for _ in 0..12 { rt.frame(1./60.); }
    assert!(rt.elements().iter().any(|e| e.text.as_deref() == Some("Welcome back, Zoë 王")));

    let mut restored = restart(&path);
    assert_eq!(restored.device_label(), "Zoë 王");
    for bad in ["x\n", "\u{202e}x", &"é".repeat(129)] { assert!(restored.save_name(bad).is_err()); }
    assert_eq!(load(&path).unwrap().as_deref(), Some("Zoë 王"));
    restored.personal.path = Some(path.join("not-a-directory"));
    assert!(restored.save_name("cannot save").is_err());
    assert_eq!(restored.device_label(), "Zoë 王");
    restored.personal.path = Some(path.clone());
    restored.save_name("  ").unwrap();
    assert_eq!(restart(&path).personal.desired.as_deref(), Some(""));
}

#[test]
fn old_acknowledgements_cannot_confirm_new_membership_or_name() {
    let mut app = App::new();
    app.project = "test".into(); app.status = "live".into();
    app.personal.session = Some(2);
    let (tx,rx) = mpsc::sync_channel(2); app.commands = Some(tx);
    app.save_name("A").unwrap(); app.save_name("B").unwrap();
    assert!(matches!(rx.try_recv(), Ok(Command::Personal { session:2, revision:1, .. })));
    assert!(matches!(rx.try_recv(), Ok(Command::Personal { session:2, revision:2, .. })));
    for (session,revision) in [(1,2),(2,1)] {
        app.name_result(session,revision,Ok(Member{actor:"wrong".into(),display_name:Some("A".into())}));
        assert!(app.me.is_none() && app.name_status().contains("Waiting"));
    }
}

struct Daemon(Child);
impl Drop for Daemon { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }

#[track_caller]
fn wait_for(rt: &mut rune::Runtime<App>, predicate: impl Fn(&rune::Runtime<App>) -> bool) {
    let start = Instant::now();
    while !predicate(rt) {
        // The idle worker re-reads member directories after 60 seconds. Exercise
        // that production path, not an artificially fast test-only refresh.
        assert!(start.elapsed() < Duration::from_secs(75), "board update timeout: {} {:?} {}", rt.state.status, rt.state.page, rt.state.project);
        rt.update(|a,tx| { poll(a, start.elapsed().as_secs_f64(), tx); }); rt.frame(1./60.);
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[test]
#[ignore = "set NEOCHAT_DESKTOP_TEST_DAEMON to a freshly built neochatd"]
fn two_boards_other_reader_reconnect_and_restricted_membership() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("personal-name.json");
    let data = dir.path().join("daemon");
    let binary = PathBuf::from(std::env::var_os("NEOCHAT_DESKTOP_TEST_DAEMON").unwrap());
    let _daemon = Daemon(Process::new(binary).args(["--bind","127.0.0.1:0","--data-dir"]).arg(&data)
        .stdout(Stdio::null()).spawn().unwrap());
    let start = Instant::now();
    let host = loop {
        if let Ok(c) = Client::discover(Some(&data), None) { break c; }
        assert!(start.elapsed() < Duration::from_secs(10)); std::thread::sleep(Duration::from_millis(20));
    };
    let admin = Client::discover_admin(Some(&data)).unwrap(); // fixture provisioning only
    let reader = admin.invite(&InviteRequest {actor:"reader".into(),channels:vec![],expires_at:None}).unwrap();
    let reader_token = reader.token;
    let reader = Client::new(host.endpoint(), &reader_token, None).unwrap();
    let mut initial = restart(&path); initial.save_name("Rin Lee").unwrap(); drop(initial);
    for (board, writable) in [("east",true),("west",true),("locked",false)] {
        let actor = format!("{board}-guest"); let channel = format!("{board}/general");
        host.action(&Action::CreateChannel { channel: channel.clone() }).unwrap();
        let request = InviteRequest {actor:actor.clone(), channels:vec![channel.clone()],expires_at:None};
        let guest = if writable { admin.invite(&request) } else { admin.restricted_invite(&request,&[Grant::Read]) }.unwrap();
        let mut app = restart(&path);
        // Do not call the desktop release checker or any local discovery/bootstrap.
        app.options = Options { endpoint: Some(host.endpoint().into()), token: guest.token.clone(),
            project: Some(board.into()), profile: Some("fixed-profile".into()), ..Default::default() };
        app.status = "disconnected".into(); app.connection_wake = Some(Arc::new(||{}));
        app.reconnect_guest();
        let mut rt = rune::Runtime::new(app, view::root, (900.,674.), rune::Env::new(true,1.));
        wait_for(&mut rt, |r| r.state.personal.confirmed.is_some());
        assert_eq!(rt.state.personal.confirmed.as_ref().unwrap().is_ok(), writable);
        let members = reader.members(&channel).unwrap();
        let me = members.iter().find(|m| m.actor == actor).unwrap();
        assert_eq!(me.display_name.as_deref(), writable.then_some("Rin Lee"));
        assert_eq!(rt.state.device_label(), "Rin Lee");
        if writable {
            let c = Client::new(host.endpoint(), &guest.token, Some("fixed-profile")).unwrap().with_sender("desktop");
            c.action(&Action::Post{channel:channel.clone(), text:"hello".into(), labels:Default::default(),task:false,needs:vec![],assignee:None}).unwrap();
            let snap = reader.snapshot().unwrap(); let message = snap.messages.iter().find(|m| m.channel == channel).unwrap();
            assert_eq!(message.author, format!("{actor}/fixed-profile"));
            let mut other = App::new(); other.record_names(&members);
            assert_eq!(other.sender_label(message), "Rin Lee · Desktop");
            let mut agent = message.clone(); agent.sender = Some("mcp:Neoism".into());
            assert_eq!(other.sender_label(&agent), "Rin Lee’s Neoism");
            if board == "west" {
                other.aliases.clear(); // The observer starts without the assertion helper's directory.
                other.options = Options { endpoint: Some(host.endpoint().into()), token: reader_token.clone(),
                    project: Some(board.into()), ..Default::default() };
                other.status = "disconnected".into(); other.connection_wake = Some(Arc::new(||{}));
                other.reconnect_guest(); other.navigate(Page::Activity);
                let mut observer = rune::Runtime::new(other, view::root, (900.,674.), rune::Env::new(true,1.));
                let shown = |r: &rune::Runtime<App>, text: &str| r.elements().iter().any(|e|
                    !e.exiting && e.visible[2] > 0. && e.visible[3] > 0. && e.text.as_deref() == Some(text));
                wait_for(&mut observer, |r| shown(r, "Rin Lee · Desktop"));
                rt.update(|a,_| a.save_name("Rin Chen").unwrap());
                wait_for(&mut rt, |r| matches!(r.state.personal.confirmed, Some(Ok(()))));
                wait_for(&mut observer, |r| shown(r, "Rin Chen · Desktop"));
                // Separate credentials: east has not been opened again yet.
                assert_eq!(reader.members("east/general").unwrap().into_iter().find(|m|m.actor=="east-guest").unwrap().display_name.as_deref(),Some("Rin Lee"));
                rt.update(|a,_| a.save_name("").unwrap());
                wait_for(&mut rt, |r| matches!(r.state.personal.confirmed, Some(Ok(()))));
                wait_for(&mut observer, |r| shown(r, "fixed-profile · Desktop"));
                rt.update(|a,_| a.save_name("Rin Lee").unwrap());
                wait_for(&mut rt, |r| matches!(r.state.personal.confirmed, Some(Ok(()))));
            }
        } else { assert!(rt.state.name_status().contains("not confirmed")); }
        assert!(host.my_member().unwrap().display_name.is_none());
        assert!(reader.my_member().unwrap().display_name.is_none());
    }
}