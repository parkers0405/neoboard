use super::*;
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct State {
    pub menu: Option<String>,
    pub target: Option<String>,
    pub sending: Option<String>,
}

impl App {
    pub(super) fn deletion_restriction(&self, channel: &str) -> Option<&'static str> {
        if !channel.contains('/') {
            Some("Board root channels are protected.")
        } else if channel.split_once('/').is_some_and(|(_, leaf)| leaf == "general") {
            Some("The default general channel is protected.")
        } else if self.me.as_ref().is_none_or(|m| m.actor != "local") {
            Some("Only the board host can delete channels.")
        } else { None }
    }

    pub(super) fn confirm_channel_delete(&mut self) {
        let Some(channel) = self.channel_actions.target.clone() else { return };
        if self.dialog != Some(Dialog::DeleteChannel) || self.pending || !self.snapshot.channels.contains(&channel) || self.deletion_restriction(&channel).is_some() { return }
        self.dispatch(Action::DeleteChannel { channel: channel.clone() }, None);
        if self.pending { self.channel_actions.sending = Some(channel); }
    }

    pub(super) fn invalidate_channel(&mut self, channel: &str) {
        let mut ids = BTreeSet::new();
        for snapshot in std::iter::once(&mut self.snapshot).chain(self.board_cache.iter_mut()) {
            snapshot.channels.retain(|c| c != channel);
            snapshot.messages.retain(|m| { if m.channel == channel { ids.insert(m.id); false } else { true } });
        }
        self.positions = self.snapshot.messages.iter().enumerate().map(|(i,m)|(m.id,i)).collect();
        self.board_extra.0.retain(|r| r.channel != channel);
        self.board_extra.1.retain(|s| s.channel != channel);
        self.reactions.retain(|r| r.channel != channel);
        self.schedules.retain(|s| s.channel != channel);
        self.seen.remove(channel);
        self.attachments.retain(|id,_| !ids.contains(id));
        self.reply_labels.retain(|id,_| !ids.contains(id));
        self.reply_drafts.retain(|id,_| !ids.contains(id));
        self.organization.tasks.retain(|id,_| !ids.contains(id));
        let removed = |id: Option<u64>| id.is_some_and(|id| ids.contains(&id));
        if removed(self.thread) { self.thread = None; }
        if removed(self.reply_owner) { self.reply_owner = None; self.reply = edit(""); }
        if removed(self.compose_file_owner) { self.compose_file_owner = None; self.compose_file_open = false; self.compose_file = edit(""); }
        if removed(self.attachment_message) { self.attachment_message = None; self.download_id = None; }
        if removed(self.task_menu) { self.task_menu = None; }
        if removed(self.row_menu) { self.row_menu = None; }
        if removed(self.hover_row) { self.hover_row = None; }
        if removed(self.task_filters.thread) { self.task_filters.thread = None; }
        if self.task_filters.channel.as_deref() == Some(channel) { self.task_filters.channel = None; }
        if self.member_list.as_ref().is_some_and(|(c,_)| c == channel) { self.member_list = None; }
        if matches!(self.dialog, Some(Dialog::ReplyTags(id) | Dialog::DeleteThread(id)) if ids.contains(&id)) { self.dialog = None; }
        if self.channel_actions.menu.as_deref() == Some(channel) { self.channel_actions.menu = None; }
        if self.channel_actions.target.as_deref() == Some(channel) {
            self.channel_actions.target = None;
            if self.dialog == Some(Dialog::DeleteChannel) { self.dialog = None; }
        }
        if matches!(&self.page, Page::Channel(c) if c == channel) { self.navigate(Page::Activity); }
        if self.channel.text == channel { self.default_channel(); }
        if self.task_channel == channel || removed(self.task_parent) || removed(self.editing_task) {
            self.clear_task_draft();
            if self.dialog == Some(Dialog::NewTask) { self.dialog = None; }
        }
    }
}

