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
        // Two grants on one endpoint may have different actors and channel scopes.
        let existing = self.0.iter_mut().find(|p| p.project == project && p.invite == invite && (invite.is_some() || p.endpoint == endpoint));
        if let Some(p) = existing {
            if endpoint.is_some() { p.endpoint = endpoint; }
        } else {
            self.0.push(BoardProfile { project, invite, endpoint });
        }
    }
}

impl BoardProfile {
    pub fn location(&self, index: usize) -> String {
        if self.invite.is_none() && self.endpoint.is_none() { return "Local".into(); }
        let endpoint = self.endpoint.as_deref().and_then(|s| neochat::invite_file::https_endpoint(s).ok());
        format!("Host {} · {}", index + 1, endpoint.as_deref().map(|s| s.trim_start_matches("https://").trim_end_matches('/')).unwrap_or("Saved connection"))
    }

    pub fn on_server(&self, opts: &crate::Options) -> bool {
        if self.invite.is_some() { self.invite == opts.invite }
        else { opts.invite.is_none() && self.endpoint == opts.endpoint }
    }
}

impl crate::App {
    /// Both pickers use saved connection references plus the current server's discovered namespaces.
    pub fn board_rows(&self) -> Vec<(&str, Option<(usize, &BoardProfile)>, bool)> {
        let profiles = self.saved_boards.entries();
        profiles.iter().enumerate().map(|(i,p)| (p.project.as_str(), Some((i,p)), p.on_server(&self.options)))
            .chain(self.projects().into_iter().filter(|id| !profiles.iter().any(|p| p.on_server(&self.options) && p.project == *id)).map(|id| (id, None, true))).collect()
    }
}