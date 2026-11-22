//! The blocking shared client never runs on Rune's thread. Writes are not retried:
//! a lost response is ambiguous, so keep the draft and let the user inspect the board.
use std::{path::{Path, PathBuf}, sync::mpsc, time::Duration};
use neochat::{client::{Client, connect_local}, protocol::{Action, Snapshot, Event, Reaction, ScheduledPost, Attachment, MAX_ATTACHMENT_BYTES}};

#[derive(Clone, Default)]
pub struct Options {
    pub endpoint: Option<String>,
    pub project: Option<String>,
    pub invite: Option<PathBuf>,
    pub token: String,
    pub profile: Option<String>,
    pub data_dir: Option<PathBuf>,
    pub fixture: bool,
}

impl Options {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut opts = Self { token: std::env::var("NEOCHAT_TOKEN").unwrap_or_default(), ..Self::default() };
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            match arg.as_str() {
                "--endpoint" | "--token" | "--profile" | "--data-dir" | "--project" => {
                    i += 1;
                    let value = args.get(i).ok_or_else(|| format!("{arg} needs a value"))?.clone();
                    match arg.as_str() {
                        "--endpoint" => opts.endpoint = Some(value),
                         "--project" if neochat::valid_project(&value) => opts.project = Some(value),
                         "--project" => return Err("Invalid board id".into()),
                        "--token" => opts.token = value,
                        "--profile" => opts.profile = Some(value),
                        _ => opts.data_dir = Some(value.into()),
                    }
                }
                "--fixture" | "--snap" | "--tree" | "--scene" => opts.fixture = true,
                _ => {} // Rune owns --size, --scale, --snap, --scene, --script, etc.
            }
            i += 1;
        }
        if opts.profile.is_none() { opts.profile = Some(crate::profile::local_name(opts.fixture)); }
        crate::profile::validate_posting_name(opts.profile.as_deref().unwrap())?;
        Ok(opts)
    }

    fn connect(&self) -> Result<Client, String> {
        if let Some(path) = &self.invite {
            let invite = neochat::invite_file::InviteFile::read(path)?;
            Client::new(&invite.endpoint, &invite.token, self.profile.as_deref()).map_err(|e| e.to_string())
        } else if let Some(endpoint) = &self.endpoint {
            Client::new(endpoint, &self.token, self.profile.as_deref()).map_err(|e| e.to_string())
        } else {
            connect_local(self.data_dir.as_deref(), self.profile.as_deref()).map_err(|e| e.to_string())
        }.map(|c|c.with_sender("desktop"))
    }
}

pub enum Command {
    CreateBoard { id: String, name: String },
    JoinBoard { invite: JoinSource, name: String },
    Host { id: String, request: u64, operation: HostOp },
    BoardPassword { id: String, request: u64, password: Option<JoinPassword> },
    SwitchBoard { id: String, path: Option<PathBuf> },
    Members(String),
    Personal(String),
    PostWithFile { action:Action, draft:Option<(Option<u64>,String)>, path:PathBuf },
    Action { action: Action, draft: Option<(Option<u64>, String)> },
    Attachments(u64),
    Upload { message: u64, path: PathBuf },
    Download { id: u64, path: PathBuf },
}
pub struct HostedBoard { pub path: PathBuf, pub endpoint: String }
pub type BoardSetup = (String, Snapshot, neochat::protocol::Member, Option<HostedBoard>);
// Deliberately neither serializable nor Debug. The worker drops the password after exchange.
pub struct JoinPassword(zeroize::Zeroizing<String>);
impl JoinPassword {
    pub fn new(value: String) -> Self { Self(zeroize::Zeroizing::new(value)) }
}

pub enum JoinSource {
    File(PathBuf), Link(neochat::invite_file::InviteFile),
    Password { endpoint: String, project: String, password: JoinPassword },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HostOp { Inspect, Create, Stop }

pub struct HostState { pub route: neochat::sharing::RouteState, pub link: Option<String>, pub endpoint: Option<String> }

// Host administration never uses the current session's bearer or an explicit endpoint,
// even if the latter points to localhost. Discovery itself performs no network I/O.
fn change_password(opts: &Options, id: &str, password: Option<JoinPassword>) -> bool {
    if opts.fixture || opts.invite.is_some() || opts.endpoint.is_some() || opts.project.as_deref().is_some_and(|p| p != id) { return false; }
    let Ok(admin) = Client::discover_admin(opts.data_dir.as_deref()) else { return false; };
    let local = reqwest::Url::parse(admin.endpoint()).ok().is_some_and(|url| url.host_str().is_some_and(|host|
        host == "localhost" || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())));
    local && admin.set_board_password(id, password.as_ref().map(|p| p.0.as_str())).is_ok()
}

