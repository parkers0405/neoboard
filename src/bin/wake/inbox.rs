use super::store::Source;
#[cfg(test)]
#[path = "inbox_tests.rs"]
mod tests;
use neochat::{client::InboxPage, invite_file::InviteFile};
use std::{fs::File,io::Read,path::Path,time::Duration};

fn access(path:&Path,project:&str)->Result<(String,String),String> {
    if !path.is_absolute() || !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {return Err("Choose a saved, regular scoped invite file".into());}
    let mut bytes=Vec::new(); File::open(path).and_then(|f|f.take(8193).read_to_end(&mut bytes)).map_err(|_|"Cannot open scoped invite")?;
    let inv:InviteFile=serde_json::from_slice(&bytes).map_err(|_|"Invalid scoped invite")?;
    if bytes.len()>8192 || inv.version!=1 || inv.expires_at<=neochat::board::now() || inv.project.as_deref()!=Some(project) || inv.token.is_empty() || inv.token.len()>512 || !inv.token.bytes().all(|b| b.is_ascii_graphic()) {return Err("Expected an unexpired invite scoped to the selected board".into());}
    let u=reqwest::Url::parse(&inv.endpoint).map_err(|_|"Invalid board origin")?;
    let local=u.host_str().unwrap_or("").trim_matches(['[',']']).parse::<std::net::IpAddr>().is_ok_and(|ip|ip.is_loopback());
    if u.host().is_none() || !(u.scheme()=="https" || u.scheme()=="http" && local) || !u.username().is_empty() || u.password().is_some() || u.path()!="/" || u.query().is_some() || u.fragment().is_some() {return Err("Use an HTTPS board origin (or numeric loopback HTTP)".into());}
    Ok((u.origin().ascii_serialization(),inv.token))
}
fn page(origin:&str,token:&str,after:u64)->Result<InboxPage,String>{
    let c=reqwest::blocking::Client::builder().redirect(reqwest::redirect::Policy::none()).no_proxy().timeout(Duration::from_secs(8)).build().map_err(|_|"Cannot initialize inbox client")?;
    let response=c.get(format!("{origin}/v1/inbox")).query(&[("after",after.to_string()),("read","false".to_string()),("limit","1".to_string())]).bearer_auth(token).send().map_err(|_|"Board unreachable; inbox not delivered")?;
    if !response.status().is_success(){return Err(format!("Inbox unavailable (HTTP {}); check board, invite and grants",response.status().as_u16()));}
    let mut bytes=Vec::new(); response.take(1024*1024+1).read_to_end(&mut bytes).map_err(|_|"Cannot read inbox")?;
    if bytes.len()>1024*1024{return Err("Oversized inbox response".into());}
    let p:InboxPage=serde_json::from_slice(&bytes).map_err(|_|"Board does not support the expected inbox response")?;
    if p.items.len()>1 || !neochat::valid_project(&p.actor) || matches!(p.actor.as_str(),"local"|"system") {return Err("A dedicated agent credential is required; do not use host credentials".into());}
    Ok(p)
}
/// A local board listens on a new port after every launch. Follow it only through this
/// user's own discovery file, and only once the saved grant authenticates there as the
/// same actor. Returns the origin to adopt, or None when nothing should change.
pub fn relocated(s:&Source)->Option<String>{
    let (saved,token)=access(&s.invite,&s.project).ok()?;
    if !s.origin.starts_with("http://"){return None;}
    let found=neochat::client::Client::discover(s.local.as_deref(),None).ok()?;
    let u=reqwest::Url::parse(found.endpoint()).ok()?;
    if u.scheme()!="http" || u.path()!="/" || !u.host_str().unwrap_or("").trim_matches(['[',']']).parse::<std::net::IpAddr>().is_ok_and(|ip|ip.is_loopback()){return None;}
    let origin=u.origin().ascii_serialization();
    if origin==s.origin && origin==saved{return None;}
    (page(&origin,&token,0).ok()?.actor==s.actor).then_some(origin)
}
/// Point the saved grant at the board's new address, keeping its credential and expiry.
pub fn readdress(path:&Path,origin:&str)->Result<(),String>{
    let mut bytes=Vec::new(); File::open(path).and_then(|f|f.take(8193).read_to_end(&mut bytes)).map_err(|_|"Cannot open scoped invite")?;
    let mut inv:InviteFile=serde_json::from_slice(&bytes).map_err(|_|"Invalid scoped invite")?;
    inv.endpoint=origin.into();
    inv.replace(path)
}
pub fn identify(path:&Path, project:&str)->Result<(String,String),String>{let (origin,token)=access(path,project)?;let actor=page(&origin,&token,0)?.actor;Ok((origin,actor))}
pub(super) fn saved_origin(path:&Path, project:&str)->Result<String,String>{access(path,project).map(|(origin,_)|origin)}
pub fn poll(s:&Source, after:u64)->Result<InboxPage,String>{
    let (origin,token)=access(&s.invite,&s.project)?;if origin!=s.origin{return Err("Saved invite changed server; wake stopped".into());}
    let p=page(&origin,&token,after)?;if p.actor!=s.actor{return Err("Saved invite changed actor; wake stopped".into());} Ok(p)
}