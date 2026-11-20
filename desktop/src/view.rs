use super::*;
#[path = "task_form.rs"]
mod task_form;
#[path = "sprints.rs"]
pub(crate) mod sprints;
use rune::{Color, Cx, El, ScrollSpec, TextStyle, col, el, row, text, text_input};

pub(super) const TITLEBAR_H: f32 = 30.0;

fn gray(v: u8) -> Color {
    let s = v as f32 / 255.0;
    let c = if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) };
    Color { r: c, g: c, b: c, a: 1.0 }
}
fn font() -> TextStyle { TextStyle::new(12.0, 400.0) }
fn label(value: impl Into<String>) -> El { text(value.into()).font(font()).color(gray(210)) }
fn muted(value: impl Into<String>) -> El { label(value).color(gray(144)) }
fn marked_sender(m: &Message, text: El) -> El {
    match framework::mark(m.sender.as_deref()) {
        Some(mark) => row().gap(4.).items_center().shrink().child(text.shrink()).child(mark),
        None => text,
    }
}
fn line() -> El { el().w_full().h(1.0).fill(gray(35)) }
fn spacer() -> El { el().grow(1.0) }
fn menu_action(title:&str,glyph:&str,danger:bool)->El {
    let ink=if danger{rune::oklch(0.77,0.12,24.)}else{gray(215)};
    row().w_full().h(28.).px(8.).items_center().gap(8.).radius(4.).hover(gray(39))
        .child(label(glyph).w(16.).color(if danger{ink}else{gray(150)})).child(label(title).color(ink))
}
fn button(value: impl Into<String>, action: impl Fn(&mut App) + 'static) -> El {
    el().px(8.0).h(28.0).center().child(text(value.into()).font(font()).color(gray(210))).on_press(action)
}
fn age(app: &App, m: &Message) -> String {
    let secs = app.now.saturating_sub(m.created_at);
    if secs < 60 { "now".into() } else if secs < 3600 { format!("{}m", secs / 60) }
    else if secs < 86400 { format!("{}h", secs / 3600) } else { format!("{}d", secs / 86400) }
}
fn chip(value: String) -> El {
    el().px(5.0).h(18.0).center_y().border(1.0, gray(38)).fill(gray(0)).child(muted(value).font(TextStyle::new(10.0, 400.0)))
}
fn metadata_chip(value: String) -> El {
    el().px(6.).h(18.).shrink().center_y().border(1., gray(61)).fill(gray(28))
        .child(label(value).font(TextStyle::new(10.,400.)).color(gray(164)).truncate())
}
fn nav(value: String, count: Option<usize>, active: bool, page: Page) -> El {
    static HOME:rune::Icon=rune::Icon{name:"Home",view:24.,body:r#"<path d="m3 10 9-7 9 7v10h-7v-6h-4v6H3z" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linejoin="round"/>"#};
    static ACTIVITY:rune::Icon=rune::Icon{name:"Activity",view:24.,body:r#"<path d="M2 12h5l3-8 4 16 3-8h5" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/>"#};
    static TASKS:rune::Icon=rune::Icon{name:"Tasks",view:24.,body:r#"<path d="m3 7 2 2 4-5m3 3h9M3 16l2 2 4-5m3 3h9" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/>"#};
    let ink=gray(if active {230}else{140});
    let mut item = row().w_full().h(30.0).px(8.0).items_center().fill(gray(if active { 18 } else { 0 }));
    if let Some(icon)=match &page {Page::Home=>Some(&HOME),Page::Activity=>Some(&ACTIVITY),Page::Tasks=>Some(&TASKS),_=>None} {
        item=item.gap(8.).child(rune::icon(icon,15.).color(ink));
    }
    item=item.child(label(value).color(ink).truncate())
        .on_press(move |a: &mut App| a.navigate(page.clone()));
    if let Some(count) = count.filter(|n| *n > 0) { item = item.child(spacer()).child(muted(count.to_string())); }
    item
}
fn board_icon(remote:bool, size:f32)->El {
    static FOLDER:rune::Icon=rune::Icon{name:"Local board",view:24.,body:r#"<path d="M3 7V5h7l2 3h9v12H3z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/>"#};
    static CLOUD:rune::Icon=rune::Icon{name:"Remote board",view:24.,body:r#"<path d="M7 19a5 5 0 0 1-1-9.9 7 7 0 0 1 13.1-1A5.5 5.5 0 0 1 18 19Z" fill="none" stroke="currentColor" stroke-width="1.8"/>"#};
    rune::icon(if remote{&CLOUD}else{&FOLDER},size).color(gray(170))
}
fn board_picker(app: &App, scale: f32) -> El {
    let name = if app.project.is_empty() { "Boards" } else { &app.project };
    let font = TextStyle::new(12. * scale, 600.);
    row().key("projects-toggle").max_w(170. * scale).h(26. * scale).px(8. * scale).gap(8. * scale)
        .items_center().radius(5.).hover_alone().hover(gray(34))
        .children((!app.project.is_empty()).then(||board_icon(app.remote(),15.*scale)))
        .child(label(name).font(font.clone()).color(splash::wordmark_ink()).truncate().shrink())
        .child(muted("▾").font(font))
        .on_press(|a: &mut App| a.project_menu = !a.project_menu)
}

fn divider(slot:usize,value:f32,min:f32,max:f32)->El {
    let vertical=slot==2;
    let bar=if vertical {el().w_full().h(7.).center().child(line()).cursor(rune::Cursor::ResizeRow)}else{el().w(7.).h_full().center().child(el().w(1.).h_full().fill(gray(35))).cursor(rune::Cursor::ResizeColumn)};
    bar.key(format!("pane-divider-{slot}")).hover(gray(46)).on_drag(move|a:&mut App,d:&mut rune::Drag,tx:&mut rune::Tx|{
        tx.instant(); match d.phase {rune::Phase::Down=>d.data=value,rune::Phase::Move=>{let delta=match slot{0=>d.dx,1=>-d.dx,_=>d.dy};a.pane_sizes[slot]=(d.data+delta/a.zoom.factor()).clamp(min,max).round();},rune::Phase::Up=>if d.cancelled(){a.pane_sizes[slot]=d.data;}}
    })
}
fn sidebar(app: &App, width:f32) -> El {
    let mut side = col().key("sidebar").w(width).h_full().fill(gray(0));
    side = side.child(row().w_full().h(45.0).px(16.0).gap(3.).items_center()
        .child(board_picker(app, 1.).shrink())
        .child(spacer())
        .child(button("‹", |a| {a.sidebar = false;a.project_menu = false;a.save_preferences();}).key("collapse-sidebar"))).child(line());
    let mut channels = col().w_full().padding(8.0)
        .child(nav("Home".into(), None, app.page == Page::Home, Page::Home))
        .child(nav("Activity".into(), None, app.page == Page::Activity, Page::Activity))
        .child(nav("Tasks".into(), Some(app.snapshot.messages.iter().filter(|m| projects::contains(&app.project,&m.channel) && m.parent.is_none() && app.organization.tasks.get(&m.id).is_none_or(|p|p.parent.is_none()) && m.task.as_ref().is_some_and(|t| matches_filter(&t.state, OPEN_TASKS))).count()), app.page == Page::Tasks, Page::Tasks))
        .child(row().h(46.0).px(8.0).items_center().child(muted("channels").grow(1.0)).child(el().key("add-channel").size(28.,28.).center().radius(7.).hover(gray(40)).child(label("+")).on_press_tx(|a:&mut App,tx| {a.error=None;a.new_channel=edit("");a.dialog=Some(Dialog::NewChannel);tx.focus("new-channel",false);} )));
    let mut names = app.snapshot.channels.clone();
    names.sort_by_key(|c| (c.contains('/'), c.clone()));
    for channel in names {
        if !projects::contains(&app.project,&channel) {continue;}
        let leaf=channel.split_once('/').map_or(channel.as_str(),|(_,path)|path);
        let unread = app.snapshot.messages.iter().filter(|m| m.channel == channel && m.id > *app.seen.get(&channel).unwrap_or(&0)).count();
        let item = nav(format!("#{leaf}"), Some(unread), app.page == Page::Channel(channel.clone()), Page::Channel(channel.clone()));
        channels = channels.child(row().w_full().items_center().child(item.w(0.).grow(1.))
            .child(el().key(format!("channel-options-{channel}")).size(26.,28.).center().radius(5.).hover(gray(40)).child(muted("⋯"))
                .on_press(move |a:&mut App| a.channel_actions.menu = Some(channel.clone()))));
    }
    side.child(col().grow(1.0).w_full().scroll(ScrollSpec { stick_bottom: false }).child(channels))
        .child(row().key("local-profile").w_full().h(40.).px(16.).gap(8.).items_center()
            .child(app.profiles.view(&app.local_name, 24., app.fixture))
            .child(label(app.me.as_ref().and_then(|m|m.display_name.as_deref()).unwrap_or(&app.local_name)).grow(1.).w(0.).truncate())
            .child(button("⚙", |a| a.dialog = Some(Dialog::Settings)).key("profile-settings").size(30.,30.).center().radius(7.).hover(gray(40))))
}
fn header(app: &App) -> El {
    let title = match &app.page { Page::Home => "Home".into(), Page::Activity => "Activity".into(), Page::Tasks => "Tasks".into(), Page::Scheduled => "scheduled".into(), Page::Channel(c) => format!("#{c}") };
    let mut header = row().key("header").w_full().h(45.0).px(16.0).gap(6.0).items_center();
    if !app.sidebar || (app.width < 900.0 && app.thread.is_some()) {
        header = header.child(button("›", |a| { a.sidebar = true; a.save_preferences(); if a.width < 900.0 { a.thread = None; } }));
    }
    header = if app.page == Page::Tasks { sprints::tabs(app,header) } else { header.child(label(title).color(gray(232))) };
    if app.page==Page::Home {return header;}
    header = header.child(spacer());
    if app.page == Page::Tasks && app.sprint.show && sprints::current(app).is_none() {
        header = header.child(button("+ New sprint", sprints::new_sprint).key("new-sprint-action").radius(4.).hover(gray(24)));
    } else if app.page == Page::Tasks && !sprints::current(app).is_some_and(|s| s.finished) {
        header = header.child(el().key("new-task").px(10.).h(28.).center().radius(4.).hover(gray(24))
            .child(label("+ New task"))
            .on_press_tx(|a: &mut App, tx| {
                if !a.creating_task { a.error = None; a.details_picker = None; a.pick_assignee = false; }
                if !a.creating_task && (a.editing_task.is_some() || a.task_parent.is_some()) { a.clear_task_draft(); }
                if !a.creating_task && let Some(sprint) = sprints::current(a) {
                    a.sprint.assignment = Some(sprint.id);
                    a.task_global = false;
                }
                if a.task_channel.is_empty() { a.task_channel = a.channel.text.clone(); }
                a.dialog = Some(Dialog::NewTask);
                if !a.creating_task { tx.focus("task-title", false); }
            }));
    }
    if app.page == Page::Tasks && app.sprint.show { return header; }
    static FILTER: rune::Icon = rune::Icon{name:"Filters",view:24.,body:r#"<path d="M3 5h18M6 12h12M10 19h4" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>"#};
    header.child(el().key("filters").size(32.,32.).center().radius(8.).fill(gray(if !app.query.text.is_empty() || app.page==Page::Tasks && (app.task_filters.active() || app.filter != ALL_TASKS){40}else{0})).hover(gray(40)).child(rune::icon(&FILTER,18.).color(gray(189))).on_press_tx(|a:&mut App,tx|{a.details_picker=None;a.dialog=Some(Dialog::Filters);tx.focus("filter",false);} ))
}
fn task_status(state:TaskState)->El {
    static DONE:rune::Icon=rune::Icon{name:"Completed",view:24.,body:r#"<g fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" stroke-linecap="round"><circle cx="12" cy="12" r="9.5"/><path d="m8 12 2.7 2.7 5.3-5.4"/></g>"#};
    if state==TaskState::Completed{return row().items_center().gap(5.).child(rune::icon(&DONE,14.).color(gray(227))).child(label("completed"));}
    let icon=match state {TaskState::InProgress=>"▶",TaskState::Claimed=>"⦿",TaskState::Completed=>"☑",TaskState::Waiting=>"◌",TaskState::Failed=>"⊗",TaskState::Blocked|TaskState::Canceled=>"⊘",TaskState::Ready=>"○"};
    row().items_center().gap(5.).child(label(icon)).child(label(state_name(&state)))
}
fn lease(app:&App,expires:u64)->String {let s=expires.saturating_sub(app.now);format!("{}:{:02} left",s/60,s%60)}
fn active_lease(task: &neochat::protocol::Task, now: u64) -> Option<(&str, u64)> {
    Some((task.owner.as_deref()?, task.expires_at.filter(|end| *end > now)?))
}
fn lease_label(app: &App, task: &neochat::protocol::Task) -> String {
    active_lease(task, app.now).map_or_else(|| "Unowned".into(), |(actor, end)| format!("{} · {}", app.actor_label(actor), lease(app, end)))
}
fn thread_options(app:&App,id:u64,row:El)->El {
    let mut slot=el().size(23.,23.).center();
    if app.hover_row==Some(id)||app.row_menu==Some(id){slot=slot.child(el().key(format!("thread-options-{id}")).size(23.,23.).center().radius(5.).fill(gray(27)).hover(gray(45)).child(label("⋯"))
        .on_layout(move|a:&mut App,r|if a.row_menu.is_none(){a.row_anchor=r;}).on_press(move|a:&mut App|a.row_menu=Some(id)));}
    row.child(slot).on_hover_change(move|a:&mut App,inside,_|{if inside{a.hover_row=Some(id);}else if a.hover_row==Some(id){a.hover_row=None;}})
}
fn task_row(app: &App, m: &Message) -> El {
    let task = m.task.as_ref().unwrap();
    let id = m.id;
    let split=app.thread.is_some();let mut title=row().w_full().items_center().gap(6.).clip_soft(0.)
        .child(muted(format!("#{id}"))).child(label(m.text.lines().next().unwrap_or("")).truncate().shrink());
    for(k,v)in m.labels.iter().filter(|(k,_)| k.as_str() != "area").take(if split{2}else{4}){title=title.child(chip(format!("{k}={v}")));}
    let mut body=col().grow(1.).w(0.).gap(3.).child(title).children(m.sender.as_ref().map(|_|marked_sender(m, muted(app.sender_label(m)).truncate())));
    if app.organization.tasks.get(&m.id).is_some_and(|p| p.global) { body = body.child(muted("Assigned to Everyone")); }
    else if matches!(task.state, TaskState::Claimed | TaskState::InProgress) {body=body.child(muted(format!("Lease: {}",lease_label(app,task))).truncate());}
    else if let Some(actor)=&task.assignee {body=body.child(muted(format!("Assigned to {}",app.actor_label(actor))).truncate());}
    else if !task.needs.is_empty(){body=body.child(muted(format!("needs {}",task.needs.iter().map(|id|format!("#{id}")).collect::<Vec<_>>().join(", "))).truncate());}
    let mut r=row().key(format!("task-{id}")).w_full().min_h(if app.compact{30.}else{42.}).px(16.).py(5.).gap(7.).items_center().fill(gray(if app.thread==Some(id){18}else{0})).hover(gray(20)).child(task_status(task.state).w(92.)).child(body);
    if !split{r=r.child(muted(format!("#{}",m.channel)).truncate());}
    thread_options(app,id,r.child(muted(age(app,m)).w(24.)).on_press(move|a:&mut App|a.open_thread(id)))
}
fn message(app: &App, m: &Message, in_thread: bool) -> El {
    let id = m.id;
    let mut body = col().grow(1.0).w(0.0).gap(5.0);
    let byline = row().w_full().gap(8.0).items_center().child(marked_sender(m, muted(app.sender_label(m)).truncate().shrink()))
            .child(muted(if in_thread { String::new() } else { format!("{} #{}", if m.parent.is_some() { "replied in" } else { "in" }, m.channel) }))
            .child(muted(age(app, m)).font(TextStyle::new(10.0, 400.0)));
    if let Some(task) = &m.task {
        let (title, description) = m.text.split_once('\n').unwrap_or((&m.text, ""));
        body = body.child(label(title).selectable(true).font(TextStyle::new(17., 650.)).color(gray(240)).wrap().w_full())
            .child(row().w_full().flex_wrap().gap(8.).items_center().child(metadata_chip(state_name(&task.state).into())).child(byline));
        if !description.trim().is_empty() {
            body = body.child(label(description.trim()).selectable(true).wrap().w_full());
        }
    } else {
        body = body.child(byline).child(label(&m.text).selectable(true).wrap().w_full());
    }
    if !m.labels.is_empty() {
        body = body.child(row().w_full().gap(4.).flex_wrap().children(m.labels.iter().filter(|(k,_)| m.task.is_none() || k.as_str() != "area").map(|(k,v)| {
            metadata_chip(format!("{k}={v}")).key(format!("message-label-{id}-{k}"))
        })));
    }
    let mut counts = BTreeMap::<String, usize>::new();
    for r in app.reactions.iter().filter(|r| r.id == id) { *counts.entry(r.reaction.clone()).or_default() += 1; }
    if !counts.is_empty() {
        body = body.child(row().gap(4.0).children(counts.iter().map(|(r,n)| chip(if r == "+1" {
            format!("{n} {}", if *n == 1 { "like" } else { "likes" })
        } else { format!("{r} {n}") }))));
    }
    if in_thread {
        let files = if m.attachments.is_empty() { app.attachments.get(&id).map_or(&[][..], Vec::as_slice) } else { &m.attachments };
        if !files.is_empty() {
            body = body.child(row().w_full().gap(4.).flex_wrap().children(files.iter().map(|file| {
                let file_id = file.id;
                metadata_chip(file.name.clone()).key(format!("message-file-{file_id}")).hover(gray(44))
                    .on_press(move |a: &mut App| { a.files(id); a.download_id = Some(file_id); a.download_path = edit(""); })
            })));
        }
    }
    if !in_thread && m.parent.is_none() {
        let (count,last)=app.snapshot.messages.iter().filter(|r|r.parent==Some(id)).fold((0usize,None::<&Message>),|(n,last),r| {
            (n+1,Some(last.filter(|p|(p.created_at,p.id)>(r.created_at,r.id)).unwrap_or(r)))
        });
        if let Some(last)=last {
            body=body.child(row().w_full().child(row().key(format!("open-replies-{id}")).h(27.).px(8.).gap(8.).items_center().shrink()
                .radius(5.).fill(gray(26)).border(1.,gray(48)).hover(gray(39))
                .child(marked_sender(last, label(format!("{} replied",app.sender_label(last))).key(format!("reply-by-{id}")).font(TextStyle::new(12.,600.)).truncate().shrink()))
                .children((count > 1).then(|| muted(format!("{count} replies"))))
                .on_press(move |a:&mut App|a.open_thread(id))));
        }
    }
    let r=row().key(format!("{}-{id}", if in_thread { "reply" } else { "message" })).w_full().padding(12.0).gap(14.0)
        .child(app.profiles.view(profile::display_name(&m.author), 28., app.fixture))
        .child(body).on_press(move |a: &mut App| a.open_thread(id));
    if !in_thread && m.parent.is_none(){thread_options(app,id,r)}else{r}
}
fn form_input(value:&TextEdit, key:impl Into<String>, hint:&str)->El {
    text_input(value,font()).key(key.into()).w_full().h(36.).px(12.).py(10.).single_line()
        .radius(7.).fill(gray(0)).border(1.,gray(72)).color(gray(235)).placeholder(hint,gray(148))
}
fn input(app: &App, which: &'static str, hint: &'static str) -> El {
    let value = match which {
        "schedule-at" => &app.schedule_at,
        "upload-path" => &app.upload_path, _ => &app.download_path,
    };
    form_input(value,which,hint).on_edit(move |a: &mut App| match which {
            "schedule-at" => &mut a.schedule_at,
            "upload-path" => &mut a.upload_path, _ => &mut a.download_path,
        })
}
fn attachment_panel(app: &App, id: u64) -> El {
    let mut panel = col().key("attachments").w_full().padding(12.0).gap(6.0).border(1.0, gray(35))
        .child(row().child(label(format!("attachments · #{id}"))).child(spacer())
            .child(button("refresh", move |a| a.files(id)))
            .child(button("×", |a| a.attachment_message = None)))
        .child(input(app, "upload-path", "Upload: full local file path"))
        .child(button(if app.pending { "working…" } else { "upload file (≤8 MiB)" }, move |a| {
            let path = a.upload_path.text.as_str();
            if path.trim().is_empty() { a.error = Some("Enter the local file path".into()); return; }
            a.queue_file(Command::Upload { message: id, path: path.into() }, true);
        }));
    #[cfg(feature = "native-picker")]
    { panel = panel.child(el().key("browse-upload").h(28.0).px(8.0).center_y().child(label("browse local files…"))
        .on_press_tx(|_: &mut App,tx| tx.open_file(rune::FileDialogOptions { title:"Choose attachment to upload".into(),..Default::default() },
            |a: &mut App,result,_| a.picked_path(result,true)))); }
    let files = app.attachments.get(&id).map(Vec::as_slice).or_else(|| app.snapshot.messages.iter().find(|m| m.id == id && !m.attachments.is_empty()).map(|m| m.attachments.as_slice()));
    if let Some(files) = files {
        if files.is_empty() { panel = panel.child(muted("No attachments")); }
        for file in files {
            let file_id = file.id;
            panel = panel.child(row().w_full().items_center()
                .child(label(format!("{} · {} B", file.name, file.size)).truncate().shrink()).child(spacer())
                .child(button("save as…", move |a| { a.download_id = Some(file_id); a.download_path = edit(""); })));
        }
    } else { panel = panel.child(muted("Refresh to load attachments")); }
    if let Some(file_id) = app.download_id.filter(|selected| files.is_some_and(|fs| fs.iter().any(|f| f.id == *selected))) {
        panel = panel.child(muted(format!("Save attachment #{file_id} · existing files are never overwritten")).wrap())
            .child(input(app, "download-path", "Save as: full destination file path"))
            .child(button("download to path", move |a| {
                let path = a.download_path.text.as_str();
                if path.trim().is_empty() { a.error = Some("Enter a destination file path".into()); return; }
                a.queue_file(Command::Download { id: file_id, path: path.into() }, true);
            }));
        #[cfg(feature = "native-picker")]
        { panel = panel.child(el().key("browse-download").h(28.0).px(8.0).center_y().child(label("choose destination…"))
            .on_press_tx(|_: &mut App,tx| tx.save_file(rune::FileDialogOptions { title:"Save attachment to a new file".into(),file_name:Some("attachment.bin".into()),..Default::default() },
                |a: &mut App,result,_| a.picked_path(result,false)))); }
    }
    panel
}
fn field(app: &App, which: &'static str, hint: &'static str) -> El {
    let value = match which { "labels" => &app.labels, "needs" => &app.needs, "assignee" => &app.assignee, _ => &app.channel };
    form_input(value,format!("composer-{which}"),hint).grow(1.).w(0.)
        .on_edit(move |a: &mut App| match which { "labels" => &mut a.labels, "needs" => &mut a.needs, "assignee" => &mut a.assignee, _ => &mut a.channel })
}
fn composer(app: &App, reply: bool) -> El {
    use rune::{icon, Icon};
    static CLIP: Icon = Icon { name:"Attach file", view:24., body:r#"<path d="m21 11-9.2 9.2a6 6 0 0 1-8.5-8.5l10-10a4 4 0 0 1 5.7 5.7l-10 10a2 2 0 0 1-2.8-2.8l9.2-9.2" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>"# };
    static TAG: Icon = Icon { name:"Labels", view:24., body:r#"<path d="M3 3h8l10 10-8 8L3 11z" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linejoin="round"/><circle cx="7.5" cy="7.5" r="1.2"/>"# };
    static CLOCK: Icon = Icon { name:"Schedule post", view:24., body:r#"<g fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"><circle cx="12" cy="12" r="9.5"/><path d="M12 5.7v6.6l4.3 2.5"/></g>"# };
    let tool = |glyph: &'static Icon, active: bool| el().key(glyph.name).size(30.,30.).radius(7.).fill(gray(if active {46} else {0})).hover(gray(44)).center()
        .child(icon(glyph,17.).color(gray(if active {245} else {181})));
    let draft = if reply { &app.reply } else { &app.draft };
    let mut card = col().key(if reply { "reply-composer" } else { "composer" }).w_full().padding(9.0).gap(8.0)
        .radius(12.).fill(gray(0)).border(1.0, gray(48));
    card = card.child(text_input(draft, font()).key(if reply { "reply-input" } else { "post-input" }).w_full().h(if reply { 46.0 } else { 54.0 })
        .color(gray(220)).placeholder(if reply { "reply to thread…" } else { "start a thread…" }, gray(100))
        .on_edit(move |a: &mut App| if reply { &mut a.reply } else { &mut a.draft })
        .on_input_key(move |a: &mut App, e, _| {
            if e.key == Key::Enter && !e.mods.shift { a.submit(reply); true } else { false }
        }));
    let mut tools = row().w_full().items_center().gap(3.0);
    let owner = if reply { app.thread } else { None };
    let attached = app.compose_file_open && app.compose_file_owner == owner;
    tools=tools.child(tool(&CLIP,attached).on_press_tx(move |_:&mut App,tx| tx.open_file(rune::FileDialogOptions{title:"Attach a photo or file".into(),..Default::default()},move |a:&mut App,result,_| match result {
        Ok(Some(path))=>{ if let Some(path)=path.to_str(){a.compose_file=edit(path);a.compose_file_open=true;a.compose_file_owner=owner;}else{a.error=Some("Filename is not valid UTF-8".into());} },
        Ok(None)=>{}, Err(e)=>a.error=Some(format!("Could not open file chooser: {e}")),
    })));
    let tagged = if reply { owner.and_then(|id|app.reply_labels.get(&id)).is_some_and(|l|!l.text.is_empty()) } else { !app.labels.text.is_empty() };
    tools = tools.child(tool(&TAG,tagged).key(if reply {"reply-tags"} else {"post-tags"}).on_press(move |a:&mut App| {
        a.details_picker=None; a.pick_assignee=false;
        a.dialog=if reply { owner.map(|id|{a.reply_labels.entry(id).or_insert_with(||edit(""));Dialog::ReplyTags(id)}) } else {Some(Dialog::Tags)};
    }));
    if !reply { tools=tools.child(tool(&CLOCK,app.show_schedule).on_press(|a:&mut App| a.dialog=Some(Dialog::Schedule))); }
    if attached {
        let filename=std::path::Path::new(&app.compose_file.text).file_name().unwrap_or_default().to_string_lossy();
        tools=tools.child(el().h(26.).shrink().px(7.).radius(6.).fill(gray(41)).center().child(label(format!("{filename} ×")).truncate()).on_press(|a:&mut App| {a.compose_file_open=false;a.compose_file=edit("");}));
    }
    card = card.child(tools
        .child(spacer())
        .child(el().h(32.0).px(12.0).radius(7.).center().fill(gray(if app.pending { 50 } else { 235 }))
            .child(label("Send").color(gray(8)))
            .on_press(move |a: &mut App| a.submit(reply))));
    col().w_full().padding(16.).child(card)
}

fn modal_frame(key: &str, viewport: [f32; 2], width: f32, header: Option<El>, body: El, footer: Option<El>, dismiss: impl Fn(&mut App) + 'static) -> El {
    let [w, h] = viewport;
    let panel = col().selection_group(true).key(key.to_owned()).w(width.min((w - 24.).max(1.))).max_h((h - 24.).max(1.))
        .fill(gray(0)).border(1., gray(48)).radius(12.).clip_soft(12.)
        .children(header)
        .child(col().key("modal-body").w_full().min_h(0.).shrink().scroll(ScrollSpec { stick_bottom: false }).child(body))
        .children(footer)
        .modal(true, dismiss);
    el().absolute(0.,0.).w_full().h_full().center().fill(gray(0).alpha(0.64)).child(panel)
}

fn dialog_action(button: El) -> El {
    button.radius(7.).fill(gray(0)).border(1., gray(55)).hover(gray(32))
}

fn dialog(app:&App, page:Dialog, viewport: [f32; 2], filter_anchor: Option<[f32;4]>)->El {
    use Dialog::*;
    if page == NewTask && app.details_picker == Some("new-sprint") { return sprints::create_dialog(app, viewport); }
    if page == NewTask {
        if let Some(key) = app.details_picker { return task_form::chooser(app, viewport, key); }
    }
    let title=match page {NewSprint=>return sprints::create_dialog(app,viewport),Settings=>"Settings",Personal=>"Personal",BoardView=>"Board View",Connection|Share=>"Board connection",Tags=>"Message details",NewTask if app.editing_task.is_some()=>"Edit task",NewTask if app.task_global=>"New global task",NewTask if app.task_parent.is_some()=>"New subtask",NewTask=>"New task",ReplyTags(_)=>"Reply labels",Filters=>"Filters",Schedule=>"Schedule delivery",NewChannel=>"New channel",DeleteThread(_)=>"Delete thread?",DeleteChannel=>"Delete channel?"};
    let nav=|name:&str, detail:&str, dest:Dialog| row().w_full().h(58.).px(14.).items_center().radius(10.).fill(gray(0)).border(1.,gray(55)).hover(gray(48))
        .child(col().gap(3.).child(label(name)).child(muted(detail).font(TextStyle::new(10.,400.)))).child(spacer()).child(muted("›")).on_press(move |a:&mut App| {if dest==Personal{a.personal_name=edit(a.me.as_ref().and_then(|m|m.display_name.as_deref()).unwrap_or(""));a.error=None;}if dest==Connection {a.open_share();}a.dialog=Some(dest);});
    let back = matches!(page, BoardView|Connection|Personal).then(|| button("‹ Back",|a|a.dialog=Some(Settings)).radius(7.).hover(gray(52)));
    let top=row().w_full().px(12.).h(40.).gap(8.).items_center().border_sides([0.,0.,1.,0.],gray(35)).children(back).child(label(title).font(TextStyle::new(15.,600.)).truncate().shrink()).child(spacer())
        .child(el().w(62.).h(30.).center().child(dialog_action(button(if page==Share{"Close"}else if matches!(page,NewChannel|NewTask|DeleteThread(_)|DeleteChannel){"Cancel"}else{"Done"},|a|a.dialog=None).key("dialog-close"))));
    let mut panel=col().w_full().padding(16.).gap(10.);
    let pref=|name:&str, value:&str, change:fn(&mut App)| row().w_full().h(44.).px(14.).items_center().hover(gray(49))
        .child(label(name)).child(spacer()).child(muted(value)).on_press(move |a:&mut App|{change(a);a.save_preferences();});
    panel=match page {
        NewSprint => unreachable!("rendered above"),
        DeleteChannel => {
            let channel = app.channel_actions.target.as_deref().unwrap_or("");
            let mut p = panel.child(label(format!("#{channel}")).wrap())
                .child(muted("Deleting this channel removes its threads, replies, tasks, attachments and scheduled posts for everyone. This cannot be undone.").wrap());
            if let Some(error) = &app.error { p = p.child(label(error).wrap()); }
            p.child(row().child(spacer()).child(menu_action(if app.pending {"Deleting…"} else {"Delete channel"}, "×", true)
                .w(160.).key("confirm-delete-channel").on_press(App::confirm_channel_delete)))
        }
        Connection | Share => {
            use connection::HostOp;
            use neochat::sharing::RouteState::*;
            let s = &app.share;
            let mut p = panel.child(label(&s.board));
            if app.remote() {
                p = p.child(label(match app.status.as_str() { "live" => "Joined", "connecting" => "Connecting…", _ => "Disconnected" }).key("guest-status"))
                    .child(muted("Disconnecting affects only this desktop. Your board stays saved and the host stays online.").wrap());
                if app.status == "disconnected" { p = p.child(dialog_action(button("Reconnect", App::reconnect_guest).key("reconnect-guest"))); }
                else { p = p.child(dialog_action(button("Disconnect this desktop", App::disconnect_guest).key("disconnect-guest"))); }
            } else if s.busy {
                p = p.child(muted(match s.operation { Some(HostOp::Create) => "Creating invite link…", Some(HostOp::Stop) => "Stopping host tunnel…", _ => "Checking host status…" })).child(el().key("share-progress").w(160.).h(5.).radius(2.).fill(gray(53)).child(el().absolute(112.*s.pulse,0.).w(48.).h(5.).radius(2.).fill(gray(200))));
            } else if app.status != "live" { p = p.child(muted(if app.status == "connecting" {"Connecting to local board…"} else {"Local board offline"})); }
            else if let Some(Ok(state)) = &s.result {
                p = p.child(label(match state.route { Connected => "Hosting", Stopped => "Local", Connecting => "Tunnel connecting…", Reconnecting => "Tunnel reconnecting…" }).key("host-status"));
                if s.confirm_stop {
                    p = p.child(muted("Stop the host-wide tunnel? Guests on every board shared by this host will lose remote access. Local boards stay available.").wrap())
                        .child(dialog_action(button("Stop host-wide tunnel", |a| a.host_request(HostOp::Stop)).key("confirm-stop-host")))
                        .child(dialog_action(button("Cancel", |a| a.share.confirm_stop = false)));
                } else {
                    if app.host_link().is_some() {
                        p = p.child(dialog_action(button(if s.copied {"Copied!"} else {"Copy invite link"}, |_| {}).key("copy-share-link").on_press_tx(|a: &mut App, tx| { if let Some(link) = a.host_link() { tx.copy(link.to_owned()); a.share.copied = true; } })));
                    } else if matches!(state.route, Connected | Stopped) {
                        p = p.child(dialog_action(button(if state.route == Connected { "Create and copy invite link" } else { "Start hosting and copy invite link" }, |a| a.host_request(HostOp::Create)).key("create-share-link")));
                    } else { p = p.child(dialog_action(button("Refresh status", |a| a.host_request(HostOp::Inspect)))); }
                    if state.route != Stopped { p = p.child(dialog_action(button("Disconnect host tunnel…", |a| a.share.confirm_stop = true).key("disconnect-host"))); }
                }
            } else {
                if let Some(Err(error)) = &s.result { p = p.child(muted(error).wrap()); }
                p = p.child(dialog_action(button("Retry status check", |a| a.host_request(HostOp::Inspect)).key("retry-host")));
            }
            p
        }
        DeleteThread(id)=>{let title=app.snapshot.messages.iter().find(|m|m.id==id).and_then(|m|m.text.lines().next()).unwrap_or("Thread");
            let mut p=panel.child(label(format!("#{id} · {title}")).truncate()).child(muted("This will remove the thread, its replies, and its attachments from the board. This cannot be undone.").wrap());
            if let Some(error)=&app.error {p=p.child(muted(error).wrap());}
            p.child(row().child(spacer()).child(button(if app.pending{"Deleting…"}else{"Delete thread"},move|a|a.dispatch(Action::DeleteThread{id},None)).key("confirm-delete").radius(7.).border(1.,gray(75))))},
        ReplyTags(id)=>{
            let empty=edit("");
            panel.child(muted("Labels")).child(form_input(app.reply_labels.get(&id).unwrap_or(&empty),"reply-labels","category=design")
                .on_edit(move |a:&mut App|a.reply_labels.entry(id).or_insert_with(||edit(""))))
                .child(muted("Labels apply only to this reply. Use key=value or key=\"multiple words\".").wrap())
        },
        NewChannel=>{
            let mut p=panel.child(muted("Channel name")).child(form_input(&app.new_channel,"new-channel","e.g. design or team/design").on_edit(|a:&mut App|&mut a.new_channel).on_submit(|a:&mut App,_|a.create_channel()));
            if let Some(error)=&app.error {p=p.child(muted(error).wrap());}
            p.child(row().w_full().child(spacer()).child(button(if app.creating_channel.is_some(){"Creating…"}else{"Create channel"},App::create_channel).key("create-channel").radius(7.).fill(gray(0)).border(1.,gray(55))))
        },
        Settings=>panel.child(nav("Personal","Your display name on this board",Personal)).child(nav("Board View","Task density, sort order and sidebar",BoardView)).child(dialog_action(button("Check for updates",|a|a.release.manual()).key("check-updates"))),
        Personal=>{let mut p=panel.child(muted("Display name")).child(form_input(&app.personal_name,"personal-name",&app.local_name).on_edit(|a:&mut App|&mut a.personal_name))
            .child(muted(format!("System name: {}. Leave blank to use it again.",app.local_name)).wrap());
            if let Some(e)=&app.error{p=p.child(muted(e).wrap());}
            p.child(row().child(spacer()).child(button("Save name",|a|{a.error=None;a.queue_file(Command::Personal(a.personal_name.text.trim().into()),false);})))},
        BoardView=>panel.child(col().w_full().fill(gray(0)).border(1.,gray(55)).radius(10.)
            .child(pref("Task spacing",if app.compact{"Compact"}else{"Comfortable"},|a|a.compact=!a.compact)).child(line())
            .child(pref("Activity and scheduled order",if app.oldest{"Oldest first"}else{"Newest first"},|a|a.oldest=!a.oldest)).child(line())
            .child(pref("Sidebar",if app.sidebar{"Visible"}else{"Hidden"},|a|a.sidebar=!a.sidebar)))
            .child(muted("Preferences are saved on this device.").font(TextStyle::new(11.,400.))),
        NewTask => task_form::properties(app, panel),
        Tags => {
            let current = projects::channel(&app.project,app.channel.text.trim().trim_start_matches('#'));
            let channel = current.split_once('/').map_or(current.as_str(),|(_,s)|s);
            let mut p = panel.child(muted("Channel")).child(select_trigger(format!("#{channel}"),app.details_picker==Some("pick-channel"),|a| a.details_picker = (a.details_picker!=Some("pick-channel")).then_some("pick-channel")).key("pick-channel"));
            if app.details_picker==Some("pick-channel") {
                let mut choices=col().w_full().max_h(116.).scroll(ScrollSpec{stick_bottom:false});
                for ch in app.snapshot.channels.iter().filter(|c|projects::contains(&app.project,c)) {
                    let name=ch.clone(); let short=ch.split_once('/').map_or(ch.as_str(),|(_,s)|s);
                    choices=choices.child(button(format!("#{short}"),move|a|{a.channel=edit(&name);a.details_picker=None;}));
                }
                if !app.snapshot.channels.iter().any(|c|projects::contains(&app.project,c)){choices=choices.child(muted("No channels yet. Use + in the sidebar."));}
                p=p.child(choices);
            }
            p.child(muted("Labels · optional")).child(row().w_full().child(field(app,"labels","category=design")))
        },
        Filters=>{
            let mut p=panel.child(muted("Search")).child(form_input(&app.query,"filter","Words or labels").on_edit(|a:&mut App|&mut a.query));
            let selector = |key: &'static str, title: &str, value: String| {
                row().w_full().items_center().child(muted(title).w(96.))
                    .child(select_trigger(value,app.details_picker==Some(key), move |a| {
                        a.details_picker = if a.details_picker == Some(key) { None } else { Some(key) };
                        a.filter_choice_search=edit("");
                    }).grow(1.).w(0.).key(key))
            };
            if app.page==Page::Tasks {
                let filters=&app.task_filters;
                p = p.child(selector("filter-status", "Status", status_name(app.filter)));
                if app.details_picker == Some("filter-status") {
                    p = p.child(col().w_full().max_h(156.).scroll(ScrollSpec { stick_bottom:false }).children(std::iter::once(ALL_TASKS).chain(0..ALL_TASKS).map(|i| {
                        choice_row(status_name(i),i==app.filter, move |a| {a.filter=i;a.details_picker=None;}).key(format!("task-status-{}",FILTERS[i]))
                    })));
                }
                let scoped=||app.snapshot.messages.iter().filter(|m|m.task.is_some() && projects::contains(&app.project,&m.channel) && filters.channel.as_ref().is_none_or(|c|*c==m.channel));
                for (key,title,value) in [("filter-channel","Channel",filters.channel.clone().unwrap_or_else(||"All channels".into())),("filter-assignee","Assignee",match filters.assignee.as_deref(){None=>"Anyone",Some("")=>"Unassigned",Some("@me")=>"Assigned to me",Some(id)=>app.actor_label(id)}.into()),("filter-thread","Task thread",filters.thread.map_or_else(||"All task threads".into(),|id|format!("#{id}")))] {
                    p=p.child(selector(key,title,value));
                    if app.details_picker!=Some(key){continue;}
                    let mut choices=col().w_full().max_h(124.).scroll(ScrollSpec{stick_bottom:false});
                    let options:Vec<(String,Option<String>)>=match key {
                        "filter-channel"=>std::iter::once(("All channels".into(),None)).chain(app.snapshot.channels.iter().filter(|c|projects::contains(&app.project,c)).map(|c|(format!("#{c}"),Some(c.clone())))).collect(),
                        "filter-assignee"=>[("Anyone".into(),None),("Assigned to me".into(),Some("@me".into())),("Unassigned".into(),Some(String::new()))].into_iter().chain(scoped().filter_map(|m|m.task.as_ref()?.assignee.as_deref()).collect::<std::collections::BTreeSet<_>>().into_iter().map(|id|(app.actor_label(id).to_string(),Some(id.into())))).collect(),
                        _=>std::iter::once(("All task threads".into(),None)).chain(scoped().map(|m|(format!("#{}  {}",m.id,m.text.lines().next().unwrap_or("")),Some(m.id.to_string())))).collect(),
                    };
                    let search=if options.len()>7 {p=p.child(form_input(&app.filter_choice_search,"filter-choice-search","Find an option…").on_edit(|a:&mut App|&mut a.filter_choice_search));app.filter_choice_search.text.trim().to_lowercase()}else{String::new()};
                    let mut found=false;
                    for (name,id) in options {if !name.to_lowercase().contains(&search){continue;}found=true;let selected=match key{"filter-channel"=>filters.channel==id,"filter-assignee"=>filters.assignee==id,_=>filters.thread.map(|n|n.to_string())==id};choices=choices.child(choice_row(name,selected,move|a|{match key {"filter-channel"=>{a.task_filters.channel=id.clone();a.task_filters.thread=None;},"filter-assignee"=>a.task_filters.assignee=id.clone(),_=>a.task_filters.thread=id.as_ref().and_then(|s|s.parse().ok())}a.details_picker=None;}));}if !found{choices=choices.child(muted("No matches"));}p=p.child(choices);
                }
            }
            if matches!(app.page,Page::Activity|Page::Scheduled) {p = p.child(selector("filter-sort","Sort order",if app.oldest { "Oldest first" } else { "Newest first" }.into()));}
            if app.details_picker == Some("filter-sort") {
                p = p.child(col().w_full().children([(false,"Newest first"),(true,"Oldest first")].into_iter().map(|(oldest,name)| {
                    choice_row(name.into(),app.oldest == oldest, move |a| {
                        a.oldest = oldest;
                        a.details_picker = None;
                        a.save_preferences();
                    }).hover(gray(32)).radius(7.)
                })));
            }
            p.child(button("Clear filters",|a|{a.query=edit("");a.task_filters=Default::default();a.details_picker=None;if a.page==Page::Tasks{a.filter=ALL_TASKS;}}))},
        Schedule=>crate::calendar::picker(app,panel),
    };
    if let Some([x,y,w,h]) = filter_anchor {
        let width = 360.0_f32.min((viewport[0]-8.).max(1.));
        let left = (x+w-width).clamp(4.,(viewport[0]-width-4.).max(4.));
        let top = (y+h+4.).clamp(4.,(viewport[1]-42.).max(4.));
        return col().key("filters-popover").absolute(left,top).w(width).max_h((viewport[1]-top-4.).max(1.))
            .border(1.,gray(48)).radius(8.).fill(gray(0)).clip_soft(8.).scroll(ScrollSpec {stick_bottom:false})
            .child(panel).modal(true, |a: &mut App| {a.dialog=None;a.details_picker=None;});
    }
    let footer = match page {
        Schedule => Some(crate::calendar::footer(app)),
        NewTask => Some(col().w_full().padding(10.).gap(5.).border_sides([1.,0.,0.,0.],gray(35))
            .children(app.error.as_ref().map(|e| muted(e).truncate()))
            .child(row().w_full().child(spacer()).child(dialog_action(button(if app.creating_task { "Saving…" } else if app.editing_task.is_some() { "Save changes" } else { "Create task" }, App::create_task)
                .key("create-task"))))),
        _ => None,
    };
    modal_frame("board-dialog", viewport, if page == Schedule { 388. } else { 436. }, Some(top), panel, footer, |a| a.dialog = None)
}
fn thread(app: &App, root: &Message,width:f32,height:f32) -> El {
    let id = root.id;
    let mut content = col().selection_group(true).w_full().min_h(app.thread_h).child(spacer()).child(message(app, root, true));
    let parent=app.organization.tasks.get(&id).and_then(|p|p.parent);
    let mut panel=col().key("thread-panel").w(width).h_full().clip_soft(0.0)
        .child(row().w_full().h(45.).px(10.).gap(6.).items_center().children(parent.map(|p|button("‹ Back to parent",move|a|a.open_thread(p)).key("subtask-back"))).child(label(if parent.is_some(){"subtask"}else if root.task.is_some(){"task"}else{"thread"})).child(muted(format!("in #{}",root.channel.strip_prefix(&app.project).and_then(|s|s.strip_prefix('/')).unwrap_or(&root.channel))).truncate().shrink()).child(spacer()).child(button("×",|a|a.thread=None))).child(line());
    if let Some(task) = &root.task {
        let placement = app.organization.tasks.get(&id);
        let global = placement.is_some_and(|p| p.global);
        let field=|name:&str,value:String| row().w_full().items_center().gap(8.).child(muted(name).w(90.)).child(label(value).grow(1.).w(0.).truncate());
        let status=row().w_full().gap(6.).items_center().child(task_status(task.state));
        let mut details=col().key("task-summary").w_full().padding(16.).gap(7.).child(status);
        details = details.child(row().w_full().items_center().gap(8.).child(muted("Created by").w(90.)).child(marked_sender(root, label(app.sender_label(root)).truncate())))
            .child(field("Assigned to", if global { "Everyone" } else { task.assignee.as_deref().map_or("Unassigned", |actor| app.actor_label(actor)) }.into()));
        let mut subtask_options=None;
        if let Some(parent) = placement.and_then(|p| p.parent) {
            let title = app.snapshot.messages.iter().find(|m| m.id == parent).and_then(|m| m.text.lines().next()).unwrap_or("");
            details = details.child(field("Parent task",format!("#{parent} {title}").trim_end().to_owned()).key("parent-task-link").radius(4.).hover(gray(35)).on_press(move |a:&mut App| a.open_thread(parent)));
        } else if !global {
            let children: Vec<_> = app.snapshot.messages.iter().filter(|m| app.organization.tasks.get(&m.id).and_then(|p| p.parent) == Some(id)).collect();
            let mut section=col().w_full().gap(4.);
            let has_children=!children.is_empty();
            if has_children {
                let done = children.iter().filter(|m| m.task.as_ref().is_some_and(|t| t.state==TaskState::Completed)).count();
                section = section.child(label("Subtasks")).child(muted(format!("{done} of {} complete",children.len())));
                let mut items = col().w_full();
                let height = (children.len().min(4) * 26) as f32;
                for child in children {
                    let id = child.id;
                    items = items.child(row().key(format!("open-subtask-{id}")).w_full().h(26.).px(5.).items_center().gap(6.).radius(4.).hover(gray(35)).child(label(format!("#{id} {}",child.text.lines().next().unwrap_or(""))).w(0.).grow(1.).truncate()).children(child.task.as_ref().map(|t|task_status(t.state))).on_press(move|a:&mut App|a.open_thread(id)));
                }
                section = section.child(col().key(format!("subtasks-{id}")).w_full().h(height).scroll(ScrollSpec{stick_bottom:false}).child(items));
            }
            if task.state!=TaskState::Completed {section=section.child(button("+ Add subtask", move |a| a.add_subtask(id)).key("add-subtask"));}
            if has_children || task.state!=TaskState::Completed {subtask_options=Some(section);}
        }
        if !global { details = details.child(field("Lease", lease_label(app,task))); }
        if let Some(started) = task.started_at { details = details.child(field("Started", format!("{} UTC",format_utc(started)))); }
        if let Some(actor) = &task.finished_by { details = details.child(field("Finished by", app.actor_label(actor).into())); }
        for dep in task.needs.iter().take(4){let status=app.snapshot.messages.iter().find(|m|m.id==*dep).and_then(|m|m.task.as_ref()).map_or("",|t|state_name(&t.state));let dep=*dep;details=details.child(field("needs",format!("#{dep} {status}")).on_press(move|a:&mut App|a.open_thread(dep)));}
        if task.needs.len()>4 {details=details.child(muted(format!("+{} more dependencies",task.needs.len()-4)));}
        if let Some(result)=&task.result{details=details.child(field("result",result.clone()));}
        let me=app.me.as_ref().map(|m|m.actor.as_str());
        let holder=active_lease(task, app.now).map(|(actor,_)|actor);
        let mine=!global && me.is_some() && holder==me && matches!(task.state,TaskState::Claimed|TaskState::InProgress);
        let creator=me==root.author.split('/').next();
        let active=!matches!(task.state,TaskState::Completed|TaskState::Failed|TaskState::Canceled);
        let claim=!global && holder.is_none() && matches!(task.state,TaskState::Ready|TaskState::InProgress) && (task.assignee.is_none() || task.assignee.as_deref()==me);
        let primary=|name:&str|el().h(30.).px(12.).center().radius(7.).fill(gray(226)).hover(gray(248)).child(label(name).color(gray(10)));
        let mut actions=row().w_full().flex_wrap().gap(8.).items_center();
        if global && active && let Some(me) = me {
            let done = placement.is_some_and(|p|p.done_by.iter().any(|actor|actor==me));
            actions = actions.child(primary(if done { "Undo my completion" } else { "Mark done for me" }).on_press(move |a: &mut App| {
                a.dispatch(Action::AcknowledgeTask { id, done: !done },None);
            }));
        }
        if claim{actions=actions.child(primary(if task.state==TaskState::InProgress {"Reclaim task"} else {"Claim task"}).key("claim-task").on_press(move|a:&mut App|a.dispatch(Action::Claim{id,lease_secs:300},None)));}
        if mine && task.state==TaskState::Claimed {actions=actions.child(primary("Start").key("start-task").on_press(move|a:&mut App|a.dispatch(Action::Start{id},None)));}
        if mine {
            let unfinished=app.snapshot.messages.iter().any(|m|app.organization.tasks.get(&m.id).and_then(|p|p.parent)==Some(id) && m.task.as_ref().is_some_and(|t|t.state!=TaskState::Completed));
            if unfinished && app.confirm_parent_completion==Some(id) {
                details=details.child(muted("Unfinished subtasks will keep their current status.").wrap());
                actions=actions.child(button("Cancel",|a|a.confirm_parent_completion=None)).child(primary("Complete parent anyway").on_press(move|a:&mut App|{a.confirm_parent_completion=None;let text=a.reply.text.clone();a.dispatch(Action::CompleteAnyway{id,text:text.clone()},Some((Some(id),text)));}));
            } else if unfinished {actions=actions.child(primary("Complete task").on_press(move|a:&mut App|a.confirm_parent_completion=Some(id)));}
            else {actions=actions.child(primary("Complete task").on_press(move|a:&mut App|{let text=a.reply.text.clone();a.dispatch(Action::Complete{id,text:text.clone()},Some((Some(id),text)));}));}
        }
        if active && (creator||mine){actions=actions.child(button("More actions ▾",move|a|a.task_menu=if a.task_menu==Some(id){None}else{Some(id)}).key("task-more-actions").radius(7.).border(1.,gray(52)));}
        if global||claim||mine||(active&&creator){details=details.child(actions);}
        if active && app.task_menu==Some(id) && (mine||creator){
            let mut menu=col().w_full().padding(6.).radius(7.).border(1.,gray(52));
            if mine || (creator && holder.is_none()) {
                menu = menu.child(button("Edit task", |_| {}).key("edit-task").on_press_tx(move |a: &mut App, tx| {
                    a.open_task_editor(id);
                    if a.dialog == Some(Dialog::NewTask) { tx.focus("task-title", false); }
                }));
            }
            if mine {menu=menu.child(button("Extend claim by 5 minutes",move|a|{a.task_menu=None;a.dispatch(Action::Renew{id,lease_secs:300},None);})).child(button("Mark failed",move|a|{a.task_menu=None;let text=a.reply.text.clone();a.dispatch(Action::Fail{id,text:text.clone()},Some((Some(id),text)));}));}
            if creator {menu=menu.child(button("Cancel task",move|a|{a.task_menu=None;a.dispatch(Action::Cancel{id},None);}));}
            details=details.child(menu);
        }
        if let Some(options)=subtask_options {details=details.child(line()).child(options);}
        let max=(height-215.).max(48.);let summary=app.pane_sizes[2].clamp(48.,max);
        panel=panel.child(col().key("task-details-scroll").w_full().max_h(summary).scroll(ScrollSpec{stick_bottom:false}).child(details)).child(divider(2,summary,48.,max));
    }
    let mut replies: Vec<_> = app.snapshot.messages.iter().filter(|m| m.parent == Some(id)).collect();
    replies.sort_by_key(|m| (m.created_at,m.id));
    if !replies.is_empty() {
        content = content.child(line()).child(muted(format!("{} {}", replies.len(),if replies.len()==1{"reply"}else{"replies"})).key("thread-reply-count").padding(12.0));
    }
    let start=replies.len().saturating_sub(app.reply_limit);
    if start>0 {
        let anchor=format!("reply-{}",replies[start].id);
        let limit=app.reply_limit;
        content=content.child(el().key(format!("reply-edge-{id}-{limit}")).w_full().h(4.).on_visible(move|a:&mut App,on,tx|{
            if on && a.thread==Some(id) && a.reply_limit==limit {tx.keep_on_screen(anchor.clone()); a.reply_limit=limit.saturating_add(40);}
        }));
    }
    for reply in replies.into_iter().skip(start) { content = content.child(message(app, reply, true)); }
    if let Some(message) = app.attachment_message.filter(|selected| *selected == id || app.snapshot.messages.iter().any(|m| m.id == *selected && m.parent == Some(id))) {
        content = content.child(attachment_panel(app, message));
    }
    panel
        .child(col().key(format!("thread-scroll-{id}")).w_full().grow(1.0).h(0.0).scroll(ScrollSpec { stick_bottom: true })
            .on_layout(|a: &mut App, r| a.thread_h = r[3]).child(content))
        .child(composer(app, true))
}

fn choice_row(name:String,selected:bool,action:impl Fn(&mut App)+'static)->El {
    row().w_full().h(28.).px(8.).gap(6.).items_center().radius(4.).hover(gray(36)).child(label(name).selectable(false).grow(1.).w(0.).truncate()).child(muted(if selected{"✓"}else{""}).w(15.).selectable(false)).on_press(action)
}
fn select_trigger(value:String,open:bool,action:impl Fn(&mut App)+'static)->El {
    row().h(30.).px(8.).gap(8.).items_center().radius(4.).fill(gray(if open{30}else{16})).hover(gray(35))
        .child(label(value).selectable(false).truncate().shrink()).child(spacer()).child(muted(if open{"▴"}else{"▾"}).selectable(false)).on_press(action)
}

fn agent_picker(app:&App)->El {
    let mut picker = col().w_full().gap(4.)
        .child(select_trigger(app.setup_agent.name().into(),app.setup_choices,|a| a.setup_choices = !a.setup_choices).w_full().key("setup-agent"));
    if app.setup_choices {
        let mut choices = col().w_full().padding(4.).radius(6.).fill(gray(17)).border(1.,gray(55));
        for agent in setup_guide::Agent::ALL {
            choices = choices.child(choice_row(agent.name().into(),app.setup_agent==agent,move |a:&mut App| {
                a.setup_agent = agent; a.setup_choices = false; a.home_copied = None;
            }).key(format!("setup-agent-{}",agent.name())));
        }
        picker = picker.child(choices);
    }
    picker
}

fn setup_drawer(app:&App, width: f32)->El {
    let step = |index: &str, title: &str, description: &str, body: El| row().w_full().gap(12.)
        .child(col().w(24.).items_center().child(el().size(24.,24.).center().radius(5.).border(1.,gray(48)).child(muted(index))).child(el().w(1.).grow(1.).fill(gray(35))))
        .child(col().w(0.).grow(1.).gap(7.).pb(18.).child(label(title).font(TextStyle::new(14.,600.)).wrap())
            .child(muted(description).wrap()).child(body));
    let copy=|key:&'static str,title:&'static str,contents:String|dialog_action(button(if app.home_copied==Some(key){"Copied!"}else{title}, |_| {})).key(key)
        .on_press_tx(move |a:&mut App,tx|{tx.copy(contents.clone());a.home_copied=Some(key);});
    let block = |key,title,contents:String| col().w_full().padding(8.).gap(4.).radius(6.).fill(gray(17)).border(1.,gray(42))
        .child(row().child(spacer()).child(copy(key,title,contents.clone())))
        .child(el().w_full().scroll_x().child(label(contents).font(TextStyle::new(11.,400.))));
    let p=app.project.as_str();
    let guide = setup_guide::Guide::new(app);
    let picker = agent_picker(app);
    let mut cards=col().w_full().padding(16.).gap(12.)
        .child(row().w_full().gap(5.).items_center().child(label("Follow these steps").wrap().w(0.).grow(1.))
            .child(copy("copy-setup","Copy for agent",guide.copy_text())))
        .child(step("1","Connect to this board",&guide.intro,el()))
        .child(step("2","Connect MCP","Choose your agent to see its configuration.",picker.child(muted(&guide.instruction).wrap()).child(block("copy-mcp-config","Copy",guide.snippet))))
        .child(step("3","See available channels",&guide.question, el()));
    cards = cards.child(row().gap(4.).flex_wrap().children((!app.remote()).then(|| button("Share this board", |a| { a.setup_open = false; a.open_share(); })))
        .child(button("Join another board", |a| { a.setup_open = false; a.begin_add_board(); a.onboarding.joining = true; })));
    let panel=col().key("setup-drawer").w(width.min(490.)).h_full().fill(gray(0)).border(1.,gray(48))
        .child(row().w_full().h(56.).px(16.).items_center().gap(8.).child(label("Setup").font(TextStyle::new(17.,600.))).child(muted(p).truncate().shrink()).child(spacer()).child(button("Close",|a|a.setup_open=false)))
        .child(line()).child(col().w_full().grow(1.).h(0.).scroll(ScrollSpec{stick_bottom:false}).child(cards));
    el().absolute(0.,0.).w_full().h_full().fill(rune::Color{r:0.,g:0.,b:0.,a:0.64}).child(panel.pin(None,Some(0.),Some(0.),Some(0.)).modal(true,|a:&mut App|a.setup_open=false))
}
fn home(app:&App, [width,height]:[f32;2])->El {
    let width=width-if app.sidebar{200.}else{0.};
    let narrow=width<540.;
    if narrow || height < 480. {
        let shortcut = |title, key| el().key(key).w_full().h(34.).center().child(splash::card_title(app,title)).border(1.,gray(48)).radius(7.).hover(gray(24));
        return col().w_full().h(0.).grow(1.).items_center().padding(10.).gap(5.)
            .child(crate::splash::view(app,(width-20.).min((height-166.).max(0.)*3.)))
            .child(shortcut("Setup", "home-setup").on_press(|a: &mut App|{a.setup_open=true;a.home_copied=None;}))
            .child(shortcut("Tasks", "home-tasks").on_press(|a: &mut App|a.navigate(Page::Tasks)))
            .child(shortcut("Activity", "home-activity").on_press(|a: &mut App|a.navigate(Page::Activity)));
    }
    let module=|key:&str| col().key(key).padding(14.).gap(7.).radius(12.).border(1.,gray(52)).hover(gray(18));
    let top=|tile:El| tile.w(0.).grow(1.).min_h(136.);
    let setup=module("home-setup").child(muted("01 / GET STARTED")).child(splash::card_title(app,"Setup"))
        .child(muted("Connect an agent, invite a teammate, or join a board.").wrap()).child(spacer()).child(label("Open guide  ›"))
        .on_press(|a:&mut App|{a.setup_open=true;a.home_copied=None;});
    let preview=|tasks:bool| {
        let mut items=Vec::with_capacity(4);
        for m in app.snapshot.messages.iter().filter(|m|projects::contains(&app.project,&m.channel)) {
            if tasks && !m.task.as_ref().is_some_and(|t| matches_filter(&t.state, OPEN_TASKS)){continue;}
            let rank=(tasks && m.task.as_ref().is_some_and(|t|t.state==TaskState::Claimed),m.created_at,m.id);
            let pos=items.partition_point(|(other,_)|*other>rank);
            if pos<3 {items.insert(pos,(rank,m));items.truncate(3);}
        }
        let mut panel=module(if tasks{"home-tasks"}else{"home-activity"}).child(muted(if tasks{"02 / WORK"}else{"03 / FEED"}))
            .child(row().w_full().items_center().child(splash::card_title(app,if tasks{"Tasks"}else{"Activity"})).child(spacer()).child(muted("View all ›"))).child(line());
        if items.is_empty(){panel=panel.child(muted(if tasks{"No open tasks."}else{"No activity in this project yet."}).wrap());}
        for (_,m) in items {let prefix=if tasks{muted(format!("{:?}",m.task.as_ref().unwrap().state)).truncate().shrink()}else{marked_sender(m,muted(app.sender_label(m)).truncate().shrink())};panel=panel.child(row().w_full().gap(6.).child(prefix).child(label(m.text.lines().next().unwrap_or("")).grow(1.).w(0.).truncate()));}
        panel.on_press(move|a:&mut App|{a.query=edit("");a.filter=ALL_TASKS;a.navigate(if tasks{Page::Tasks}else{Page::Activity});})
    };
    let wordmark_width = 540.0_f32.min((height - 370.) * 2.8);
    let cards=col().w_full().gap(10.).child(crate::splash::view(app,wordmark_width)).child(row().w_full().gap(12.).child(top(setup)).child(top(preview(true)))).child(preview(false).w_full());
    col().w_full().grow(1.).h(0.).child(col().w_full().padding(20.).items_center().child(cards))
}

fn onboarding_panel(app: &App) -> El {
    use onboarding::Step;
    let o = &app.onboarding;
    if o.switching { return el().padding(22.).radius(12.).fill(gray(20)).child(label("Opening board…")); }
    let (step, title, detail) = match o.step {
        Step::Name => ("01 / PROFILE", "Welcome to NeoBoard", "Choose the name teammates will see on the board."),
        Step::Board => ("02 / BOARD", if o.adding { "Add a board" } else { "Join or create a board" }, "Create a local board or join one with a private invite."),
        Step::Connect => ("03 / AGENT SETUP", "Connect your agent", "Choose your agent and copy its setup guide. You can also do this later from Home."),
    };
    let mut panel = col().w_full().padding(18.).gap(10.)
        .child(muted(step)).child(label(title).font(TextStyle::new(20., 600.)).color(splash::wordmark_ink()).wrap())
        .child(muted(detail).w_full().wrap());
    match o.step {
        Step::Name => {
            panel = panel.child(form_input(&o.name, "onboarding-name", "Your display name").on_edit(|a: &mut App| &mut a.onboarding.name))
                .child(button("Continue", |_| {}).key("onboarding-next").on_press_tx(|a: &mut App, tx| {
                    if a.onboarding.name.text.trim().is_empty() { a.onboarding.error = Some("Enter your display name first.".into()); }
                    else {
                        a.onboarding.step = Step::Board; a.onboarding.error = None;
                        a.onboarding.joining = a.remote();
                        tx.focus(if a.onboarding.joining { "onboarding-invite" } else { "onboarding-board-name" }, false);
                    }
                }));
        }
        Step::Board => {
            if !app.remote() {
                let mode = |title, key, joining| {
                    let selected = o.joining == joining;
                    el().key(key).h(30.).px(12.).center().hover(gray(24))
                        .border_sides([0.,0.,1.,0.], gray(if selected { 190 } else { 35 }))
                        .child(label(title).color(gray(if selected { 232 } else { 151 })))
                        .on_press_tx(move |a: &mut App, tx| {
                            if !a.onboarding.busy {
                                a.onboarding.joining = joining;
                                a.onboarding.error = None;
                                tx.focus(if joining { "onboarding-invite" } else { "onboarding-board-name" }, false);
                            }
                        })
                };
                panel = panel.child(row().w_full().border_sides([0.,0.,1.,0.],gray(35))
                    .child(mode("Create", "onboarding-create", false)).child(mode("Join", "onboarding-join", true)));
            }
            if o.joining {
                panel = panel.child(muted("Paste the private invite link from your host. No separate password is needed. Anyone with the link can join, so keep it out of public chats.").w_full().wrap());
                panel = panel.child(form_input(&o.invite, "onboarding-invite", "Paste invite link").on_edit(|a: &mut App| &mut a.onboarding.invite));
            } else {
                panel = panel.child(form_input(&o.board, "onboarding-board-name", "Board name").on_edit(|a: &mut App| &mut a.onboarding.board));
            }
            panel = panel.child(if o.busy { muted("Connecting…") } else {
                row().gap(8.).child(dialog_action(button(if o.joining { "Join board" } else { "Create board" }, App::submit_board_setup).key("onboarding-submit")))
                    .child(button(if o.adding { "Cancel" } else { "Back" }, |a| {
                        if a.onboarding.adding { a.onboarding = Default::default(); }
                        else { a.onboarding.step = Step::Name; a.onboarding.error = None; }
                    }))
            });
        }
        Step::Connect => {
            let prompt = setup_guide::Guide::new(app).copy_text();
            panel = panel.child(chip(app.project.clone())).child(agent_picker(app)).child(row().gap(8.)
                .child(button(if app.home_copied == Some("onboarding-copy") { "Copied!" } else { "Copy for agent" }, |_| {})
                    .key("onboarding-copy").on_press_tx(move |a: &mut App, tx| { tx.copy(prompt.clone()); a.home_copied = Some("onboarding-copy"); }))
                .child(button("Open board", |a| a.onboarding = Default::default()).key("onboarding-open")));
        }
    }
    if let Some(error) = &o.error { panel = panel.child(label(error).color(rune::oklch(0.75, 0.1, 20.)).wrap().w_full()); }
    panel
}

fn onboarding_layer(app: &App, viewport: [f32; 2]) -> El {
    modal_frame("board-onboarding", viewport, 460., None, onboarding_panel(app), None, |_| {})
}

fn update_layer(app: &App, viewport: [f32; 2]) -> Option<El> {
    use release_update::Stage;
    let (title, detail) = match &app.release.stage {
        Stage::Hidden => return None,
        Stage::Checking => ("Checking for updates", "Contacting GitHub Releases…".into()),
        Stage::Notice(detail) => ("Check for updates",detail.clone()),
        Stage::ManualRelease(version, _) => ("Update available", format!("Neoboard {version} is available. No automatic update is available for this installation. View the official release for manual options. Before upgrading a host, stop its daemon and back up the entire data directory.")),
        Stage::Offer(version, action) => ("Update available", format!("Neoboard {version} is available. {}", action.instructions())),
        Stage::Running => ("Preparing update", "The updater is working. It does not report progress.".into()),
        Stage::HandedOff(installer, detail) => (match installer { release_update::Installer::Msi => "Windows Installer started", release_update::Installer::Dmg => "Disk image opened" }, format!("Finish the upgrade outside NeoBoard. Installation is not confirmed. {}\n\n{detail}", release_update::RECONNECT)),
        Stage::Finished(Ok(detail)) if cfg!(windows) => ("Update staged", format!("Your running installation has not been replaced. After switching, point MCP clients to the new bundle. {}\n\n{detail}", release_update::RECONNECT)),
        Stage::Finished(Ok(detail)) => ("Updater finished", format!("Finish your drafts before restarting Neoboard and its services.\n\n{detail}")),
        Stage::Finished(Err(error)) => ("Update not confirmed", format!("{error}\n\nKeep your drafts. Check the installation before retrying. {}", release_update::RECONNECT)),
    };
    let mut body = col().w_full().padding(20.).gap(12.).child(label(title).font(TextStyle::new(16.,600.))).child(muted(detail).wrap());
    if matches!(app.release.stage, Stage::Running) {
        body = body.child(el().key("update-indeterminate").w(160.).h(5.).radius(2.).fill(gray(53))
            .child(el().absolute(112.*app.release.pulse,0.).w(48.).h(5.).radius(2.).fill(gray(200))));
    } else {
        let mut actions = row().gap(8.).child(dialog_action(button("Dismiss", |a| a.release.dismiss())));
        if let Stage::Offer(_, action) = app.release.stage {
            actions = actions.child(dialog_action(button(action.label(), |a| a.release.accept()).key("start-update")));
        } else if let Stage::ManualRelease(_, url) = &app.release.stage {
            let url = url.clone();
            actions = actions.child(dialog_action(button("View release", |_| {}).key("view-release").on_press_tx(move |a: &mut App, tx| {
                if let Err(e) = tx.open_url(&url) { a.release.stage = Stage::Notice(format!("Could not open {url}: {e}")); }
            })));
        }
        body = body.child(actions);
    }
    Some(modal_frame("release-update", viewport, 416., None, body, None, |a| a.release.dismiss()))
}

pub fn root(app: &App, cx: &mut Cx<App>) -> El {
    let insets = cx.chrome_insets();
    let native_title = insets.height > 0.0 && (insets.leading > 0.0 || insets.trailing > 0.0);
    let first_run = app.onboarding_visible() && app.project.is_empty();
    let viewport = zoom::viewport(app, cx);
    let height = if native_title { insets.height } else { 0. };
    let titlebar = native_title.then(|| {
        cx.caption_color(splash::wordmark_ink());
        row().key("titlebar").w_full().h(height).fill(gray(26)).items_center().window_drag()
            .border_sides([0.0, 0.0, 1.0, 0.0], gray(28))
    });
    let shell = col().w_full().h_full().fill(gray(0));
    // Rune modals block earlier hit targets. Reserve the usual band but place its native
    // drag target above onboarding, leaving the board behind the modal and out of reach.
    let (shell, onboarding_chrome) = if app.onboarding_visible() && matches!(app.release.stage, release_update::Stage::Hidden) {
        (shell.pt(height), titlebar.map(|bar| bar.absolute(0., 0.)))
    } else {
        (shell.children(titlebar), None)
    };
    if first_run {
        let content = el().w_full().h_full().child(onboarding_layer(app, viewport)).children(update_layer(app, viewport));
        return shell.child(zoom::wrap(app, cx, content)).children(onboarding_chrome);
    }
    app.profiles.begin_view();
    let mut main = col().key("main").w(0.0).grow(1.0).h_full().clip_soft(0.0).child(header(app)).child(line());
    if app.page==Page::Home {main=main.child(home(app,viewport));} else if app.page==Page::Tasks && app.sprint.show {main=main.child(sprints::browser(app));} else {
    let messages = app.visible();
    let paged = matches!(app.page, Page::Tasks | Page::Activity | Page::Channel(_));
    let scope = format!("{:?}:{:?}:{:?}:{}:{}:{:?}",app.page,app.project,app.query.text,app.oldest,app.filter,app.task_filters);
    let limit = if !paged { usize::MAX } else if app.feed_page.0 == scope { app.feed_page.1 } else { 40 };
    let older_above = matches!(app.page,Page::Channel(_)) || app.page == Page::Activity && app.oldest;
    let start = if older_above { messages.len().saturating_sub(limit) } else { 0 };
    let more = messages.len() > limit;
    let sentinel = || {
        let scope = scope.clone();
        let anchor = older_above.then(|| format!("message-{}",messages[start].id));
        el().key(format!("page-edge-{scope}-{limit}")).w_full().h(4.).on_visible(move |a:&mut App,on,tx| {
            if on { if let Some(key) = &anchor { tx.keep_on_screen(key.clone()); } a.feed_page = (scope.clone(),limit.saturating_add(40)); }
        })
    };
    let mut list = col().selection_group(true).w_full();
    if older_above { list = list.min_h(app.feed_h).child(spacer()); }
    if more && older_above { list = list.child(sentinel()); }
    if app.page == Page::Scheduled {
        list = list.child(muted("Your pending scheduled posts · UTC (not local time)").wrap().padding(20.0));
        let query = app.query.text.to_lowercase();
        let mut scheduled: Vec<_> = app.schedules.iter().filter(|s| projects::contains(&app.project,&s.channel) && (query.is_empty() || format!("{} {} {} {} {:?}",s.id,s.channel,s.author,s.text,s.labels).to_lowercase().contains(&query))).collect();
        scheduled.sort_by_key(|s| (s.publish_at,s.id));
        if !app.oldest { scheduled.reverse(); }
        if scheduled.is_empty() { list = list.child(muted("No pending scheduled posts match this view").padding(20.0)); }
        for post in scheduled {
            let id = post.id;
            list = list.child(col().w_full().padding(16.0).gap(6.0)
                .child(muted(format!("#{id} · #{} · {} UTC · in {}m", post.channel, format_utc(post.publish_at), post.publish_at.saturating_sub(app.now)/60)).wrap())
                .child(label(&post.text).wrap().w_full())
                .child(row().w_full().scroll_x().gap(5.0).children(post.labels.iter().map(|(k,v)| chip(format!("{k}={v}")))))
                .child(button("cancel scheduled post", move |a| a.dispatch(Action::CancelSchedule { id }, None)))
                .child(line()));
        }
    } else if messages.is_empty() {
        list = list.child(muted(if app.status == "connecting" { "connecting to board…" } else { "No messages match this view." }).padding(20.0));
    } else {
        for m in messages.iter().skip(start).take(limit) { list = list.child(if app.page == Page::Tasks { task_row(app,m) } else { message(app,m,false) }); }
    }
    if more && !older_above { list = list.child(sentinel()); }
    main = main.child(col().key(if paged {format!("paged-{scope}")} else { format!("feed-{:?}-{}", match &app.page { Page::Home => "home", Page::Activity => "activity", Page::Tasks => "tasks", Page::Scheduled => "scheduled", Page::Channel(c) => c }, app.filter) })
        .w_full().grow(1.0).h(0.0).scroll(ScrollSpec { stick_bottom: older_above })
        .on_layout(|a: &mut App, r| a.feed_h = r[3]).child(list));
    if let Some(error) = &app.error { main = main.child(label(error).color(rune::oklch(0.75,0.1,20.0)).wrap().padding(12.0)); }
    if let Some(notice) = &app.notice { main = main.child(muted(notice).wrap().px(20.0)); }
    if !matches!(app.page, Page::Activity | Page::Tasks) {main = main.child(composer(app, false));}
    }
    let mut root = row().key("neochat").w_full().h_full().fill(gray(0)).on_layout(|a: &mut App, r| a.width = r[2]);
    let show_left=app.sidebar && (app.width >= 900.0 || app.thread.is_none());
    let left_max=(viewport[0]-if app.thread.is_some(){540.}else{230.}).clamp(140.,420.);
    let left=if show_left {app.pane_sizes[0].clamp(140.,left_max)} else {0.};
    if show_left { root = root.child(sidebar(app,left)).child(divider(0,left,140.,left_max)); }
    root = root.child(main);
    if let Some(thread_root) = app.thread.and_then(|id| app.snapshot.messages.iter().find(|m| m.id == id)) {
        let max=(viewport[0]-left-210.).max(110.);let min=245_f32.min(max);let right=app.pane_sizes[1].clamp(min,max);
        root = root.child(divider(1,right,min,max)).child(thread(app,thread_root,right,viewport[1]-height));
    }
    if let Some(id)=app.row_menu && let Some(m)=app.snapshot.messages.iter().find(|m|m.id==id && m.parent.is_none()){
        let width = (viewport[0] - 8.).clamp(1.,175.);
        let mut menu=col().key("thread-context").w(width).max_h((viewport[1]-8.).max(1.)).scroll(ScrollSpec { stick_bottom: false }).padding(5.).gap(2.).fill(gray(18)).border(1.,gray(57)).radius(7.)
            .child(menu_action("Open thread","›",false).on_press(move|a:&mut App|{a.row_menu=None;a.open_thread(id);})).child(menu_action("Copy thread ID","#",false).on_press_tx(move|a:&mut App,tx|{tx.copy(id.to_string());a.row_menu=None;}));
        let can_delete=app.me.as_ref().map(|m|m.actor.as_str())==m.author.split('/').next();
        if can_delete{menu=menu.child(line()).child(menu_action("Delete thread…","×",true).on_press(move|a:&mut App|{a.row_menu=None;a.error=None;a.dialog=Some(Dialog::DeleteThread(id));}).key("delete-option"));}
        let [x,mut y,w,h]=app.row_anchor;
        if native_title { y -= height; } // Convert window coordinates to the board viewport.
        let menu_h = (if can_delete{103.0_f32}else{70.}).min((viewport[1]-8.).max(1.));
        let top = if y + h + menu_h <= viewport[1] - 4. { y + h } else { y - menu_h };
        root=root.child(menu.absolute((x+w-width).clamp(4.,(viewport[0]-width-4.).max(4.)),top.clamp(4.,(viewport[1]-menu_h-4.).max(4.))).modal(true,|a:&mut App|a.row_menu=None));
    }
    if let Some(channel) = &app.channel_actions.menu && let Some([x,y,_,h]) = cx.layout(&format!("channel-options-{channel}")) {
        let copy = channel.clone();
        let mut menu = col().key("channel-context").padding(5.).gap(2.).fill(gray(18)).border(1.,gray(57)).radius(7.)
            .child(menu_action("Copy channel", "#", false).key("copy-channel").on_press_tx(move |a:&mut App,tx| { tx.copy(copy.clone()); a.channel_actions.menu = None; })).child(line());
        if let Some(reason) = app.deletion_restriction(channel) {
            menu = menu.child(muted(reason).key("channel-protected").wrap().padding(6.));
        } else {
            menu = menu.child(menu_action("Delete channel…", "×", true).key("delete-channel-option").on_press(|a:&mut App| {
                a.channel_actions.target = a.channel_actions.menu.take(); a.error = None; a.dialog = Some(Dialog::DeleteChannel);
            }));
        }
        let width = (viewport[0]-8.).clamp(1.,220.);
        root = root.child(menu.w(width).absolute(x.clamp(4.,(viewport[0]-width-4.).max(4.)), (y+h-height).clamp(4.,(viewport[1]-96.).max(4.)))
            .modal(true,|a:&mut App| a.channel_actions.menu = None));
    }
    if let Some(page)=app.dialog {
        if page != Dialog::Filters { root=root.child(dialog(app,page,viewport,None)); }
        else if let Some([x,y,w,h]) = cx.layout("filters") {
            root=root.child(dialog(app,page,viewport,Some([x,y-height,w,h])));
        }
    }
    if app.setup_open {root=root.child(setup_drawer(app,viewport[0]));}
    app.profiles.end_view();
    if app.onboarding_visible() { root = root.child(onboarding_layer(app, viewport)); }
    root = root.children(update_layer(app, viewport));
    let menu = if matches!(app.release.stage, release_update::Stage::Hidden) && app.project_menu && app.sidebar && (app.width >= 900.0 || app.thread.is_none()) {
        cx.layout("projects-toggle").map(|[x, y, _, h]| board_menu(app, x, y + h - height, viewport, height))
    } else { None };
    shell.child(zoom::wrap(app, cx, root)).children(menu).children(onboarding_chrome)
}

fn status_name(index: usize) -> String {
    let name = FILTERS[index];
    format!("{}{}", name[..1].to_ascii_uppercase(), &name[1..])
}

fn board_menu(app: &App, x: f32, y: f32, viewport: [f32; 2], chrome: f32) -> El {
        static SHARE: rune::Icon = rune::Icon { name: "Share board", view: 24., body: r#"<path d="M12 15V3m-4 4 4-4 4 4M7 10H5a1 1 0 0 0-1 1v8a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-8a1 1 0 0 0-1-1h-2" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/>"# };
        let scale = app.zoom.factor();
        let width = (viewport[0] - 8.).clamp(1., 208.);
        let x = x.clamp(4., (viewport[0] - width - 4.).max(4.));
        let y = y.clamp(4., (viewport[1] - 52.).max(4.));
        let list_height = (viewport[1] - y - 52.).clamp(0.,135.);
        let mut entries = col().w_full().gap(2.);
        let local_projects = app.projects();
        let mut ids = local_projects.clone();
        ids.extend(app.saved_boards.keys().map(String::as_str));
        for id in ids {
            let share = (local_projects.contains(id) && (!app.remote() || id == app.project)).then(|| {
                let id = id.to_owned();
                el().key(format!("share-board-{id}")).size(26.,26.).center().radius(5.).hover(gray(56))
                    .child(rune::icon(&SHARE, 15.).color(splash::wordmark_ink())).on_press(move |a: &mut App| a.open_board_share(&id))
            });
            let remote = if local_projects.contains(id) {app.remote()} else {app.saved_boards.get(id).is_some_and(Option::is_some)};
            let id = id.to_owned();
            entries = entries.child(row().key(format!("project-{id}")).w_full().h(32.).px(6.).gap(4.).items_center().radius(5.).hover_group_focus().hover(gray(36))
                .child(row().w(0.).grow(1.).h_full().items_center().gap(4.).child(board_icon(remote,15.)).child(label(&id).truncate().shrink()).child(spacer())
                    .child(muted(if id == app.project {"✓"}else{""})).on_press(move |a: &mut App| a.switch_board(&id)))
                .children(share));
        }
        col().key("projects-menu").absolute(x * scale, chrome + y * scale).origin(0.,0.).scale(scale).w(width).padding(6.).gap(4.).fill(gray(0)).border(1.,gray(62)).radius(8.)
            .child(col().w_full().max_h(list_height).scroll(ScrollSpec {stick_bottom:false}).child(entries))
            .child(line()).child(button("Add new +", |a| {
                a.project_menu = false;
                if !a.project.is_empty() { a.begin_add_board(); }
            }).key("add-board").w_full().radius(5.).hover(gray(36)))
            .modal(true, |a: &mut App| a.project_menu = false)
}