fn host_control(dir: Option<&std::path::Path>, id: &str, op: HostOp) -> Result<HostState, String> {
    use neochat::sharing::{self, RouteState};
    match op {
        HostOp::Stop => sharing::stop_local(dir)?,
        HostOp::Create => {
            let invite = sharing::share_desktop_local(dir, id)?;
            return Ok(HostState { route: RouteState::Connected, link: Some(invite.to_link()?), endpoint: Some(invite.endpoint) });
        }
        HostOp::Inspect => {}
    }
    let status = sharing::status_local(dir).map_err(|e| format!("Couldn't check the host tunnel. If the daemon is older, update and restart neochatd. {e}"))?;
    let link = if status.state == RouteState::Connected {
        sharing::lookup_local(dir, id)?.filter(|invite| invite.project.as_deref() == Some(id)
            && status.endpoint.as_deref().and_then(|e| neochat::invite_file::https_endpoint(e).ok()) == neochat::invite_file::https_endpoint(&invite.endpoint).ok())
            .and_then(|invite| invite.to_link().ok())
    } else { None };
    Ok(HostState { route: status.state, link, endpoint: status.endpoint })
}

pub enum Update {
    BoardSetup(Result<BoardSetup, String>),
    Host(u64, Result<HostState, String>),
    BoardPassword(u64, bool),
    Members(String,Result<Vec<neochat::protocol::Member>,String>),
    Names(String,Vec<neochat::protocol::Member>),
    Personal(Result<neochat::protocol::Member,String>),
    SentFile(PathBuf,String),
    Snapshot(Snapshot),
    Event(Event),
    Status(String),
    Done(Option<(Option<u64>, String)>, Result<(), String>),
    Extra(Result<(Vec<Reaction>, Vec<ScheduledPost>), String>),
    Organization(neochat::protocol::Organization),
    SprintCreated(u64),
    Attachments(u64, Result<Vec<Attachment>, String>),
    FileDone(Result<String, String>),
}

// A bounded read also handles files that grow after metadata was checked.
fn upload_bytes(path: &std::path::Path) -> Result<(String, Vec<u8>), String> {
    use std::io::Read;
    if !path.is_absolute() { return Err("Choose an absolute upload path (no ~ expansion)".into()); }
    let name = path.file_name().and_then(|s| s.to_str()).filter(|s| !s.is_empty())
        .ok_or("Choose a file with a UTF-8 filename")?.to_string();
    if !std::fs::metadata(path).map_err(|e| e.to_string())?.is_file() { return Err("Upload path must be a regular file".into()); }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() { return Err("Upload path must be a regular file".into()); }
    let mut bytes = Vec::new();
    file.take(MAX_ATTACHMENT_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_ATTACHMENT_BYTES { return Err("Attachment exceeds 8 MiB upload limit".into()); }
    Ok((name, bytes))
}

fn save_download(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    if !path.is_absolute() { return Err("Choose an absolute destination file path (no ~ expansion)".into()); }
    // Explicit user path only. Never join an untrusted remote filename or overwrite a file/symlink.
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let mut file = options.open(path).map_err(|e| e.to_string())?;
    file.write_all(bytes).and_then(|_| file.sync_all()).map_err(|e| format!("Save failed (partial file may remain): {e}"))
}

/// The membership this device already holds for the invite's board, if it still works.
fn membership(stored: &std::path::Path, invite: &neochat::invite_file::InviteFile, profile: Option<&str>) -> Option<(neochat::invite_file::InviteFile, PathBuf)> {
    let mut files: Vec<_> = std::fs::read_dir(stored).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "neochat")).collect();
    files.sort();
    files.into_iter().take(256).find_map(|path| {
        let held = neochat::invite_file::InviteFile::read(&path).ok()?;
        if held.project.is_none() || held.project != invite.project || held.endpoint != invite.endpoint || held.token == invite.token { return None; }
        Client::new(&held.endpoint, &held.token, profile).ok()?.my_member().ok()?;
        Some((held, path))
    })
}

