//! Sprint navigation and task-form controls. Membership is independent of thread replies.
use super::*;

pub(crate) struct State {
    pub show: bool,
    detail: Option<u64>,
    confirm_finish: bool,
    pub assignment: Option<u64>,
    title: TextEdit,
    description: TextEdit,
}

impl Default for State {
    fn default() -> Self {
        Self { show: false, detail: None, confirm_finish: false, assignment: None, title: edit(""), description: edit("") }
    }
}

impl State {
    pub fn draft_fields(&self) -> [(&'static str, &str); 2] {
        [("Sprint title", &self.title.text), ("Sprint description", &self.description.text)]
    }
    pub fn clear_draft(&mut self) {
        self.assignment = None;
        self.title = edit("");
        self.description = edit("");
    }

    pub fn reset_view(&mut self) {
        self.show = false;
        self.detail = None;
        self.confirm_finish = false;
    }
}

impl App {
    pub(crate) fn sprint_created(&mut self, id: u64) {
        if self.dialog == Some(Dialog::NewSprint) {
            self.dialog = None;
            self.sprint.title = edit("");
            self.sprint.description = edit("");
            self.sprint.detail = Some(id);
            self.sprint.confirm_finish = false;
            return;
        }
        if self.dialog == Some(Dialog::NewTask) && self.details_picker == Some("new-sprint") {
            self.sprint.assignment = Some(id);
            self.details_picker = None;
            self.sprint.title = edit("");
            self.sprint.description = edit("");
        }
    }

