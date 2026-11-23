//! A device preference, not a posting profile or a board credential. Missing means
//! “leave existing board names alone”; a saved empty string explicitly clears them.
use std::{io::{self, Write}, path::Path};
use super::*;

#[derive(Default)]
pub struct Personal {
    path: Option<std::path::PathBuf>,
    pub desired: Option<String>,
    pub session: Option<u64>,
    pub revision: u64,
    confirmed: Option<Result<(), String>>,
}

impl Personal {
    pub fn reset_connection(&mut self) { self.session = None; self.confirmed = None; }
}

fn normalized(raw: &str) -> Result<&str, &'static str> {
    neochat::protocol::validate_display_name(raw)?;
    Ok(raw.trim())
}

#[cfg(test)]
mod tests;

fn load(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let name: String = serde_json::from_slice(&bytes)?;
            Ok(Some(normalized(&name).map_err(io::Error::other)?.into()))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn store(path: &Path, name: &str) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("Missing settings directory"))?;
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(&serde_json::to_vec(name)?)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)?;
    Ok(())
}

impl App {
    pub fn load_personal(&mut self) {
        self.personal.path = preferences_path().map(|p| p.with_file_name("personal-name.json"));
        self.restore_name();
    }

    fn restore_name(&mut self) {
        if let Some(path) = &self.personal.path {
            match load(path) {
                Ok(name) => self.personal.desired = name,
                Err(e) => self.error = Some(format!("Could not read your device display name: {e}")),
            }
        }
        self.onboarding.name = edit(self.personal.desired.as_deref().unwrap_or(""));
    }

    pub fn device_label(&self) -> &str {
        self.personal.desired.as_deref().or_else(|| self.me.as_ref()?.display_name.as_deref())
            .filter(|s| !s.is_empty()).unwrap_or(&self.local_name)
    }

    pub fn open_personal(&mut self) {
        self.personal_name = edit(self.saved_field_value());
        self.error = None;
    }

    fn saved_field_value(&self) -> &str {
        self.personal.desired.as_deref().or_else(|| self.me.as_ref()?.display_name.as_deref()).unwrap_or("")
    }

    pub fn name_field_status(&self) -> String {
        if self.personal_name.text.trim() != self.saved_field_value() { "Unsaved change on this device.".into() }
        else { self.name_status() }
    }

    pub fn save_name(&mut self, raw: &str) -> Result<(), String> {
        let name = normalized(raw)?;
        if let Some(path) = &self.personal.path {
            store(path, name).map_err(|e| format!("Could not save your device display name: {e}"))?;
        } else if !self.fixture && !cfg!(test) { return Err("No settings directory available".into()); }
        self.personal.desired = Some(name.into());
        self.personal.revision += 1;
        self.sync_name();
        Ok(())
    }

    pub fn sync_name(&mut self) {
        self.personal.confirmed = None;
        let (Some(session), Some(name)) = (self.personal.session, &self.personal.desired) else { return };
        if self.status != "live" || self.project.is_empty() { return; }
        let command = Command::Personal { session, revision: self.personal.revision, name: name.clone() };
        if !self.commands.as_ref().is_some_and(|tx| tx.try_send(command).is_ok()) {
            self.personal.confirmed = Some(Err("Connection busy; save again to retry.".into()));
        }
    }

    pub fn name_result(&mut self, session: u64, revision: u64, result: Result<neochat::protocol::Member, String>) {
        if self.personal.session != Some(session) || revision != self.personal.revision { return; }
        self.personal.confirmed = Some(result.map(|member| { self.aliases.remove(&member.actor); self.me = Some(member); }));
    }

    pub fn name_status(&self) -> String {
        if self.personal.desired.is_none() { return "No device preference yet. Existing board names are unchanged.".into(); }
        if self.status == "access denied" { return "Saved on this device. This connection no longer has board access.".into(); }
        if self.status != "live" { return "Saved on this device. Board name not verified while offline.".into(); }
        match &self.personal.confirmed {
            Some(Ok(())) => "Saved on this device and confirmed on this board.".into(),
            Some(Err(e)) => format!("Saved on this device. Board name not confirmed: {e}"),
            None => "Saved on this device. Waiting for board confirmation.".into(),
        }
    }
}