pub(super) fn menu_scene(app: &mut App) {
    fixture::load(app, false);
    app.me = Some(neochat::protocol::Member { actor: "local".into(), display_name: Some("Parker".into()) });
    app.channel_actions.menu = Some(projects::channel(&app.project, "dev/rune"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_options_require_host_and_explicit_confirmation() {
        let mut app = App::new();
        menu_scene(&mut app);
        let target = app.channel_actions.menu.take().unwrap();
        let root = app.project.clone();
        app.snapshot.channels.push(root.clone());
        assert!(app.deletion_restriction(&format!("{root}/general")).is_some());
        app.me.as_mut().unwrap().actor = "guest".into();
        assert!(app.deletion_restriction(&target).is_some());
        app.me.as_mut().unwrap().actor = "local".into();
        let (tx,rx) = mpsc::sync_channel(4); app.commands = Some(tx);
        let mut rt = rune::Runtime::new(app,view::root,(1252.,674.),rune::Env::new(true,1.));
        let click = |rt: &mut rune::Runtime<App>, key: &str| {
            for _ in 0..24 { rt.frame(1./60.); }
            let (x,y) = rt.find(key).unwrap_or_else(|| panic!("missing {key}"));
            rt.pointer_move(x,y); rt.pointer_down(x,y); rt.pointer_up(x,y); rt.frame(1./60.);
        };
        click(&mut rt,&format!("channel-options-{root}"));
        assert!(rt.find("channel-protected").is_some());
        assert!(rt.find("delete-channel-option").is_none());
        click(&mut rt,"copy-channel");
        for last in ["dialog-close", "confirm-delete-channel"] {
            click(&mut rt,&format!("channel-options-{target}"));
            click(&mut rt,"delete-channel-option");
            assert!(rx.try_recv().is_err());
            click(&mut rt,last);
        }
        assert!(matches!(rx.try_recv(),Ok(Command::Action { action:Action::DeleteChannel { channel }, .. }) if channel == target));
    }

    #[test]
    fn delete_failure_keeps_target_and_retry_success_closes_dialog() {
        let mut app = App::new(); menu_scene(&mut app);
        let target = app.channel_actions.menu.take().unwrap();
        app.channel_actions.target = Some(target.clone()); app.dialog = Some(Dialog::DeleteChannel);
        let (commands, rx) = mpsc::sync_channel(2); app.commands = Some(commands);
        let (updates, inbox) = mpsc::channel(); app.updates = Some(inbox);
        app.confirm_channel_delete(); app.confirm_channel_delete();
        assert_eq!(rx.try_iter().count(),1);
        updates.send(Update::Done(None,Err("denied".into()))).unwrap();
        poll(&mut app,0.,&mut Tx::default());
        assert!(!app.pending && app.dialog == Some(Dialog::DeleteChannel));
        assert!(app.snapshot.channels.contains(&target) && app.error.as_ref().unwrap().contains("denied"));
        app.confirm_channel_delete();
        updates.send(Update::Done(None,Ok(()))).unwrap(); poll(&mut app,0.,&mut Tx::default());
        assert!(app.dialog.is_none() && app.error.is_none() && !app.snapshot.channels.contains(&target));
        assert!(app.notice.unwrap().contains(&target));
    }

    #[test]
    fn deletion_event_invalidates_only_its_channel() {
        let mut app = App::new(); menu_scene(&mut app);
        let target = app.channel_actions.menu.clone().unwrap();
        app.board_cache = Some(app.snapshot.clone());
        app.board_extra = (app.reactions.clone(),app.schedules.clone());
        app.navigate(Page::Channel(target.clone())); app.open_thread(1);
        app.reply_labels.insert(1,edit("stale")); app.reply_drafts.insert(2,edit("keep"));
        let (tx,rx) = mpsc::channel(); app.updates = Some(rx);
        tx.send(Update::Event(neochat::protocol::Event { cursor:101,created_at:0,actor:"local".into(),kind: EventKind::ChannelDeleted {channel:target.clone()} })).unwrap();
        poll(&mut app,0.,&mut Tx::default());
        assert_eq!(app.page, Page::Activity); assert_ne!(app.channel.text,target); assert_eq!(app.thread,None);
        assert!(!app.board_cache.unwrap().channels.contains(&target)); assert!(!app.snapshot.channels.contains(&target));
        assert!(!app.positions.contains_key(&100)); assert!(app.positions.contains_key(&2));
        assert!(app.attachments.is_empty() && app.reactions.is_empty() && app.board_extra.0.is_empty() && app.reply_labels.is_empty());
        assert!(app.reply_drafts.contains_key(&2)); assert!(!app.schedules.is_empty());
    }
}