    fn submit_sprint(&mut self) {
        let title = self.sprint.title.text.trim();
        if title.is_empty() || title.chars().any(char::is_control) {
            self.error = Some("Enter a single-line sprint title.".into());
            return;
        }
        self.dispatch(Action::CreateSprint { board: self.project.clone(), title: title.into(), description: self.sprint.description.text.trim().into() }, None);
    }
}

/// Tasks or Sprints, as a segmented switch at the top of the filter panel.
pub(super) fn tabs(app: &App) -> El {
    let tab = |name: &str, key: &str, active: bool| el().key(key.to_owned()).w(0.).grow(1.).h(28.).center().radius(7.)
        .fill(gray(if active { 40 } else { 14 })).hover(gray(if active { 40 } else { 26 }))
        .child(label(name).font(rune::TextStyle::new(13., if active { 600. } else { 450. })).color(gray(if active { 240 } else { 160 })));
    row().w_full().gap(2.).padding(3.).radius(10.).fill(gray(14)).border(1., gray(44))
        .child(tab("Tasks", "tasks-tab", !app.sprint.show).on_press(|a: &mut App| { a.sprint.reset_view(); a.thread = None; a.dialog = None; a.details_picker = None; }))
        .child(tab("Sprints", "sprints-tab", app.sprint.show).on_press(|a: &mut App| { a.sprint.reset_view(); a.sprint.show = true; a.thread = None; a.dialog = None; a.details_picker = None; }))
}

pub(super) fn current(app: &App) -> Option<&neochat::protocol::Sprint> {
    app.sprint.detail.filter(|_| app.sprint.show).and_then(|id| app.organization.sprints.get(&id)).filter(|s| s.board == app.project)
}

pub(super) fn new_sprint(app: &mut App) {
    if app.pending { return; }
    app.error = None;
    app.sprint.title = edit("");
    app.sprint.description = edit("");
    app.dialog = Some(Dialog::NewSprint);
}

pub(super) fn create_dialog(app: &App, viewport: [f32; 2]) -> El {
    let body = col().w_full().padding(16.).gap(10.)
        .child(label("Sprint title"))
        .child(form_input(&app.sprint.title,"sprint-title","Short sprint title").on_edit(|a: &mut App| &mut a.sprint.title))
        .child(label("Description"))
        .child(text_input(&app.sprint.description,font()).key("sprint-description").w_full().h(76.).padding(10.).border(1.,gray(72)).color(gray(235)).on_edit(|a: &mut App| &mut a.sprint.description))
        .children(app.error.as_ref().map(|e| muted(e).wrap()));
    let standalone = app.dialog == Some(Dialog::NewSprint);
    let back = move |a: &mut App| {
        if a.pending { return; }
        if standalone {
            a.dialog = None;
            a.sprint.title = edit("");
            a.sprint.description = edit("");
        } else { a.details_picker = None; }
        a.error = None;
    };
    modal_frame("create-sprint-dialog",viewport,436.,Some(label("Create sprint").padding(12.)),body,
        Some(row().w_full().padding(10.).child(button(if standalone {"Cancel"} else {"Back to task"},back).key("sprint-dismiss")).child(spacer()).child(button(if app.pending {"Creating…"} else {"Create sprint"},App::submit_sprint).key("submit-sprint"))),back)
}

fn selected(app: &App) -> Option<u64> {
    match app.editing_task { Some(id) => app.organization.task_sprints.get(&id).copied(), None => app.sprint.assignment }
}

pub(super) fn assignment(app: &App) -> El {
    let selected = selected(app);
    let title = selected.map(|id| app.organization.sprints.get(&id).map_or_else(|| format!("#{id}"), |s| s.title.clone())).unwrap_or_else(|| "None".into());
    task_form::selector(app, "Sprint", "pick-sprint", title)
}

pub(super) fn choices(app: &App, rows: &mut task_form::Choices) {
        let editing = app.editing_task;
        let mut option = |id, name| rows.add(name, selected(app) == id, format!("assign-sprint-{}", id.unwrap_or(0)), move |a| {
            if a.pending { return; }
            if let Some(task) = editing { a.dispatch(Action::SetTaskSprint { id: task, sprint: id },None); }
            else { a.sprint.assignment = id; }
            a.details_picker = None;
        });
        option(None,"No sprint".into());
        for s in app.organization.sprints.values().filter(|s| s.board == app.project && !s.finished) { option(Some(s.id),s.title.clone()); }
}

pub(super) fn browser(app: &App) -> El {
    let mut body = col().w_full().padding(16.).gap(8.);
    let membership = |m: &Message| app.organization.task_sprints.get(&m.id).copied().filter(|_| m.parent.is_none() && m.task.is_some() && projects::contains(&app.project,&m.channel));
    if let Some(s) = current(app) {
        let id = s.id;
        let members: Vec<_> = app.snapshot.messages.iter().filter(|m| membership(m) == Some(id)).collect();
        let total = members.len();
        let done = members.iter().filter(|m| m.task.as_ref().is_some_and(|t| t.state == TaskState::Completed)).count();
        body = body.child(button("‹ All sprints", |a| { a.sprint.detail = None; a.sprint.confirm_finish = false; a.thread = None; }).key("sprint-back"))
            .child(label(&s.title).wrap()).child(muted(&s.description).wrap()).child(label(format!("{done} of {total} main tasks complete"))).child(line()).child(label("Main tasks"));
        if total == 0 { body = body.child(muted("No main tasks assigned.")); }
        for m in members {
            let id = m.id;
            body = body.child(row().w_full().items_center().child(label(m.text.lines().next().unwrap_or("")).truncate().shrink()).child(spacer())
                .child(muted(state_name(&m.task.as_ref().unwrap().state))).child(button("Open thread",move |a| a.open_thread(id)).key(format!("sprint-thread-{id}"))));
        }
        body = body.child(line());
        if s.finished { body = body.child(label("Finished")); }
        else if app.sprint.confirm_finish && done < total {
            body = body.child(muted("Some main tasks are unfinished. Finish without changing them?"))
                .child(button("Finish anyway", move |a| a.dispatch(Action::FinishSprint {id,allow_unfinished:true},None)).key("finish-anyway"))
                .child(button("Keep sprint open", |a| a.sprint.confirm_finish = false));
        } else {
            body = body.child(button("Finish sprint",move |a| {
                if done < total { a.sprint.confirm_finish = true; }
                else { a.dispatch(Action::FinishSprint {id,allow_unfinished:false},None); }
            }).key("finish-sprint"));
        }
    } else {
        let mut progress = std::collections::HashMap::new();
        for m in &app.snapshot.messages {
            if let Some(id) = membership(m) {
                let (done,total) = progress.entry(id).or_insert((0usize,0usize));
                *total += 1;
                *done += usize::from(m.task.as_ref().is_some_and(|t| t.state == TaskState::Completed));
            }
        }
        let mut count = 0;
        for s in app.organization.sprints.values().filter(|s| s.board == app.project) {
            count += 1;
            let id = s.id;
            let (done,total) = progress.get(&id).copied().unwrap_or_default();
            body = body.child(row().key(format!("sprint-{id}")).w_full().px(12.).h(56.).gap(10.).items_center().radius(7.).border(1.,gray(48)).hover(gray(38))
                .child(col().w(0.).grow(1.).gap(3.).child(label(&s.title).truncate()).child(row().items_center().gap(6.)
                    .child(row().w(44.).h(4.).fill(gray(50)).radius(2.).child(el().w(44. * done as f32 / total.max(1) as f32).h_full().radius(2.).fill(gray(166))))
                    .child(muted(format!("{done} of {total} main tasks complete")).truncate().shrink())))
                .child(muted(if s.finished {"Finished"} else {"Open"})).child(muted("›"))
                .on_press(move |a: &mut App| { a.sprint.detail = Some(id); a.sprint.confirm_finish = false; }));
        }
        if count == 0 { body = body.child(muted("No sprints yet. Choose New sprint to create one.")); }
    }
    col().w_full().grow(1.).h(0.).scroll(ScrollSpec {stick_bottom:false}).child(body).children(app.error.as_ref().map(|e| muted(e).wrap().padding(12.)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use neochat::protocol::Sprint;
    use std::sync::mpsc;

    fn click(rt: &mut rune::Runtime<App>, key: &str) {
        for _ in 0..5 { rt.frame(1./60.); }
        let (x,y) = rt.find(key).unwrap_or_else(|| panic!("missing {key}"));
        rt.pointer_move(x,y); rt.pointer_down(x,y); rt.pointer_up(x,y);
        rt.frame(1./60.);
    }

    fn type_in(rt: &mut rune::Runtime<App>, key: &'static str, value: &str) {
        rt.frame(1./60.);
        rt.update(|_,tx| tx.focus(key,false)); rt.frame(1./60.);
        rt.key(rune::KeyEvent { key: Key::Text(value.into()), mods: Default::default() });
    }

    #[test]
    fn task_chooser_filters_dependencies_and_escape_keeps_the_draft() {
        let mut app = App::new(); fixture::load(&mut app,true);
        let mut rt = rune::Runtime::new(app,view::root,(1252.,900.),rune::Env::new(true,1.));
        click(&mut rt,"new-task"); type_in(&mut rt,"task-title","Do not lose this");
        click(&mut rt,"pick-needs");
        assert!(rt.find("task-title").is_some(), "the menu floats over the form");
        type_in(&mut rt,"task-picker-search","RECONNECT");
        rt.frame(1./60.);
        assert!(rt.find("needs-3").is_none());
        click(&mut rt,"needs-2");
        assert_eq!(rt.state.details_picker,Some("pick-needs"));
        rt.key(rune::KeyEvent {key:Key::Escape,mods:Default::default()}); rt.frame(1./60.);
        assert!(rt.state.dialog == Some(Dialog::NewTask));
        assert!(rt.state.details_picker.is_none());
        click(&mut rt,"pick-parent"); assert!(rt.state.task_search.text.is_empty());
        click(&mut rt,"pick-parent"); assert!(rt.state.details_picker.is_none(), "the field closes its own menu");
        assert!(rt.find("task-global-toggle").is_none());
        click(&mut rt,"task-advanced"); click(&mut rt,"task-global-toggle");
        assert!(rt.state.task_global && rt.find("pick-channel").is_none());
        click(&mut rt,"task-global-toggle");
        assert_eq!(rt.state.needs.text,"2"); assert_eq!(rt.state.task_title.text,"Do not lose this");
    }

    #[test]
    fn sprint_created_from_add_task_is_used_for_atomic_post() {
        let mut app = App::new(); fixture::load(&mut app,false);
        let (commands,rx) = mpsc::sync_channel(8); app.commands = Some(commands);
        let (updates,inbox) = mpsc::sync_channel(8); app.updates = Some(inbox);
        let mut rt = rune::Runtime::new(app,view::root,(1252.,674.),rune::Env::new(true,1.));
        click(&mut rt,"new-task"); type_in(&mut rt,"task-title","Keep this draft");
        click(&mut rt,"pick-sprint"); click(&mut rt,"new-sprint"); type_in(&mut rt,"sprint-title","Beta");
        type_in(&mut rt,"sprint-description","Prepare preview"); click(&mut rt,"submit-sprint");
        assert!(matches!(rx.try_recv(),Ok(Command::Action {action: Action::CreateSprint {board,title,description},..}) if board==rt.state.project && title=="Beta" && description=="Prepare preview"));
        updates.send(Update::SprintCreated(19)).unwrap(); updates.send(Update::Done(None,Ok(()))).unwrap();
        rt.update(|a,tx| {poll(a,0.,tx);});
        assert_eq!(rt.state.details_picker,None); assert_eq!(rt.state.sprint.assignment,Some(19));
        click(&mut rt,"create-task");
        assert!(matches!(rx.try_recv(),Ok(Command::Action {action: Action::PostSprintTask {sprint:19,text,..},..}) if text=="Keep this draft"));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn contextual_header_creates_sprint_then_posts_task_in_it() {
        let mut app = App::new(); fixture::load(&mut app,true);
        let (commands,rx) = mpsc::sync_channel(8); app.commands = Some(commands);
        let mut rt = rune::Runtime::new(app,view::root,(1252.,800.),rune::Env::new(true,1.));
        click(&mut rt,"filters"); click(&mut rt,"sprints-tab");
        assert!(rt.find("new-task").is_none() && rt.find("filters").is_some(), "the filter panel is the way back to Tasks");
        click(&mut rt,"new-sprint-action"); click(&mut rt,"sprint-dismiss");
        assert!(rt.state.dialog.is_none() && rx.try_recv().is_err());
        click(&mut rt,"new-sprint-action"); type_in(&mut rt,"sprint-title","Beta"); click(&mut rt,"submit-sprint");
        assert!(matches!(rx.try_recv(),Ok(Command::Action {action: Action::CreateSprint {title,..},..}) if title=="Beta"));
        rt.update(|a,_| {
            a.organization.sprints.insert(19,Sprint { id:19, board:a.project.clone(), title:"Beta".into(), description:String::new(), finished:false });
            // Two main tasks (one done), plus a reply that must not count toward progress.
            for id in [1,7,100] { a.organization.task_sprints.insert(id,19); }
            a.sprint_created(19); a.pending=false;
        });
        click(&mut rt,"sprint-back");
        assert!(rt.elements().iter().any(|e| !e.exiting && e.text.as_deref()==Some("1 of 2 main tasks complete")));
        click(&mut rt,"sprint-19"); click(&mut rt,"new-task");
        assert_eq!(rt.state.sprint.assignment,Some(19));
        type_in(&mut rt,"task-title","Release notes"); click(&mut rt,"create-task");
        assert!(matches!(rx.try_recv(),Ok(Command::Action {action: Action::PostSprintTask {sprint:19,..},..})));
        rt.update(|a,_| { a.dialog=None; a.pending=false; a.organization.sprints.get_mut(&19).unwrap().finished=true; });
        for _ in 0..5 { rt.frame(1./60.); }
        assert!(rt.find("new-task").is_none() && rt.find("new-sprint-action").is_none());
        click(&mut rt,"filters"); click(&mut rt,"tasks-tab");
        assert!(rt.find("new-task").is_some() && rt.find("filters").is_some());
    }

    #[test]
    fn sprint_navigation_membership_and_finish_confirmation() {
        let mut app = App::new(); fixture::load(&mut app,false);
        app.organization.sprints.insert(19,Sprint { id:19,board:app.project.clone(),title:"Beta".into(),description:"Prepare preview".into(),finished:false });
        app.organization.task_sprints.insert(1,19);
        let (commands,rx) = mpsc::sync_channel(8); app.commands = Some(commands);
        let mut rt = rune::Runtime::new(app,view::root,(1252.,800.),rune::Env::new(true,1.));
        click(&mut rt,"filters"); click(&mut rt,"sprints-tab"); click(&mut rt,"sprint-19"); click(&mut rt,"sprint-thread-1");
        assert_eq!(rt.state.thread,Some(1)); click(&mut rt,"sprint-back"); assert_eq!(rt.state.thread,None);
        click(&mut rt,"sprint-19"); click(&mut rt,"finish-sprint"); assert!(rx.try_recv().is_err());
        click(&mut rt,"finish-anyway"); assert!(matches!(rx.try_recv(),Ok(Command::Action {action: Action::FinishSprint {id:19,allow_unfinished:true},..})));
        rt.state.pending=false; click(&mut rt,"filters"); click(&mut rt,"tasks-tab"); rt.state.open_task_editor(1);
        click(&mut rt,"pick-sprint"); click(&mut rt,"assign-sprint-0");
        assert!(matches!(rx.try_recv(),Ok(Command::Action {action: Action::SetTaskSprint {id:1,sprint:None},..})));
        rt.state.pending=false; click(&mut rt,"dialog-close"); click(&mut rt,"new-task"); click(&mut rt,"pick-sprint"); click(&mut rt,"assign-sprint-19");
        assert_eq!(rt.state.sprint.assignment,Some(19)); assert!(rx.try_recv().is_err());
    }
}