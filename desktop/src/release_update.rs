//! One background check per launch. Only a local user action can release the worker to run the CLI.
use std::{fs, io::Read, path::{Path, PathBuf}, process::{Command, Stdio}, sync::mpsc, time::Duration};
use semver::Version;

const LATEST: &str = "https://api.github.com/repos/parkers0405/neoboard/releases/latest";
const LIMIT: u64 = 256 * 1024;

fn newer(body: impl Read, current: &Version) -> Option<Version> {
    #[derive(serde::Deserialize)]
    struct Release { tag_name: String, draft: bool, prerelease: bool }
    let mut data = Vec::new();
    body.take(LIMIT + 1).read_to_end(&mut data).ok()?;
    if data.len() as u64 > LIMIT { return None; }
    let release: Release = serde_json::from_slice(&data).ok()?;
    let tag = release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name);
    let version = Version::parse(tag).ok()?;
    (!release.draft && !release.prerelease && version.pre.is_empty() && version.cmp_precedence(current).is_gt()).then_some(version)
}

fn latest() -> Option<Version> {
    let client = reqwest::blocking::Client::builder().https_only(true)
        .redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(8))
        .user_agent(concat!("neoboard-desktop/", env!("CARGO_PKG_VERSION"))).build().ok()?;
    let response = client.get(LATEST).send().ok()?;
    if response.status() != reqwest::StatusCode::OK { return None; }
    newer(response, &Version::parse(env!("CARGO_PKG_VERSION")).ok()?)
}

fn binary(name: &str) -> String { format!("{name}{}", std::env::consts::EXE_SUFFIX) }

fn package_cli(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    if !exe.is_absolute() || exe.file_name()?.to_str()? != binary("neochat-desktop") { return None; }
    for parent in dir.ancestors() {
        for marker in [".git", "Cargo.toml", ".rustc_info.json"] {
            match fs::symlink_metadata(parent.join(marker)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                _ => return None,
            }
        }
    }
    if !fs::symlink_metadata(dir.join("LICENSE")).ok()?.is_file() { return None; }
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(dir).ok()?.permissions().mode() & 0o022 != 0 { return None; }
    }
    for name in ["neochat-desktop", "neoboard", "neochat", "neochatd", "neochat-mcp", "neochat-share"] {
        let meta = fs::symlink_metadata(dir.join(binary(name))).ok()?;
        if !meta.is_file() { return None; }
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode();
            if mode & 0o111 == 0 || mode & 0o022 != 0 { return None; }
        }
        #[cfg(windows)] {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 { return None; } // reparse point
        }
    }
    Some(dir.join(binary("neoboard")))
}

fn install(exe: &Path) -> Result<String, String> {
    let cli = package_cli(exe).ok_or("The packaged updater is no longer available.")?;
    let mut command = Command::new(cli);
    command.arg("update").current_dir(exe.parent().unwrap()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    #[cfg(windows)] {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn().map_err(|e| format!("Could not run the updater: {e}"))?;
    // The CLI prints its verified staging/backup path and manual restart steps to stderr.
    // Drain the entire pipe without waiting on the UI or retaining an unbounded log.
    let mut pipe = child.stderr.take().unwrap();
    let mut tail = std::collections::VecDeque::with_capacity(8192);
    let mut chunk = [0; 4096];
    let diagnostic = loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break String::from_utf8_lossy(tail.make_contiguous()).trim().to_string(),
            Ok(n) => { tail.drain(..(tail.len()+n).saturating_sub(8192)); tail.extend(&chunk[..n]); }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break String::new(),
        }
    };
    drop(pipe);
    let status = child.wait().map_err(|e| format!("Could not confirm updater exit: {e}"))?;
    if status.success() { Ok(diagnostic) } else { Err(format!("Updater returned {status}. {diagnostic}")) }
}

