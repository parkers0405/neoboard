use std::path::PathBuf;

/// Preferences contain connection references, never an invite link or bearer token.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BoardProfile {
    pub project: String,
    pub invite: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SavedBoards(Vec<BoardProfile>);

impl SavedBoards {
    pub fn entries(&self) -> &[BoardProfile] { &self.0 }

    /// Forget only this preference. Other consumers may reference the same invite file.
    pub fn forget(&mut self, profile: &BoardProfile) { self.0.retain(|p| p != profile); }

    pub fn remember(&mut self, project: String, invite: Option<PathBuf>, endpoint: Option<String>) {
        let existing = self.0.iter_mut().find(|p| p.project == project && (p.invite == invite
            || (p.invite.is_some() && invite.is_some() && endpoint.is_some() && p.endpoint == endpoint)));
        if let Some(p) = existing {
            // Renewing an invite on the same endpoint replaces only its credential reference.
            p.invite = invite;
            if endpoint.is_some() { p.endpoint = endpoint; }
        } else {
            self.0.push(BoardProfile { project, invite, endpoint });
        }
    }
}