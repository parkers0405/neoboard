use super::runtime::Target;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs::{self, File, OpenOptions}, io::Write, path::{Path, PathBuf}};

pub const DEFAULT_CHAT: &str = "default";
fn default_chat() -> String { DEFAULT_CHAT.into() }

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && !matches!(id, "." | "..")
        && id.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

pub fn binding_key(board: &str, harness: &str, chat: &str) -> Result<String, String> {
    if !valid_id(board) { return Err("Invalid board ID".into()); }
    if !valid_id(chat) { return Err("Invalid chat ID".into()); }
    Ok(if chat == DEFAULT_CHAT { format!("{board}:{harness}") } else { format!("{board}:{harness}:{chat}") })
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Source {
    #[serde(default = "default_chat")] pub chat_id: String,
    pub board: String, pub project: String, pub invite: PathBuf, pub origin: String, pub actor: String, pub harness: String,
}
#[derive(Serialize, Deserialize)]
pub struct Pending { pub source: Source, pub expires: u64 }
#[derive(Clone, Serialize, Deserialize)]
pub struct Binding {
    pub source: Source, pub target: Target, pub bearer: Option<String>, pub enabled: bool,
    pub watermark: u64, pub uncertain: Option<String>, pub last: String, pub test: Option<String>,
}
#[derive(Default, Serialize, Deserialize)]
pub struct State { pub pending: BTreeMap<String, Pending>, pub bindings: BTreeMap<String, Binding> }

impl State {
    pub fn prepare(&mut self, code: String, source: Source, now: u64) -> Result<(), String> {
        let key = binding_key(&source.board, &source.harness, &source.chat_id)?;
        if self.bindings.contains_key(&key) || self.bindings.values().any(|b| super::duplicate(&b.source, &source)) {
            return Err("Already paired for this board actor; use On or Off".into());
        }
        let same_chat = |s: &Source| s.board == source.board && s.harness == source.harness && s.chat_id == source.chat_id;
        if self.pending.values().any(|p| p.expires > now && !same_chat(&p.source) && super::duplicate(&p.source, &source)) {
            return Err("Another pairing request uses this scoped actor. Give each conversation its own actor.".into());
        }
        self.pending.retain(|_, p| p.expires > now && !same_chat(&p.source));
        self.pending.insert(code, Pending { source, expires: now + 600 });
        Ok(())
    }

    pub fn list(&self, board: &str) -> Result<Vec<serde_json::Value>, String> {
        if !valid_id(board) { return Err("Invalid board ID".into()); }
        Ok(self.bindings.values().filter(|b| b.source.board == board).map(|b| serde_json::json!({
            "chat_id": b.source.chat_id, "harness": b.source.harness, "actor": b.source.actor,
            "session": b.target.session, "enabled": b.enabled,
        })).collect())
    }
}

pub fn home() -> Result<PathBuf,String> {
    let p = std::env::var_os("NEOBOARD_WAKE_HOME").map(PathBuf::from).or_else(|| dirs::data_local_dir().map(|p| p.join("neoboard").join("wake"))).ok_or("Cannot locate user data directory")?;
    if !p.is_absolute() { return Err("Wake state must use an absolute directory".into()); }
    let mut builder = fs::DirBuilder::new(); builder.recursive(true);
    #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
    builder.create(&p).map_err(|_| "Cannot create private wake directory")?;
    let m = fs::symlink_metadata(&p).map_err(|_| "Cannot access wake directory")?;
    if !m.is_dir() { return Err("Expected private wake directory, not a link".into()); }
    #[cfg(unix)] { use std::os::unix::fs::MetadataExt; if m.mode() & 0o077 != 0 || m.uid() != unsafe {libc::geteuid()} {return Err("Wake directory must be owned by you and mode 0700".into());} }
    Ok(p)
}

pub fn lock(dir:&Path, name:&str, wait:bool)->Result<File,String> {
    let f=OpenOptions::new().create(true).truncate(false).read(true).write(true).open(dir.join(name)).map_err(|_|"Cannot open wake lock")?;
    if wait { f.lock().map_err(|_|"Cannot lock wake state")?; } else { f.try_lock().map_err(|_|"Wake operation already in progress")?; }
    Ok(f)
}

pub fn transaction<T>(dir:&Path, op:impl FnOnce(&mut State)->Result<T,String>)->Result<T,String> {
    let _lock=lock(dir,"state.lock",true)?;
    let path=dir.join("bindings.json");
    let mut state=match File::open(&path) { Ok(f)=>serde_json::from_reader(f).map_err(|_|"Invalid wake state; not resetting delivery history")?, Err(e) if e.kind()==std::io::ErrorKind::NotFound=>State::default(), Err(_)=>return Err("Cannot read wake state".into()) };
    let before=serde_json::to_vec(&state).map_err(|_|"Cannot encode state")?;
    let out=op(&mut state)?;
    let after=serde_json::to_vec(&state).map_err(|_|"Cannot encode state")?;
    if before!=after {
        let mut tmp=tempfile::NamedTempFile::new_in(dir).map_err(|_|"Cannot save private wake state")?;
        tmp.write_all(&after).and_then(|_|tmp.as_file().sync_all()).map_err(|_|"Cannot sync private wake state")?;
        tmp.persist(path).map_err(|_|"Cannot replace wake state")?;
        #[cfg(unix)] File::open(dir).and_then(|f|f.sync_all()).map_err(|_|"Cannot sync wake directory")?;
    }
    Ok(out)
}