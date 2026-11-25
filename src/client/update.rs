//! Local-only bundled-daemon update protocol. All participants trust the
//! daemon's user-private data directory (inherited user ACL on Windows).
//! No network endpoint, PID lookup, signal, or process-name fallback.
use super::ClientError;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{fs::{self, File, OpenOptions}, io::{self, Write}, path::{Path, PathBuf}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};

const ACTIVE: &str = "bundled-auto-v1.json";
const LEASE: &str = "bundled-auto-v1.lock";
const HOLD: &str = "bundled-update-v1.json";
const DONE: &str = "bundled-stopped-v1.json";
const TTL: u64 = 600;
const POLL: Duration = Duration::from_millis(100);

#[derive(Serialize, Deserialize)]
struct Instance { exe: PathBuf, nonce: String }

#[derive(Serialize, Deserialize)]
struct Hold { target: Instance, version: String, expires: u64 }

fn read<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match File::open(path) {
        Ok(file) => Ok(Some(serde_json::from_reader(file)?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn publish(data: &Path, name: &str, value: &impl Serialize) -> io::Result<()> {
    let mut tmp = tempfile::NamedTempFile::new_in(data)?;
    serde_json::to_writer(&mut tmp, value)?;
    tmp.flush()?;
    tmp.persist(data.join(name))?;
    Ok(())
}

fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }
fn valid(hold: &Hold) -> bool { matches!(hold.expires.checked_sub(now()), Some(1..=TTL)) }
pub(super) fn sibling() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let candidate = exe.with_file_name(format!("neochatd{}", std::env::consts::EXE_SUFFIX));
    let actual = candidate.canonicalize()?;
    if actual != candidate { return Err(io::Error::other("daemon is not an installed sibling")); }
    Ok(actual)
}

fn release(v: &str) -> Option<[u64; 3]> {
    let mut parts = v.split('.');
    let n = [parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?];
    parts.next().is_none().then_some(n)
}

pub(super) fn check_spawn(data: &Path, bundled: bool) -> io::Result<()> {
    if let Some(h) = read::<Hold>(&data.join(HOLD))? {
        if valid(&h) {
            // Leave the hold intact for reconnecting clients from the old build.
            let newer = release(env!("CARGO_PKG_VERSION")).zip(release(&h.version)).is_some_and(|(a,b)| a > b);
            if !(bundled && newer && sibling()? == h.target.exe) {
                return Err(io::Error::other("Neoboard daemon startup paused for update"));
            }
        }
    }
    Ok(())
}

pub struct AutoDaemon { data: PathBuf, nonce: String, _lease: File }

impl AutoDaemon {
    /// Called only for an explicit bundled auto-spawn, under service.lock.
    pub fn register(data: &Path) -> io::Result<Self> {
        check_spawn(data, true)?;
        let lease = OpenOptions::new().write(true).create(true).truncate(false).open(data.join(LEASE))?;
        let active = Instance { exe: std::env::current_exe()?.canonicalize()?, nonce: crate::board::random_token() };
        publish(data, ACTIVE, &active)?;
        lease.try_lock()?;
        Ok(Self { data: data.into(), nonce: active.nonce, _lease: lease })
    }

    pub async fn stop_requested(&self) {
        loop {
            if let Ok(Some(h)) = read::<Hold>(&self.data.join(HOLD)) {
                if valid(&h) && h.target.nonce == self.nonce { return; }
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// A crash releases OS locks too. Acknowledge only after graceful drain.
    pub fn finished(&self) -> io::Result<()> { publish(&self.data, DONE, &self.nonce) }
}

fn busy(file: &File) -> io::Result<bool> {
    match file.try_lock() {
        Ok(()) => { file.unlock()?; Ok(false) }
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

fn client_error(e: io::Error) -> ClientError { ClientError(format!("local daemon update: {e}")) }

/// Request graceful shutdown only of a cooperating active sibling. `false`
/// means no eligible handshake (including manual, custom and legacy daemons).
/// Run off the UI thread; in-flight long polls can take 25 seconds to drain.
pub fn stop_for_update(data: &Path) -> Result<bool, ClientError> {
    stop(data).map_err(client_error)
}

fn stop(data: &Path) -> io::Result<bool> {
    let Some(active) = read::<Instance>(&data.join(ACTIVE))? else { return Ok(false) };
    if sibling().ok().as_ref() != Some(&active.exe) { return Ok(false); }
    let lease = OpenOptions::new().write(true).open(data.join(LEASE))?;
    if !busy(&lease)? { return Ok(false); } // stale registration, not this service
    let service = OpenOptions::new().write(true).open(data.join("service.lock"))?;
    let nonce = active.nonce.clone();
    publish(data, HOLD, &Hold { target: active, version: env!("CARGO_PKG_VERSION").into(), expires: now() + TTL })?;
    let deadline = Instant::now() + Duration::from_secs(45);
    while busy(&service)? {
        if Instant::now() >= deadline { return Err(io::Error::new(io::ErrorKind::TimedOut, "daemon did not release service.lock")); }
        std::thread::sleep(POLL);
    }
    if read::<String>(&data.join(DONE))?.as_ref() != Some(&nonce) {
        return Err(io::Error::other("daemon exited without confirming graceful shutdown"));
    }
    Ok(true)
}

/// Cancel this installation's hold on rollback. Does not launch a daemon.
/// Also safe after a failed stop; call only once the installer has abandoned
/// replacement. Otherwise the hold expires automatically after ten minutes.
pub fn resume_after_update(data: &Path) -> Result<(), ClientError> {
    (|| -> io::Result<()> {
        if let Some(h) = read::<Hold>(&data.join(HOLD))? {
            if sibling()? == h.target.exe { fs::remove_file(data.join(HOLD))?; }
        }
        Ok(())
    })().map_err(client_error)
}