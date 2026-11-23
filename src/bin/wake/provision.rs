use neochat::{client::Client, invite_file::InviteFile, protocol::{Grant,InviteRequest}};
use sha2::{Digest,Sha256};
use std::{io::Write,path::{Path,PathBuf}};

/// Only the explicit local-host Prepare action can mint a credential. Polling and runtime dispatch never use admin access.
pub fn local(dir:&Path,data:&Path,expected_endpoint:Option<&str>,key:&str,project:&str,harness:&str)->Result<PathBuf,String>{
    if !data.is_absolute() || !data.is_dir(){return Err("Choose the selected local board's data directory".into());}
    let host=Client::discover_admin(Some(data)).map_err(|_|"Local board admin unavailable; provide a scoped invite instead")?;
    let url=reqwest::Url::parse(host.endpoint()).map_err(|_|"Invalid local board endpoint")?;
    if expected_endpoint.is_some_and(|expected| expected != host.endpoint()) {return Err("Local discovery changed. Reconnect the board before creating an agent identity.".into());}
    if !url.host_str().unwrap_or("").trim_matches(['[',']']).parse::<std::net::IpAddr>().is_ok_and(|ip|ip.is_loopback()) || url.path()!="/" {return Err("Can only create an agent identity on the selected loopback board".into());}
    let path=dir.join(format!("agent-{:x}.json",Sha256::digest(key.as_bytes())));
    if path.exists(){let (origin,_)=super::inbox::identify(&path,project)?;if origin!=url.origin().ascii_serialization(){return Err("Existing agent credential belongs to another daemon; not replacing it".into());}return Ok(path);}
    let actor=format!("{harness}-{}",&super::nonce()[..10]);
    let expires_at=super::now()+365*24*3600;
    use Grant::*;
    let invite=host.restricted_invite(&InviteRequest{actor,channels:vec![format!("{project}/*")],expires_at:Some(expires_at)}, &[Read,Notifications,Reply,PostTask,Assign,Claim,Start,Renew,Complete,Block,Fail]).map_err(|_|"Could not create scoped agent identity")?;
    let id=invite.id;
    let record=InviteFile{version:1,project:Some(project.into()),endpoint:url.origin().ascii_serialization(),token:invite.token,expires_at};
    let saved=(||->Option<()>{let mut f=tempfile::NamedTempFile::new_in(dir).ok()?;serde_json::to_writer(&mut f,&record).ok()?;f.flush().and_then(|_|f.as_file().sync_all()).ok()?;f.persist_noclobber(&path).ok()?;Some(())})();
    if saved.is_none(){host.revoke_invite(id).map_err(|_|"Credential save and revocation failed; inspect local invite grants")?;return Err("Could not save private credential; grant revoked".into());} Ok(path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn changed_discovery_refused_before_provisioning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("endpoint.json"), r#"{"url":"http://127.0.0.1:1","token":"fixture"}"#).unwrap();
        std::fs::write(dir.path().join("admin-token"), "fixture").unwrap();
        let error = super::local(dir.path(), dir.path(), Some("http://127.0.0.1:2/"), "board", "Team", "neoism").unwrap_err();
        assert!(error.contains("discovery changed"));
    }
}