struct JoinCredential { invite: neochat::invite_file::InviteFile, path: Option<PathBuf>, personal: bool }

fn kept_invite(path: Option<&Path>, stored: Option<&Path>) -> bool {
    path.and_then(Path::parent).is_some_and(|dir| Some(dir) == stored)
}

fn join_board(source: JoinSource, name: Option<&str>, expected: Option<&str>, profile: Option<&str>) -> Result<BoardSetup, String> {
    let (mut invite, mut path, personal) = match source {
        JoinSource::File(path) => (neochat::invite_file::InviteFile::read(&path)?, Some(path), false),
        JoinSource::Link(invite) => (invite, None, false),
        JoinSource::Password { endpoint, project, password } => {
            let invite = neochat::client::join_with_password(&endpoint, &project, &password.0)
                .map_err(|_| "Could not join. Check the server address, board slug and password.")?;
            if invite.project.as_deref() != Some(&project) || neochat::invite_file::https_endpoint(&invite.endpoint)? != endpoint {
                return Err("The server returned a credential for a different board.".into());
            }
            (invite, None, true)
        }
    };
    let stored = crate::preferences_path().map(|p| p.with_file_name("invites"));
    // The host's link changes every day. Someone opening a new one for a board this device
    // has already joined stays the member they were, with their name and their tasks.
    if !personal && !kept_invite(path.as_deref(), stored.as_deref()) && let Some((held, at)) = stored.as_deref().and_then(|dir| membership(dir, &invite, profile)) {
        invite = held;
        path = Some(at);
    }
    let client = Client::new(&invite.endpoint, &invite.token, profile).map_err(|e| e.to_string())?.with_sender("desktop");
    finish_join(JoinCredential { invite, path, personal }, client, name, expected, profile, stored.as_deref())
}

fn finish_join(credential: JoinCredential, mut client: Client, name: Option<&str>, expected: Option<&str>, profile: Option<&str>, stored: Option<&Path>) -> Result<BoardSetup, String> {
    let JoinCredential { mut invite, mut path, personal } = credential;
    // A remote error body is arbitrary text. Never display it after a password exchange.
    let remote_error = |e: neochat::client::ClientError| if personal { "Could not verify the new board membership.".to_owned() } else { e.to_string() };
    let snapshot = client.snapshot().map_err(remote_error)?;
    let available = crate::projects::available(&snapshot, true);
    let id = match invite.project.clone() {
        Some(id) if available.contains(id.as_str()) => id,
        Some(_) => return Err("This invite does not expose its board.".into()),
        None if available.len() == 1 => available.first().unwrap().to_string(),
        None => return Err("Ask the host for an invite scoped to one board.".into()),
    };
    if expected.is_some_and(|expected| expected != id) { return Err("The invite no longer points to this board.".into()); }
    // One link is opened by several people. Each takes a credential of their own here,
    // or they would all be one member sharing one name. Reconnecting must use it, so it
    // replaces an invite this app stored; a file the user chose keeps the shared invite
    // and the credential goes in a private copy.
    if !personal && let Some(personal) = client.join().map_err(|e| e.to_string())? {
        invite.token = personal.token;
        // The host renews a membership while it is used, so only the host can say it has ended.
        invite.expires_at = i64::MAX as u64;
        if kept_invite(path.as_deref(), stored) { invite.replace(path.as_ref().unwrap())?; } else { path = None; }
        client = Client::new(&invite.endpoint, &invite.token, profile).map_err(|e| e.to_string())?.with_sender("desktop");
    }
    if let Some(name) = name { client.set_display_name(name).map_err(remote_error)?; }
    let member = client.my_member().map_err(remote_error)?;
    let path = match path {
        Some(path) => path,
        None => invite.store(stored.ok_or("No settings directory available.")?)?,
    };
    Ok((id, snapshot, member, Some(HostedBoard { path, endpoint: invite.endpoint })))
}

