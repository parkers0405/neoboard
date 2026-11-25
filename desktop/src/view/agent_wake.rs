use super::*;
use crate::agent_wake::{self as wake, Action, Panel};

pub(super) fn body(app: &App, key: &str, panel: &Panel) -> El {
    let h = panel.harness;
    let action = |id, title: &str, verb| {
        let k = key.to_owned();
        let chat = panel.chat_id.clone();
        dialog_action(button(title, move |a| if a.agent_wake.as_ref().is_some_and(|p| p.chat_id==chat) { wake::start(a, &k, h, verb); })).disabled(panel.busy()).key(id)
    };
    let title = |s: &str| label(s).font(TextStyle::new(18., 600.)).color(gray(240));
    let mut body = col().w_full().gap(6.).child(title("Connected chats"));
    if panel.list_error { body = body.child(muted("Could not refresh this board’s chat list.")); }
    else if panel.chats.is_empty() { body = body.child(muted(if panel.busy() { "Checking saved chats…" } else { "No connected chats on this board yet." })); }
    for chat in &panel.chats {
        let Some(client) = chat.client() else { continue };
        let k=key.to_owned(); let id=chat.chat_id.clone();
        let selected=app.setup_agent!=setup_guide::Agent::ClaudeDesktop && h==client && panel.chat_id==id;
        body=body.child(button(&format!("{} · @{} · {}",client.name(),chat.actor,if chat.enabled {"Wake on"} else {"Paused"}),move |a| wake::choose_chat(a,&k,client,&id,false)).fill(gray(if selected {44} else {14})).disabled(panel.busy()))
            .child(muted(&chat.session).wrap());
    }
    if app.setup_agent==setup_guide::Agent::ClaudeDesktop {return body;}
    if panel.chats.iter().any(|chat| chat.client()==Some(h)) {
        let k=key.to_owned();
        body=body.child(button("Add another chat",move |a| wake::choose_chat(a,&k,h,"",true)).disabled(panel.busy()).key("wake-add-chat"));
    }
    body=body.child(line());
    let mut step = col().gap(9.);
    let mut actions = row().gap(6.).flex_wrap();
    let status = panel.status.as_deref().unwrap_or("");
    if let Some(on) = wake::paired(status) {
        step = step.child(title(if on { "Automatic wake is on" } else { "Automatic wake is off" }));
        if let Some(actor) = status.split(" | ").nth(1) { step = step.child(muted(format!("{} on this board", actor))); }
        if panel.confirming == Some(Action::Disconnect) {
            step=step.child(label("Remove this chat’s wake pairing? Its board access and other chats will not change.").wrap());
            actions=actions.child(action("wake-disconnect-confirm","Confirm disconnect",Action::Disconnect)).child(cancel());
        } else if panel.confirming == Some(Action::On) {
            step = step.child(label("Allow this chat to run tools and use model credits when it wakes?").wrap());
            actions = actions.child(action("wake-on", "Confirm turn on", Action::On)).child(cancel());
        } else {
            step = step.child(muted(if on { "Mentions and requested task updates wake this chat. Pausing won't interrupt a turn already running." } else { "Your chat stays connected. Turn on wake to receive mentions and requested task updates." }).wrap());
            actions = actions.child(if on { action("wake-off", "Pause", Action::Off) } else { action("wake-on", "Turn on", Action::On) });
            actions = actions.child(button(if panel.details { "Hide details" } else { "Details" }, |a| if let Some(p) = &mut a.agent_wake { p.details = !p.details; }).key("wake-details"));
        }
        if panel.details && panel.confirming.is_none() {
            step = step.child(actions).child(line()).child(muted(status).wrap()).child(muted("A test sends one turn, even while paused.").wrap());
            actions = row().gap(6.).child(action("wake-test", "Send test wake", Action::Test)).child(action("wake-status", "Refresh", Action::Status));
            actions=actions.child(action("wake-disconnect","Disconnect chat",Action::Disconnect));
            if let Some(test) = &panel.test { step = step.child(label(test).wrap()); }
        }
    } else if let Some(command) = &panel.command {
        let manual=panel.manual;
        step = step.child(title("Pair this chat")).child(row().gap(6.).child(button("Agent", |a| if let Some(p)=&mut a.agent_wake {p.manual=false;a.home_copied=None;}).fill(gray(if manual {10}else{44})).key("wake-mode-agent")).child(button("Manual", |a| if let Some(p)=&mut a.agent_wake {p.manual=true;a.home_copied=None;}).fill(gray(if manual {44}else{10})).key("wake-mode-manual")));
        step=step.child(muted(if manual {format!("Run this command in the terminal for the {} conversation you want to pair.",h.name())} else {format!("Paste the prompt into the {} conversation you want to wake. The agent pairs that chat on this device; you don’t need to find its terminal.",h.name())}).wrap());
        let Some(payload)=wake::pairing_text(command,h,manual) else {return body.child(muted("Could not prepare pairing instructions for this installation."));};
        if manual {step=step.child(col().w_full().padding(10.).radius(10.).fill(gray(3)).border(1.,gray(40)).child(el().w_full().scroll_x().child(label(&payload).font(code(11.)))));}
        let k = key.to_owned();
        let chat=panel.chat_id.clone();
        actions = actions.child(dialog_action(button(if app.home_copied == Some("wake-copy") { "Copied" } else if manual { "Copy command" } else { "Copy for agent" }, |_| {})).key("wake-copy").on_press_tx(move |a: &mut App, tx| {
            if wake::selected(a, &k, h) && a.agent_wake.as_ref().is_some_and(|p|p.chat_id==chat) { tx.copy(payload.clone()); a.home_copied = Some("wake-copy"); }
        })).child(action("wake-status", "Check connection", Action::Status));
        if status == "Not paired" && app.home_copied == Some("wake-copy") { step = step.child(muted("Waiting for pairing. Automatic wake is still off.")); }
        actions = actions.child(button("Start over", |a| if let Some(p) = &mut a.agent_wake { p.command=None; p.confirming=None; a.home_copied=None; }).disabled(panel.busy()));
    } else if status == "Not paired" {
        let local = wake::local_dir(app).is_some();
        let Some(target) = setup_guide::target(app) else { return body };
        let joined = target.profile.invite.is_some();
        let file = panel.dedicated || !local && !joined;
        step = step.child(title(&format!("Connect {}", h.name())));
        if panel.confirming == Some(Action::Create) {
            step = step.child(label(format!("Create a board identity for {} on {}? This won't send a message or turn wake on.", h.name(), target.label(app))).wrap());
            actions = actions.child(action("wake-create-confirm", "Create and continue", Action::Create)).child(cancel());
        } else if file {
            step = step.child(muted("Choose the invite file your board host issued to this agent. It gives the agent its own name, inbox, and permissions on this board. Don’t use your personal invite.").wrap());
            #[cfg(feature="native-picker")]
            {
                let k = key.to_owned();
                let chat = panel.chat_id.clone();
                actions = actions.child(dialog_action(button(if panel.invite.text.is_empty() {"Choose invite…"} else {"Change file…"}, |_| {})).disabled(panel.busy()).key("wake-choose").on_press_tx(move |_: &mut App, tx| {
                    let k = k.clone(); let chat=chat.clone(); tx.open_file(rune::FileDialogOptions { title: "Choose agent invite".into(), ..Default::default() }, move |a: &mut App, result, _| {
                        if wake::selected(a, &k, h) && a.agent_wake.as_ref().is_some_and(|p|p.chat_id==chat) { let p = a.agent_wake.as_mut().unwrap(); match result {
                            Ok(Some(path)) if path.to_str().is_some() => { p.invite = edit(path.to_str().unwrap()); p.notice = None; }, Ok(None) => (), _ => p.notice=Some("Could not open invite file.".into()),
                        }}
                    });
                }));
                if let Some(name) = std::path::Path::new(&panel.invite.text).file_name() {step=step.child(label(name.to_string_lossy()).wrap());}
            }
            #[cfg(not(feature="native-picker"))]
            { step=step.child(form_input(&panel.invite,"wake-invite","Invite file path").on_edit(|a: &mut App| &mut a.agent_wake.as_mut().unwrap().invite)); }
            if !panel.invite.text.is_empty() { actions=actions.child(action("wake-prepare","Continue",Action::Prepare)); }
        } else {
            step = step.child(muted(if joined { "Use the saved invite if it was issued to this agent. If you joined as yourself, choose the agent’s invite file instead." } else { "First time? Choose Connect chat to give the agent its own name and inbox on this board. Already have an invite file for this agent? Use that instead." }).wrap())
                .child(muted(format!("Next, paste a pairing prompt into the {} chat you want to wake, or choose Manual for a terminal command. Wake stays off until you turn it on.", h.name())).wrap());
            actions = actions.child(if local {action("wake-create","Connect chat…",Action::Create)} else {action("wake-prepare","This is the agent’s saved invite",Action::Prepare)});
        }
        if (local || joined) && panel.confirming.is_none() {
            actions = actions.child(button(if file {"Back"} else {"Use another invite…"}, |a| if let Some(p)=&mut a.agent_wake {p.dedicated=!p.dedicated;p.notice=None;p.invite=edit("");}).disabled(panel.busy()).key("wake-identity-source"));
        }
    } else {
        step=step.child(muted(if panel.busy() {"Checking connection…"} else {"Connection status unavailable."}));
        if !panel.busy() { actions=actions.child(action("wake-status","Check connection",Action::Status)); }
    }
    body.child(step.child(actions)).children(panel.notice.as_ref().map(|n| label(n).wrap()))
}

fn cancel() -> El {
    button("Cancel", |a| if let Some(p) = &mut a.agent_wake { p.confirming = None; }).key("wake-create-cancel")
}