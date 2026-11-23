use super::*;
use saved_boards::BoardProfile;

#[derive(Clone)]
pub enum Target { Host(String), Saved(BoardProfile) }

impl Target {
    pub fn name(&self) -> &str {
        match self { Self::Host(id) => id, Self::Saved(p) => &p.project }
    }
}

pub struct Confirmation {
    pub target: Target,
    pub typed: TextEdit,
    pub sending: bool,
    pub error: Option<String>,
}

impl App {
    pub fn open_removal(&mut self, target: Target) {
        if self.onboarding.busy || self.removal.as_ref().is_some_and(|r| r.sending) { return; }
        self.project_menu = false;
        self.dialog = Some(Dialog::RemoveBoard);
        self.removal = Some(Confirmation { target, typed: edit(""), sending: false, error: None });
    }

    fn active_profile(&self, profile: &BoardProfile) -> bool {
        self.project == profile.project && self.options.endpoint.is_none() && self.options.invite == profile.invite
    }

    fn draft_fields(&self) -> impl Iterator<Item=(&str, &str)> {
        [("Post", &self.draft), ("Reply", &self.reply), ("Labels", &self.labels), ("Dependencies", &self.needs),
         ("Assignee", &self.assignee), ("Attachment", &self.compose_file), ("Task title", &self.task_title),
         ("Task description", &self.task_description), ("Task labels", &self.task_labels), ("New channel", &self.new_channel),
         ("Scheduled for", &self.schedule_at)].into_iter().map(|(name, edit)| (name, edit.text.as_str()))
            .chain(self.sprint.draft_fields()).filter(|(_, value)| !value.is_empty())
    }

    pub fn removal_has_drafts(&self) -> bool {
        self.compose_file_open || self.task_file.is_some() || self.draft_fields().next().is_some()
            || self.reply_drafts.values().chain(self.reply_labels.values()).any(|edit| !edit.text.is_empty())
    }

    // After revocation a reply's thread can no longer be reopened. Let the user
    // recover their own text without retaining or redisplaying the thread.
    pub fn local_drafts_text(&self) -> String {
        let mut fields: Vec<_> = self.draft_fields().map(|(name,value)| format!("{name}: {value}")).collect();
        for (kind, drafts) in [("Reply", &self.reply_drafts), ("Reply labels", &self.reply_labels)] {
            for (id,edit) in drafts { if !edit.text.is_empty() { fields.push(format!("{kind} #{id}: {}", edit.text)); } }
        }
        if let Some(file) = &self.task_file { fields.push(format!("Task attachment: {}", file.display())); }
        fields.join("\n\n")
    }

