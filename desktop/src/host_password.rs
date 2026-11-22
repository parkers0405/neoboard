use super::*;

#[derive(Default)]
pub enum Panel {
    #[default]
    Closed,
    Edit { new: TextEdit, repeat: TextEdit, error: Option<&'static str> },
    ConfirmSet(connection::JoinPassword),
    ConfirmDisable,
    Saving { disable: bool },
    Notice(&'static str),
}

impl App {
    pub fn password_controls_available(&self) -> bool {
        matches!(self.dialog, Some(Dialog::Connection | Dialog::Share)) && self.status == "live"
            && self.can_share_board(&self.share.board) && !self.share.busy && !self.share.confirm_stop
    }

    pub fn edit_host_password(&mut self) {
        if !self.password_controls_available() { return; }
        self.share.password = Panel::Edit { new: edit("").secure(), repeat: edit("").secure(), error: None };
    }

    // Build a password-join address from the route, never from a bearer invite.
    pub fn password_address(&self) -> Option<String> {
        if !self.password_controls_available() || !neochat::valid_project(&self.share.board) { return None; }
        let state = self.share.result.as_ref()?.as_ref().ok()?;
        if state.route != neochat::sharing::RouteState::Connected { return None; }
        let base = neochat::invite_file::https_endpoint(state.endpoint.as_deref()?).ok()?;
        Some(format!("{base}#neoboard={}", self.share.board))
    }

    pub fn review_password(&mut self) {
        if !self.password_controls_available() { return; }
        let Panel::Edit { new, repeat, error } = &mut self.share.password else { return; };
        *error = if new.text.is_empty() || new.text.len() > neochat::client::MAX_BOARD_PASSWORD_BYTES {
            Some("Enter a password of 1 to 1024 UTF-8 bytes.")
        } else if new.text != repeat.text { Some("The passwords don’t match. Enter them again.") } else { None };
        if error.is_some() { new.clear(); repeat.clear(); return; }
        self.share.password = Panel::ConfirmSet(connection::JoinPassword::new(std::mem::take(&mut new.text)));
    }

    pub fn confirm_password(&mut self) {
        if !self.password_controls_available() { return; }
        let password = match std::mem::take(&mut self.share.password) {
            Panel::ConfirmSet(secret) => Some(secret), Panel::ConfirmDisable => None,
            other => { self.share.password = other; return; }
        };
        let disable = password.is_none();
        self.share.request += 1;
        let cmd = Command::BoardPassword { id: self.share.board.clone(), request: self.share.request, password };
        self.share.password = if self.commands.as_ref().is_some_and(|tx| tx.try_send(cmd).is_ok()) {
            Panel::Saving { disable }
        } else { Panel::Notice("Password change was not sent. Try again when connected.") };
    }

    pub fn finish_password(&mut self, request: u64, ok: bool) {
        if request != self.share.request || !self.password_controls_available() { return; }
        if let Panel::Saving { disable } = self.share.password {
            self.share.password = Panel::Notice(if !ok { "Could not confirm the password change. Check the local host before retrying." }
                else if disable { "Password joins disabled. Existing members keep access." }
                else { "Board password set. Hosting has not changed." });
        }
    }

    pub fn close_dialog(&mut self) {
        if self.removal.as_ref().is_some_and(|r| r.sending) { return; }
        self.removal = None;
        self.share.password = Panel::Closed;
        self.dialog = None;
    }
}