#[derive(Default)]
pub enum Stage { #[default] Hidden, Offer(Version), Running, Finished(Result<String, String>) }

#[derive(Default)]
pub struct Session {
    pub stage: Stage,
    pub pulse: f32,
    inbox: Option<mpsc::Receiver<Stage>>,
    consent: Option<mpsc::Sender<()>>,
    started: bool,
}

impl Session {
    pub fn start(&mut self, fixture: bool, wake: rune::Waker) {
        if fixture || cfg!(any(test, debug_assertions)) || self.started { return; }
        self.started = true;
        let (send, recv) = mpsc::channel();
        let (approve, wait) = mpsc::channel();
        let worker = std::thread::Builder::new().name("desktop-update".into()).spawn(move || -> Option<()> {
            let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
            package_cli(&exe)?;
            let version = latest()?;
            send.send(Stage::Offer(version)).ok()?;
            wake();
            wait.recv().ok()?;
            send.send(Stage::Finished(install(&exe))).ok()?;
            wake();
            Some(())
        });
        if worker.is_ok() { self.inbox = Some(recv); self.consent = Some(approve); }
    }
    pub fn accept(&mut self) {
        if !matches!(self.stage, Stage::Offer(_)) { return; }
        self.stage = if self.consent.take().is_some_and(|tx| tx.send(()).is_ok()) { Stage::Running }
            else { Stage::Finished(Err("No packaged updater is available.".into())) };
    }
    pub fn dismiss(&mut self) {
        if matches!(self.stage, Stage::Running) { return; }
        self.consent = None;
        self.inbox = None;
        self.stage = Stage::Hidden;
    }
    pub fn poll(&mut self, seconds: f64, tx: &mut rune::Tx) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.inbox {
            match rx.try_recv() {
                Ok(stage) => { self.stage = stage; changed = true; }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.inbox = None;
                    if matches!(self.stage, Stage::Running) {
                        self.stage = Stage::Finished(Err("The updater stopped without a result.".into()));
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        if matches!(self.stage, Stage::Running) {
            self.pulse = (seconds * 3.).sin() as f32 * 0.5 + 0.5;
            tx.wake_after(Duration::from_millis(60));
            changed = true;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_newer_release_only() {
        let current = Version::new(1, 9, 0);
        for (tag, draft, prerelease, offer) in [
            ("v1.10.0", false, false, true), ("1.9.0+build", false, false, false),
            ("v1.8.9", false, false, false), ("v3.0.0", true, false, false),
            ("v2.0.0", false, true, false), ("2.0.0-rc.1", false, false, false),
            ("latest", false, false, false),
        ] {
            let json = serde_json::json!({"tag_name": tag, "draft":draft, "prerelease":prerelease}).to_string();
            assert_eq!(newer(json.as_bytes(), &current).is_some(), offer, "{tag}");
        }
        let valid = br#"{"tag_name":"v2.0.0","draft":false,"prerelease":false}"#;
        let mut oversized = valid.to_vec();
        oversized.resize(LIMIT as usize + 1, b' ');
        assert!(newer(&oversized[..], &current).is_none());
    }

    #[test]
    fn dismiss_and_update_are_session_local_and_keep_drafts() {
        let mut app = crate::App::new();
        app.draft = crate::edit("unsent");
        let (approve, rx) = mpsc::channel();
        app.release.consent = Some(approve);
        app.release.stage = Stage::Offer(Version::new(2,0,0));
        app.release.dismiss();
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected)));
        assert!(matches!(app.release.stage, Stage::Hidden));
        let (approve, rx) = mpsc::channel();
        app.release.consent = Some(approve);
        app.release.stage = Stage::Offer(Version::new(2,0,0));
        app.release.accept();
        app.release.accept();
        assert_eq!(rx.try_iter().count(), 1);
        assert!(matches!(app.release.stage, Stage::Running));
        assert_eq!(app.draft.text, "unsent");
    }

    #[cfg(unix)]
    #[test]
    fn only_flat_regular_packaged_executables() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("neochat-desktop");
        assert!(package_cli(&exe).is_none());
        for name in ["neochat-desktop", "neoboard", "neochat", "neochatd", "neochat-mcp", "neochat-share"] {
            let path = dir.path().join(name);
            fs::write(&path, "").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cli = dir.path().join("neoboard");
        assert!(package_cli(&exe).is_none());
        fs::write(dir.path().join("LICENSE"), "").unwrap();
        assert_eq!(package_cli(&exe), Some(cli.clone()));
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        assert!(package_cli(&exe).is_none());
        fs::remove_file(dir.path().join("Cargo.toml")).unwrap();
        fs::remove_file(&cli).unwrap();
        symlink(&exe, &cli).unwrap();
        assert!(package_cli(&exe).is_none());
    }
}