    pub fn removal_blocked(&self, r: &Confirmation) -> Option<&'static str> {
        if self.pending || self.onboarding.busy { return Some("Wait for the pending change to finish."); }
        let could_drop_work = match &r.target {
            Target::Host(id) => {
                if id == "general" { return Some("The default general board is protected."); }
                if self.status != "live" || !self.can_share_board(id) { return Some("Open a live connection to the local host to delete this board."); }
                true
            }
            Target::Saved(profile) => {
                if !self.saved_boards.entries().contains(profile) { return Some("This saved connection has changed. Reopen the board picker."); }
                self.active_profile(profile)
            }
        };
        (could_drop_work && self.removal_has_drafts()).then_some("Finish or clear your drafts before removing this board.")
    }

    pub fn confirm_board_removal(&mut self) {
        let Some(r) = &self.removal else { return; };
        if self.dialog != Some(Dialog::RemoveBoard) || r.sending || self.removal_blocked(r).is_some() { return; }
        match r.target.clone() {
            Target::Host(id) => {
                if r.typed.text != id { return; }
                let r = self.removal.as_mut().unwrap();
                r.sending = self.commands.as_ref().is_some_and(|tx| tx.try_send(Command::DeleteBoard(id)).is_ok());
                if !r.sending { r.error = Some("The board worker isn't ready. No request was sent.".into()); }
            }
            Target::Saved(profile) => {
                let active = self.active_profile(&profile);
                self.saved_boards.forget(&profile);
                self.close_dialog();
                if active {
                    // Drop both halves before clearing the view. No late remote snapshot
                    // or completion can be consumed by the next connection.
                    self.commands = None;
                    self.updates = None;
                    self.clear_live_board();
                    self.clear_board_selection();
                    self.project.clear(); self.options.project = None; self.options.invite = None;
                    self.status = "connecting".into();
                    if let Some(wake) = self.connection_wake.clone() {
                        let (tx, rx) = connection::start(self.options.clone(), wake);
                        self.commands = Some(tx); self.updates = Some(rx);
                    }
                }
                self.save_preferences();
            }
        }
    }

    pub fn finish_board_delete(&mut self, id: &str, ok: bool) {
        let Some(r) = &mut self.removal else { return; };
        if !r.sending || !matches!(&r.target, Target::Host(name) if name == id) { return; }
        r.sending = false;
        if !ok {
            r.typed.clear();
            r.error = Some("Deletion wasn't confirmed. Check the board before trying again.".into());
            return;
        }
        self.close_dialog();
        let channels: Vec<_> = self.board_cache.as_ref().unwrap_or(&self.snapshot).channels.iter()
            .filter(|c| projects::contains(id, c)).cloned().collect();
        for c in channels { self.invalidate_channel(&c); }
        self.organization.areas.remove(id); self.organization.area_owners.remove(id);
        self.organization.sprints.retain(|_, s| s.board != id);
        self.organization.task_sprints.retain(|_, sprint| self.organization.sprints.contains_key(sprint));
        if let Some(local) = self.saved_boards.entries().iter().find(|p| p.project == id && p.invite.is_none()).cloned() {
            self.saved_boards.forget(&local);
        }
        if self.project == id {
            self.project.clear(); self.options.project = None;
            self.clear_board_selection();
            self.member_list = None; self.aliases.clear(); self.setup_session = Default::default();
            self.resolve_project(); self.refresh_project(); self.refresh_extra(); self.default_channel();
            self.navigate(Page::Home);
        }
        self.save_preferences();
    }

    // Only after a confirmed active-board removal with no unsaved drafts.
    fn clear_board_selection(&mut self) {
        self.reply_owner = None; self.reply.clear(); self.reply_labels.clear(); self.reply_drafts.clear();
        self.sprint = Default::default(); self.clear_task_draft(); self.task_filters = Default::default();
        self.query.clear(); self.filter = ALL_TASKS; self.details_picker = None; self.pick_assignee = false;
        self.compose_file_owner = None; self.confirm_parent_completion = None; self.channel_actions = Default::default();
        self.show_schedule = false; self.attachment_message = None; self.download_id = None;
        self.upload_path.clear(); self.download_path.clear(); self.task_menu = None; self.share = Default::default();
    }

    /// Drop fetched content, not the user's draft buffers or saved connection.
    fn clear_live_board(&mut self) {
        self.personal.reset_connection();
        self.board_cache = None;
        self.snapshot = Snapshot { cursor: 0, channels: vec![], messages: vec![] };
        self.board_extra = Default::default(); self.positions.clear(); self.seen.clear();
        self.reactions.clear(); self.schedules.clear(); self.attachments.clear();
        self.organization = Default::default(); self.aliases.clear(); self.member_list = None; self.me = None;
        self.attachment_message = None; self.download_id = None; self.task_menu = None;
        self.channel_actions = Default::default(); self.share = Default::default(); self.setup_session = Default::default();
        self.channel.clear(); self.error = None; self.notice = None;
        self.navigate(Page::Home);
    }

    pub fn board_access_denied(&mut self) {
        if !self.remote() || self.status == "access denied" { return; }
        self.clear_live_board();
        self.pending = false; self.creating_task = false; self.creating_channel = None;
        if self.dialog != Some(Dialog::RemoveBoard) { self.close_dialog(); }
        self.status = "access denied".into();
        self.error = Some("This connection no longer has access to the board.".into());
    }
}

pub fn scene(app: &mut App) {
    fixture::load(app, false);
    app.status = "live".into();
    app.open_removal(Target::Host(app.project.clone()));
}

pub fn guest_scene(app: &mut App) {
    fixture::saved_boards(app);
    app.open_removal(Target::Saved(app.saved_boards.entries()[1].clone()));
}

#[cfg(test)]
mod tests;