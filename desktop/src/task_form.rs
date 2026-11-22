//! A chooser replaces the task dialog; only its query is transient.
use super::*;

fn property(name: &str, control: El) -> El {
    row().w_full().gap(8.).items_center().child(muted(name).w(96.))
        .child(control.w(0.).grow(1.))
}

pub(super) fn selector(app: &App, name: &str, key: &'static str, value: String) -> El {
    property(name, select_trigger(value, app.details_picker == Some(key), move |a| {
        a.task_search = edit("");
        a.details_picker = if a.details_picker == Some(key) { None } else { Some(key) };
        if key == "choose-assignee" && a.details_picker.is_some() { a.show_assignees(); }
    }).key(key))
}

fn task_name(m: &Message) -> String { format!("#{} {}", m.id, m.text.lines().next().unwrap_or("")) }
fn channel_name(ch: &str) -> String { format!("#{}", ch.split_once('/').map_or(ch, |(_,s)| s)) }

pub(super) fn properties(app: &App, panel: El) -> El {
    if app.creating_task { return panel.child(muted("Saving task…")); }
    let mut p = panel.child(label("Title"))
        .child(form_input(&app.task_title,"task-title","Short task title").on_edit(|a: &mut App| &mut a.task_title))
        .child(muted("Description · optional"))
        .child(text_input(&app.task_description,font()).key("task-description").w_full().h(66.).padding(10.).radius(7.).border(1.,gray(72)).color(gray(235))
            .placeholder("Context or acceptance criteria",gray(148)).on_edit(|a: &mut App| &mut a.task_description)).child(line());
    if app.task_global {
        p = p.child(property("Assigned to",label("Everyone"))).child(muted("Board-wide acknowledgement. Each person or agent marks this done for themselves.").wrap());
    } else {
        let channel = channel_name(&app.task_channel);
        p = p.child(if app.editing_task.is_some() || app.task_parent.is_some() { property("Channel", label(channel)) }
            else { selector(app,"Channel","pick-channel",channel) });
        let who = if app.assignee.text.is_empty() { "Unassigned" } else { app.actor_label(&app.assignee.text) };
        p = p.child(selector(app,"Assignee","choose-assignee",who.into()));
        if app.task_parent.is_none() { p = p.child(sprints::assignment(app)); }
        let parent = app.task_parent.map_or_else(|| "None".into(), |id| app.snapshot.messages.iter().find(|m| m.id==id).map_or_else(||format!("#{id}"),task_name));
        p = p.child(selector(app,"Parent task","pick-parent",parent));
        let ids = parse_needs(&app.needs.text).unwrap_or_default();
        p = p.child(selector(app,"Depends on","pick-needs",match ids.as_slice() { [] => "None".into(), [id] => format!("#{id}"), _ => format!("{} tasks",ids.len()) }));
    }
    if let Some(key) = app.details_picker.filter(|k| matches!(*k, "pick-channel"|"pick-sprint"|"choose-assignee"|"pick-parent"|"pick-needs")) { chooser(app, key); }
    p = p.child(property("Labels",form_input(&app.task_labels,"task-labels","category=design").on_edit(|a: &mut App| &mut a.task_labels)));
    if app.editing_task.is_none() {
        p = p.child(property("Attachment", if let Some(path) = &app.task_file {
            button(format!("{} ×",path.file_name().unwrap_or_default().to_string_lossy()),|a| a.task_file=None)
        } else { button("Attach file", |_| {}).on_press_tx(|_: &mut App, tx| {
            tx.open_file(rune::FileDialogOptions { title:"Attach a file to the task".into(), ..Default::default() }, |a: &mut App,result,_| match result {
                Ok(Some(path)) => a.task_file=Some(path), Ok(None) => {}, Err(e) => a.error=Some(e.to_string()),
            });
        }) }));
        if app.task_parent.is_none() {
            p = p.child(button(if app.task_advanced {"Hide advanced options"} else {"Advanced options"},|a| a.task_advanced=!a.task_advanced).key("task-advanced"));
            if app.task_advanced {
                p=p.child(choice_row("Global acknowledgement".into(),app.task_global,|a| a.task_global=!a.task_global).key("task-global-toggle"));
            }
        }
    }
    p
}

