use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::{fs::File, io::Read, path::Path};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InviteFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub version: u8,
    pub endpoint: String,
    pub token: String,
    pub expires_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_link_keeps_credentials_out_of_request_url_and_rejects_bad_links() {
        let invite = InviteFile { project: Some("team".into()), version: 1, endpoint: "https://board.example/".into(),
            token: "private+key&value".into(), expires_at: crate::board::now() + 300 };
        let link = invite.to_link().unwrap();
        let parsed = InviteFile::from_link(&link).unwrap();
        assert_eq!(parsed.token, invite.token);
        assert_eq!(parsed.project, invite.project);
        assert_eq!(parsed.endpoint, invite.endpoint);
        assert!(Url::parse(&link).unwrap().query().is_none());
        assert!(InviteFile::from_link(&format!("{link}&key=duplicate")).is_err());
        assert!(InviteFile::from_link(&link.replacen("https:", "http:", 1)).is_err());
        assert!(InviteFile::from_link(&link.replacen(&invite.expires_at.to_string(), "1", 1)).is_err());
    }
}

pub fn https_endpoint(endpoint: &str) -> Result<String, &'static str> {
    let url = Url::parse(endpoint).map_err(|_| "invalid HTTPS endpoint")?;
    if url.scheme() != "https" || url.host_str().is_none() || !url.username().is_empty()
        || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err("the sharing endpoint must be an HTTPS URL without credentials, query or fragment");
    }
    Ok(url.to_string())
}

impl InviteFile {
    /// Keep the bearer credential in the URL fragment, which HTTP never sends.
    pub fn to_link(&self) -> Result<String, String> {
        let project = self.project.as_deref().filter(|p| crate::valid_project(p)).ok_or("Invite links require a board scope.")?;
        let mut url = Url::parse(&https_endpoint(&self.endpoint)?).map_err(|_| "Invalid invite endpoint.")?;
        url.query_pairs_mut().append_pair("neoboard", project).append_pair("expires", &self.expires_at.to_string()).append_pair("key", &self.token);
        let fields = url.query().map(str::to_owned);
        url.set_query(None);
        url.set_fragment(fields.as_deref());
        Ok(url.to_string())
    }

    pub fn from_link(link: &str) -> Result<Self, String> {
        if link.len() > 8192 { return Err("Invite link is too large.".into()); }
        let mut endpoint = Url::parse(link.trim()).map_err(|_| "Invalid NeoBoard invite link.")?;
        let mut project = None;
        let mut expires_at = None;
        let mut token = None;
        if endpoint.query().is_some() { return Err("Invite links must not contain a query.".into()); }
        let mut fields = endpoint.clone();
        fields.set_query(endpoint.fragment());
        for (key, value) in fields.query_pairs() {
            match key.as_ref() {
                "neoboard" if project.is_none() => project = Some(value.into_owned()),
                "expires" if expires_at.is_none() => expires_at = Some(value.parse().map_err(|_| "Invalid invite expiry.")?),
                "key" if token.is_none() => token = Some(value.into_owned()),
                _ => return Err("Invalid NeoBoard invite fields.".into()),
            }
        }
        endpoint.set_fragment(None);
        Self { project: Some(project.ok_or("Invite link is missing its board.")?), version: 1,
            endpoint: endpoint.to_string(), token: token.ok_or("Invite link is missing its key.")?,
            expires_at: expires_at.ok_or("Invite link is missing its expiry.")? }.validate()
    }

    pub fn store(&self, directory: &Path) -> Result<std::path::PathBuf, String> {
        use std::io::Write;
        std::fs::create_dir_all(directory).map_err(|_| "Couldn't create the local invite directory.")?;
        let path = directory.join(format!("{}.neochat", crate::board::random_token()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let data = serde_json::to_vec(self).map_err(|_| "Couldn't encode the invite.")?;
        let mut file = options.open(&path).map_err(|_| "Couldn't save the invite locally.")?;
        if file.write_all(&data).and_then(|_| file.sync_all()).is_err() {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err("Couldn't save the invite locally.".into());
        }
        Ok(path)
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() || !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
            return Err("Choose a regular invite file by its absolute path.".into());
        }
        let file = File::open(path).map_err(|_| "Could not open the invite file.")?;
        if !file.metadata().is_ok_and(|m| m.is_file()) { return Err("Expected a regular invite file.".into()); }
        let mut data = Vec::new();
        file.take(8193).read_to_end(&mut data).map_err(|_| "Could not read the invite file.")?;
        if data.len() > 8192 { return Err("Invite file is too large.".into()); }
        serde_json::from_slice::<Self>(&data).map_err(|_| "Invalid Neoboard invite file.")?.validate()
    }

    fn validate(mut self) -> Result<Self, String> {
        if self.version != 1 { return Err("Unsupported Neoboard invite version.".into()); }
        if self.expires_at <= crate::board::now() { return Err("This invite has expired.".into()); }
        if self.project.as_deref().is_some_and(|id| !crate::valid_project(id)) { return Err("Invalid board in invite.".into()); }
        if self.token.is_empty() || self.token.len() > 512 || !self.token.bytes().all(|c| c.is_ascii_graphic()) {
            return Err("Invalid invite key.".into());
        }
        self.endpoint = https_endpoint(&self.endpoint)?;
        Ok(self)
    }
}