pub fn start(mut opts: Options, wake: rune::Waker) -> (mpsc::SyncSender<Command>, mpsc::Receiver<Update>) {
    let local_opts = Options { invite: None, endpoint: None, project: None, ..opts.clone() };
    let (commands, rx) = mpsc::sync_channel::<Command>(16);
    let (tx, updates) = mpsc::sync_channel(8);
    // Provisioning can take much longer than an event poll. Serialize host operations
    // separately so a reopened panel observes earlier changes without delaying the feed.
    let (host_tx, host_rx) = mpsc::sync_channel::<(String, u64, HostOp)>(4);
    let host_updates = tx.clone();
    let host_wake = wake.clone();
    let dir = opts.data_dir.clone();
    std::thread::spawn(move || {
        for (id, request, op) in host_rx {
            let result = host_control(dir.as_deref(), &id, op);
            if host_updates.send(Update::Host(request, result)).is_err() { break; }
            host_wake();
        }
    });
    std::thread::spawn(move || {
        let send = |update| {
            if tx.send(update).is_err() { return false; }
            wake();
            true
        };
        let change_board = |opts: &mut Options, source, name: Option<&str>, expected: Option<String>| {
            let result = if let Some(source) = source {
                join_board(source, name, expected.as_deref(), opts.profile.as_deref())
            } else {
                (|| {
                    let id = expected.ok_or("Missing board id")?;
                    let client = local_opts.connect()?;
                    let snapshot = client.snapshot().map_err(|e| e.to_string())?;
                    if !crate::projects::available(&snapshot, false).contains(id.as_str()) { return Err("This local board is not available.".into()); }
                    let member = client.my_member().map_err(|e| e.to_string())?;
                    Ok((id, snapshot, member, None))
                })()
            };
            let success = result.is_ok();
            if let Ok((id, _, _, hosted)) = &result {
                opts.invite = hosted.as_ref().map(|h| h.path.clone());
                opts.project = hosted.as_ref().map(|_| id.clone());
                opts.endpoint = None;
            }
            (send(Update::BoardSetup(result)), success)
        };
        'connection: loop {
            let client = match opts.connect() {
                Ok(c) => c,
                Err(e) => {
                    if !send(Update::Status(format!("offline · {e}"))) { break; }
                    match rx.recv_timeout(Duration::from_secs(2)) {
                        Ok(Command::JoinBoard { invite, name }) => if !change_board(&mut opts, Some(invite), Some(&name), None).0 { break; },
                        Ok(Command::SwitchBoard { id, path }) => if !change_board(&mut opts, path.map(JoinSource::File), None, Some(id)).0 { break; },
                        Ok(Command::Host { request, .. }) => if !send(Update::Host(request, Err("The local board is offline.".into()))) { break; },
                        Ok(Command::BoardPassword { request, .. }) => if !send(Update::BoardPassword(request, false)) { break; },
                        Ok(Command::CreateBoard { .. }) => if !send(Update::BoardSetup(Err(e.to_string()))) { break; },
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        _ => {},
                    }
                    continue;
                }
            };
            let mut directories=std::collections::BTreeMap::new();
            let mut cursor = match client.snapshot() {
                Ok(snapshot) => {
                    let cursor = snapshot.cursor;
                    for m in &snapshot.messages {directories.insert(m.channel.clone(),None::<std::time::Instant>);}
                    if !send(Update::Snapshot(snapshot)) { break; }
                    cursor
                }
                Err(e) => {
                    if !send(Update::Status(format!("offline · {e}"))) { break; }
                    match rx.recv_timeout(Duration::from_secs(2)) {
                        Ok(Command::JoinBoard { invite, name }) => if !change_board(&mut opts, Some(invite), Some(&name), None).0 { break; },
                        Ok(Command::SwitchBoard { id, path }) => if !change_board(&mut opts, path.map(JoinSource::File), None, Some(id)).0 { break; },
                        Ok(Command::Host { request, .. }) => if !send(Update::Host(request, Err("The local board is offline.".into()))) { break; },
                        Ok(Command::BoardPassword { request, .. }) => if !send(Update::BoardPassword(request, false)) { break; },
                        Ok(Command::CreateBoard { .. }) => if !send(Update::BoardSetup(Err(e.to_string()))) { break; },
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        _ => {},
                    }
                    continue;
                }
            };
            if !send(Update::Status("live".into())) { break; }
            if !send(Update::Personal(client.my_member().map_err(|e|e.to_string()))) {return;}
            if !send(Update::Extra(client.reactions().and_then(|r| client.schedules().map(|s| (r,s))).map_err(|e| e.to_string()))) { return; }
            if let Ok(organization) = client.organization() { if !send(Update::Organization(organization)) { return; } }
            loop {
                match rx.try_recv() {
                    Ok(command) => {
                        match command {
                            Command::JoinBoard { invite, name } => {
                                let (sent, success) = change_board(&mut opts, Some(invite), Some(&name), None);
                                if !sent { return; }
                                if success { continue 'connection; }
                            }
                            Command::SwitchBoard { id, path } => {
                                let (sent, success) = change_board(&mut opts, path.map(JoinSource::File), None, Some(id));
                                if !sent { return; }
                                if success { continue 'connection; }
                            }
                            Command::Host { id, request, operation } => {
                                let error = if opts.invite.is_some() || opts.endpoint.is_some() {
                                    "Host controls are unavailable on a joined board."
                                } else if host_tx.try_send((id, request, operation)).is_err() {
                                    "Host controls are busy. Try again shortly."
                                } else { continue; };
                                if !send(Update::Host(request, Err(error.into()))) { return; }
                            }
                            Command::BoardPassword { id, request, password } => {
                                if !send(Update::BoardPassword(request, change_password(&opts, &id, password))) { return; }
                            }
                            Command::CreateBoard { id, name } => {
                                let result = (|| -> Result<_, String> {
                                    let before = client.snapshot().map_err(|e| e.to_string())?;
                                    if crate::projects::available(&before, false).contains(id.as_str()) {
                                        return Err("A board with that id already exists.".into());
                                    }
                                    client.set_display_name(&name).map_err(|e| e.to_string())?;
                                    let channel = crate::projects::channel(&id, "general");
                                    client.action(&Action::CreateChannel { channel }).map_err(|e| e.to_string())?;
                                    let snapshot = client.snapshot().map_err(|e| e.to_string())?;
                                    let member = client.my_member().map_err(|e| e.to_string())?;
                                    Ok((id, snapshot, member, None))
                                })();
                                if !send(Update::BoardSetup(result)) { return; }
                            }
                            Command::Members(channel)=>{let result=client.members(&channel).map_err(|e|e.to_string());if !send(Update::Members(channel,result)){return;}}
                            Command::Personal(name)=>{let result=client.set_display_name(&name).and_then(|_|client.my_member()).map_err(|e|e.to_string());if !send(Update::Personal(result)){return;}}
                            Command::PostWithFile { action, draft, path } => {
                                let result = upload_bytes(&path).and_then(|(name,bytes)| {
                                    let post=client.action(&action).map_err(|e| e.to_string())?;
                                    let note=match post.id {
                                        Some(id)=>match client.attach(id,&name,bytes) { Ok(_)=>format!("Attached {name}"), Err(e)=>format!("Posted #{id}, but file upload failed: {e}. Retry from the message attachments.") },
                                        None=>"Post accepted without a message ID; file not attached.".into(),
                                    };
                                    Ok(note)
                                });
                                match result {
                                    Ok(note)=>{ if !send(Update::Done(draft,Ok(()))) || !send(Update::SentFile(path,note)) {return;} }
                                    Err(e)=>if !send(Update::Done(draft,Err(e))) {return;},
                                }
                            }
                            Command::Action { action, draft } => {
                                let create_sprint = matches!(action, Action::CreateSprint {..});
                                let refresh = matches!(action, Action::PostTask {..} | Action::UpdateTask {..} | Action::AcknowledgeTask {..} | Action::CreateSprint {..} | Action::SetTaskSprint {..} | Action::FinishSprint {..} | Action::PostSprintTask {..});
                                let result = client.action(&action).map_err(|e| e.to_string());
                                if refresh && result.is_ok() && let Ok(organization) = client.organization() {
                                    if !send(Update::Organization(organization)) { return; }
                                }
                                if create_sprint && let Ok(response) = &result && let Some(id) = response.id && !send(Update::SprintCreated(id)) { return; }
                                if !send(Update::Done(draft, result.map(|_| ()))) { return; }
                            }
                            Command::Attachments(id) => {
                                if !send(Update::Attachments(id, client.attachments(id).map_err(|e| e.to_string()))) { return; }
                            }
                            Command::Upload { message, path } => {
                                let result = upload_bytes(&path).and_then(|(name, bytes)| client.attach(message, &name, bytes).map_err(|e| e.to_string()))
                                    .map(|_| "Uploaded attachment".to_string());
                                if !send(Update::FileDone(result)) { return; }
                                if !send(Update::Attachments(message, client.attachments(message).map_err(|e| e.to_string()))) { return; }
                            }
                            Command::Download { id, path } => {
                                let result = client.download_attachment(id).map_err(|e| e.to_string())
                                    .and_then(|bytes| save_download(&path, &bytes)).map(|_| "Saved attachment (not opened)".into());
                                if !send(Update::FileDone(result)) { return; }
                            }
                        }
                    }
                    Err(mpsc::TryRecvError::Disconnected) => return,
                    Err(mpsc::TryRecvError::Empty) => {
                        if let Some((ch,time))=directories.iter_mut().find(|(_,t)|t.is_none_or(|v|v.elapsed().as_secs()>=60)) {
                            *time=Some(std::time::Instant::now());
                            if let Ok(list)=client.members(ch) && !send(Update::Names(ch.clone(),list)){return;}
                        }
                    }
                }
                match client.events(cursor, 250) {
                    Ok(events) => {
                        let refresh = events.iter().any(|e| matches!(e.kind, neochat::protocol::EventKind::Reaction { .. } | neochat::protocol::EventKind::Scheduled { .. } | neochat::protocol::EventKind::ScheduleCanceled { .. } | neochat::protocol::EventKind::SchedulePublished { .. }));
                        let organization_changed = events.iter().any(|e| matches!(&e.kind, neochat::protocol::EventKind::Message { message } if message.task.is_some()) || matches!(e.kind, neochat::protocol::EventKind::OrganizationChanged {..} | neochat::protocol::EventKind::ThreadDeleted {..} | neochat::protocol::EventKind::ChannelDeleted {..}));
                        for event in events {
                            if let neochat::protocol::EventKind::ChannelDeleted { channel } = &event.kind { directories.remove(channel); }
                            if let neochat::protocol::EventKind::Message{message}=&event.kind {directories.entry(message.channel.clone()).or_insert(None);}
                            cursor = cursor.max(event.cursor);
                            if !send(Update::Event(event)) { return; }
                        }
                        if refresh && !send(Update::Extra(client.reactions().and_then(|r| client.schedules().map(|s| (r,s))).map_err(|e| e.to_string()))) { return; }
                        if organization_changed && let Ok(organization) = client.organization() {
                            if !send(Update::Organization(organization)) { return; }
                        }
                    },
                    Err(e) => { if !send(Update::Status(format!("reconnecting · {e}"))) { return; } break; }
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    (commands, updates)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_password_uses_only_local_admin_and_never_a_share_route() {
        use std::io::{BufRead, BufReader, Read, Write};
        let dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let record = |url: &str| serde_json::to_vec(&serde_json::json!({"url":url,"token":"ordinary-test-token"})).unwrap();
        std::fs::write(dir.path().join("admin-token"), "test-admin").unwrap();
        std::fs::write(dir.path().join("endpoint.json"), record(&url)).unwrap();
        let opts = Options { data_dir: Some(dir.path().into()), ..Default::default() };
        for denied in [Options { endpoint: Some(url.clone()), ..opts.clone() },
            Options { invite: Some("unused".into()), ..opts.clone() }, Options { project: Some("other".into()), ..opts.clone() },
            Options { fixture: true, ..opts.clone() }] {
            assert!(!change_password(&denied, "team", None));
        }
        let handler = std::thread::spawn(move || {
            for expected in [Some(" secret "), None] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(&stream);
                let mut lines = (&mut reader).lines().map(Result::unwrap);
                assert_eq!(lines.next().unwrap(), "POST /v1/admin/board-password HTTP/1.1");
                let headers: Vec<_> = lines.take_while(|s| !s.is_empty()).collect();
                assert!(headers.iter().any(|h| h.eq_ignore_ascii_case("authorization: Bearer test-admin")));
                let len = headers.iter().filter_map(|h| h.to_lowercase().strip_prefix("content-length: ")?.parse::<usize>().ok()).next().unwrap();
                let mut body = vec![0; len]; reader.read_exact(&mut body).unwrap();
                assert_eq!(serde_json::from_slice::<serde_json::Value>(&body).unwrap(), serde_json::json!({"project":"team","password":expected}));
                stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").unwrap();
            }
        });
        assert!(change_password(&opts, "team", Some(JoinPassword::new(" secret ".into()))));
        assert!(change_password(&opts, "team", None));
        handler.join().unwrap();
    }

    #[test]
    fn exchanged_credential_is_verified_before_saving_and_never_redeemed_again() {
        use neochat::invite_file::InviteFile;
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;

        for valid in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let client = Client::new(&format!("http://{}", listener.local_addr().unwrap()), "personal-fixture", None).unwrap();
            let server = std::thread::spawn(move || {
                let snapshot = if valid { r#"{"cursor":0,"channels":["dev/general"],"messages":[]}"# }
                    else { r#"{"cursor":0,"channels":["other/general"],"messages":[]}"# };
                let mut calls = vec![("GET /v1/snapshot", snapshot)];
                if valid { calls.extend([("POST /v1/display-name", "{}"), ("GET /v1/me", r#"{"actor":"sam","display_name":"Sam Lee"}"#)]); }
                for (expected, reply) in calls {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut req = BufReader::new(&stream);
                    let mut line = String::new(); req.read_line(&mut line).unwrap();
                    assert!(line.starts_with(expected));
                    let mut length = 0; let mut authorized = false;
                    loop {
                        line.clear(); req.read_line(&mut line).unwrap(); if line.trim().is_empty() { break; }
                        let (k,v) = line.split_once(':').unwrap();
                        if k.eq_ignore_ascii_case("content-length") { length = v.trim().parse().unwrap(); }
                        if k.eq_ignore_ascii_case("authorization") { authorized = v.trim() == "Bearer personal-fixture"; }
                    }
                    assert!(authorized);
                    if expected.starts_with("POST") {
                        let mut body = vec![0; length]; req.read_exact(&mut body).unwrap();
                        assert_eq!(serde_json::from_slice::<serde_json::Value>(&body).unwrap(), serde_json::json!({"name":"Sam Lee"}));
                    }
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
                }
            });
            let invite = InviteFile { project: Some("dev".into()), version: 1, endpoint: "https://board.example/".into(), token: "personal-fixture".into(), expires_at: i64::MAX as u64 };
            let result = finish_join(JoinCredential { invite, path: None, personal: true }, client, Some("Sam Lee"), None, None, Some(dir.path()));
            if valid {
                let (id, _, member, hosted) = result.unwrap();
                assert_eq!(id, "dev"); assert_eq!(member.display_name.as_deref(), Some("Sam Lee"));
                let path = hosted.unwrap().path;
                assert_eq!(InviteFile::read(&path).unwrap().token, "personal-fixture");
                #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600); }
            } else { assert!(result.is_err() && dir.path().read_dir().unwrap().next().is_none()); }
            server.join().unwrap();
        }
    }

    #[test]
    fn explicit_download_never_overwrites() {
        let path = std::env::temp_dir().join(format!("neochat-desktop-save-{}", std::process::id()));
        save_download(&path, b"first").unwrap();
        assert!(save_download(&path, b"second").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        let (name, bytes) = upload_bytes(&path).unwrap();
        assert!(!name.contains('/')); assert_eq!(bytes, b"first");
        std::fs::remove_file(path).unwrap();
        assert!(upload_bytes(std::path::Path::new("/")).is_err());
        assert!(upload_bytes(std::path::Path::new("relative.txt")).is_err());
        assert!(save_download(std::path::Path::new("relative.txt"), b"x").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn download_rejects_existing_symlinks() {
        let path = std::env::temp_dir().join(format!("neochat-desktop-link-{}", std::process::id()));
        std::os::unix::fs::symlink("/nonexistent-neochat-download", &path).unwrap();
        assert!(save_download(&path,b"never follow").is_err());
        std::fs::remove_file(path).unwrap();
    }

    // Uses an existing debug daemon; deliberately does not build another dependency graph.
    #[test]
    #[ignore = "requires an up-to-date neochatd debug binary; set NEOCHAT_DESKTOP_TEST_DAEMON"]
    fn worker_live_roundtrip() {
        use std::{process::{Child, Command as Process, Stdio}, sync::Arc, time::Instant};
        struct Daemon(Child, PathBuf);
        impl Drop for Daemon { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); let _ = std::fs::remove_dir_all(&self.1); } }
        let path = std::env::temp_dir().join(format!("neochat-desktop-live-{}",std::process::id()));
        let binary = std::env::var_os("NEOCHAT_DESKTOP_TEST_DAEMON").map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/debug/neochatd"));
        let daemon = Daemon(Process::new(binary).arg("--data-dir").arg(&path).arg("--bind").arg("127.0.0.1:0")
            .stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("start test daemon"),path.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        let client = loop {
            if let Ok(client) = Client::discover(Some(&path),None) { break client; }
            assert!(Instant::now() < deadline,"daemon discovery timeout");
            std::thread::sleep(Duration::from_millis(25));
        };
        let (commands, updates) = start(Options { data_dir:Some(path.clone()), ..Options::default() },Arc::new(||{}));
        fn until(updates: &mpsc::Receiver<Update>, predicate: impl Fn(&Update)->bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let update = updates.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("worker update timeout");
                match &update {
                    Update::Done(_,Err(e)) | Update::FileDone(Err(e)) | Update::Attachments(_,Err(e)) | Update::Extra(Err(e)) | Update::BoardSetup(Err(e)) => panic!("worker failure: {e}"),
                    _ => {}
                }
                if predicate(&update) { return; }
            }
        }
        until(&updates,|u| matches!(u,Update::Extra(Ok(_))));
        commands.send(Command::CreateBoard { id: "launch-team".into(), name: "Sam Lee".into() }).unwrap();
        until(&updates, |u| matches!(u, Update::BoardSetup(Ok((id, board, member, None)))
            if id == "launch-team" && board.channels.iter().any(|c| c == "launch-team/general") && member.display_name.as_deref() == Some("Sam Lee")));
        assert!(client.snapshot().unwrap().channels.contains(&"launch-team/general".into()));
        let action = |action| {
            commands.send(Command::Action { action,draft:None }).unwrap();
            until(&updates,|u| matches!(u,Update::Done(_,Ok(()))));
        };
        action(Action::Post { channel:"general".into(),text:"original".into(),labels:Default::default(),task:false,needs:vec![],assignee:None });
        let id = client.snapshot().unwrap().messages.last().unwrap().id;
        action(Action::Edit { id,text:"edited".into() });
        action(Action::SetLabels { id,labels:[("area".into(),"desktop".into())].into() });
        assert_eq!(client.snapshot().unwrap().messages[0].text,"edited");
        assert_eq!(client.snapshot().unwrap().messages[0].labels["area"],"desktop");
        action(Action::React { id,reaction:"+1".into(),remove:false });
        until(&updates,|u| matches!(u,Update::Extra(Ok((r,_))) if r.iter().any(|r| r.id == id)));
        action(Action::React { id,reaction:"+1".into(),remove:true });
        until(&updates,|u| matches!(u,Update::Extra(Ok((r,_))) if r.is_empty()));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        action(Action::Schedule { channel:"general".into(),text:"later".into(),publish_at:now + 3600,labels:Default::default() });
        until(&updates,|u| matches!(u,Update::Extra(Ok((_,s))) if s.len() == 1));
        let schedule_id = client.schedules().unwrap()[0].id;
        action(Action::CancelSchedule { id:schedule_id });
        until(&updates,|u| matches!(u,Update::Extra(Ok((_,s))) if s.is_empty()));
        let publish_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 2;
        action(Action::Schedule { channel:"general".into(),text:"publish from desktop".into(),publish_at,labels:Default::default() });
        until(&updates,|u| matches!(u,Update::Event(Event { kind:neochat::protocol::EventKind::Message { message },.. }) if message.text == "publish from desktop"));
        let source = path.join("source.txt"); std::fs::write(&source,b"desktop attachment").unwrap();
        commands.send(Command::Upload { message:id,path:source }).unwrap();
        until(&updates,|u| matches!(u,Update::FileDone(Ok(_))));
        until(&updates,|u| matches!(u,Update::Attachments(message,Ok(files)) if *message == id && files.len() == 1));
        let file = client.attachments(id).unwrap().remove(0);
        let dest = path.join("saved.txt");
        commands.send(Command::Download { id:file.id,path:dest.clone() }).unwrap();
        until(&updates,|u| matches!(u,Update::FileDone(Ok(_))));
        assert_eq!(std::fs::read(dest).unwrap(),b"desktop attachment");
        drop(commands); drop(updates); drop(daemon);
    }
}