pub(super) struct Choices { query: String, items: Vec<El> }
impl Choices {
    pub(super) fn add(&mut self, name: String, selected: bool, key: String, action: impl Fn(&mut App)+'static) {
        if name.to_lowercase().contains(&self.query) { self.items.push(choice_row(name,selected,action).key(key)); }
    }
}

fn change_channel(a: &mut App, channel: &str) {
    if a.task_channel != channel {
        a.task_channel = channel.into(); a.task_parent=None; a.assignee=edit(""); a.needs=edit("");
    }
}

/// The open field's options, floating under it with a search box.
fn chooser(app: &App, key: &str) {
    let mut rows = Choices { query:app.task_search.text.trim().to_lowercase(), items:vec![] };
    let current = &app.task_channel;
    let mut note = None;
    match key {
        "pick-channel" => {
            for ch in app.snapshot.channels.iter().filter(|c| projects::contains(&app.project,c)) {
                let name = ch.clone(); rows.add(channel_name(ch),ch==current,format!("channel-choice-{ch}"),move|a| {change_channel(a,&name);a.details_picker=None;});
            }
        }
        "pick-sprint" => {
            sprints::choices(app,&mut rows);
            if app.editing_task.is_some() { note=Some("Choosing a sprint updates membership immediately."); }
        }
        "choose-assignee" => {
            rows.add("Unassigned".into(),app.assignee.text.is_empty(),"assign-none".into(),|a| {a.assignee=edit("");a.details_picker=None;});
            match app.member_list.as_ref().filter(|(ch,_)| ch==current).map(|(_,r)|r) {
                Some(Ok(members)) => for member in members {
                    let id=member.actor.clone(); rows.add(app.actor_label(&id).into(),id==app.assignee.text,format!("assign-member-{id}"),move|a| {a.assignee=edit(&id);a.details_picker=None;});
                }, Some(Err(e)) => note=Some(e.as_str()), None => note=Some("Loading members…"),
            }
        }
        "pick-parent" => {
            rows.add("No parent".into(),app.task_parent.is_none(),"parent-none".into(),|a| {a.task_parent=None;a.details_picker=None;});
            for m in app.snapshot.messages.iter().filter(|m| m.task.is_some() && &m.channel==current && Some(m.id)!=app.editing_task && app.organization.tasks.get(&m.id).is_none_or(|t| !t.global && t.parent.is_none())) {
                let id=m.id; rows.add(task_name(m),app.task_parent==Some(id),format!("parent-{id}"),move|a| {a.task_parent=Some(id);a.details_picker=None;});
            }
        }
        "pick-needs" => {
            let selected=parse_needs(&app.needs.text).unwrap_or_default();
            for m in app.snapshot.messages.iter().filter(|m| &m.channel==current && m.task.is_some() && Some(m.id)!=app.editing_task) {
                let id=m.id; rows.add(task_name(m),selected.contains(&id),format!("needs-{id}"),move|a| {
                    let mut ids=parse_needs(&a.needs.text).unwrap_or_default();
                    if ids.contains(&id) {ids.retain(|n| *n!=id);} else {ids.push(id);}
                    a.needs=edit(&ids.iter().map(u64::to_string).collect::<Vec<_>>().join(", "));
                });
            }
            note=Some("Select any tasks this task depends on in its channel.");
        }
        _ => {}
    }
    let mut list=col().w_full().max_h(300.).scroll(ScrollSpec{stick_bottom:false})
        .child(form_input(&app.task_search,"task-picker-search","Search").on_edit(|a:&mut App| &mut a.task_search).mb(4.))
        .children(note.map(|s|muted(s).wrap().px(8.).py(4.)));
    if rows.items.is_empty() { rows.items.push(muted("No matches").px(8.).py(6.)); }
    list=list.children(rows.items);
    if key=="pick-sprint" && app.editing_task.is_none() { list=list.child(line().my(4.)).child(choice_row("Create new sprint…".into(),false,|a| {a.error=None;a.details_picker=Some("new-sprint");}).key("new-sprint")); }
    if key=="pick-needs" { list=list.child(line().my(4.)).child(choice_row("Done".into(),false,|a| a.details_picker=None).key("task-picker-back")); }
    float_menu(app,key,list);
}
