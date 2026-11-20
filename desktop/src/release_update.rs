//! One background check per launch. Only a local user action can release the worker to run the CLI.
use std::{fs, io::Read, path::{Path, PathBuf}, process::{Command, Stdio}, sync::mpsc, time::Duration};
use semver::Version;

const LATEST: &str = "https://api.github.com/repos/parkers0405/neoboard/releases/latest";
const LIMIT: u64 = 256 * 1024;
const HANDOFF: &str = "NEOBOARD_DESKTOP_UPDATE_HANDOFF";
pub const RECONNECT: &str = "External MCP connections are not verified. Reconnect Neoboard in each MCP client to load the updated adapter.";

fn managed_installer(exe: &Path) -> Option<&'static str> {
    let dir = exe.parent()?;
    // The DMG's signed app also contains all six portable executables.
    if dir.ends_with("Contents/MacOS") && dir.ancestors().nth(2)?.extension().is_some_and(|s| s.eq_ignore_ascii_case("app")) {
        return Some("Install the new NeoBoard.app from the release DMG.");
    }
    #[cfg(windows)]
    for root in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"].iter().filter_map(std::env::var_os) {
        // current_exe was canonicalized and normally has a verbatim path prefix.
        let root = PathBuf::from(root);
        let normalized = root.canonicalize().unwrap_or_else(|_|root.clone());
        if dir.ancestors().any(|a| [root.as_os_str(), normalized.as_os_str()].iter().any(|r| a.as_os_str().eq_ignore_ascii_case(r))) {
            return Some("Run the new NeoBoard MSI to upgrade this managed installation.");
        }
    }
    None
}

fn stable_version(body: impl Read) -> Option<Version> {
    #[derive(serde::Deserialize)]
    struct Release { tag_name: String, draft: bool, prerelease: bool }
    let mut data = Vec::new();
    body.take(LIMIT + 1).read_to_end(&mut data).ok()?;
    if data.len() as u64 > LIMIT { return None; }
    let release: Release = serde_json::from_slice(&data).ok()?;
    let tag = release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name);
    let version = Version::parse(tag).ok()?;
    (!release.draft && !release.prerelease && version.pre.is_empty()).then_some(version)
}

fn latest() -> Option<Version> {
    let client = reqwest::blocking::Client::builder().https_only(true)
        .redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(8))
        .user_agent(concat!("neoboard-desktop/", env!("CARGO_PKG_VERSION"))).build().ok()?;
    let response = client.get(LATEST).send().ok()?;
    if response.status() != reqwest::StatusCode::OK { return None; }
    stable_version(response)
}

fn binary(name: &str) -> String { format!("{name}{}", std::env::consts::EXE_SUFFIX) }

fn package_cli(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    if managed_installer(exe).is_some() { return None; }
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
    // neoboard desktop owns its private companion and the install transaction.
    // Returning its reserved approval status touches neither daemon nor MCP clients.
    if std::env::var_os("NEOBOARD_PORTABLE_HANDOFF").as_deref() == Some("1".as_ref()) {
        std::process::exit(75);
    }
    #[cfg(unix)]
    let old = fs::metadata(exe).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
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
    if !status.success() { return Err(format!("Updater returned {status}. {diagnostic}")); }
    #[cfg(unix)] {
        use std::os::unix::{fs::MetadataExt, process::CommandExt};
        let new = fs::metadata(exe).map_err(|e| format!("Cannot verify installed desktop: {e}. {diagnostic}"))?;
        if (old.dev(), old.ino()) == (new.dev(), new.ino()) || package_cli(exe).is_none() {
            return Err(format!("No replacement desktop verified at the installed path. {diagnostic}"));
        }
        // Replace only ourselves. Preserve the board selection and relative CLI paths;
        // never rediscover current_exe after the swap (it can now point into the backup).
        let error = Command::new(exe).args(std::env::args_os().skip(1)).current_dir(cwd)
            .env(HANDOFF, &diagnostic).exec();
        Err(format!("Installed, but could not reopen {}: {error}. {diagnostic}", exe.display()))
    }
    #[cfg(not(unix))] { Ok(diagnostic) }
}

#[derive(Default)]
pub enum Stage { #[default] Hidden, Checking, Notice(String), Offer(Version), Running, Finished(Result<String, String>) }

#[derive(Default)]
pub struct Session {
    pub stage: Stage,
    pub pulse: f32,
    inbox: Option<mpsc::Receiver<Stage>>,
    consent: Option<mpsc::Sender<()>>,
    started: bool,
    wake: Option<rune::Waker>,
}

impl Session {
    pub fn start(&mut self, fixture: bool, wake: rune::Waker) {
        self.wake = Some(wake.clone());
        if fixture || cfg!(any(test, debug_assertions)) || self.started { return; }
        self.started = true;
        if let Ok(detail) = std::env::var(HANDOFF) {
            self.stage = Stage::Notice(format!("Desktop {} reopened. {RECONNECT}\n\n{detail}", env!("CARGO_PKG_VERSION")));
            return;
        }
        self.check(false,wake);
    }
    pub fn manual(&mut self) {
        if !matches!(self.stage,Stage::Hidden) {return;}
        if let Some(wake)=self.wake.clone() { self.stage=Stage::Checking; self.check(true,wake); }
    }
    fn check(&mut self, manual:bool, wake:rune::Waker) {
        let (send, recv) = mpsc::channel();
        let (approve, wait) = mpsc::channel();
        let worker = std::thread::Builder::new().name("desktop-update".into()).spawn(move || -> Option<()> {
            let path = std::env::current_exe().ok().and_then(|p|p.canonicalize().ok());
            let managed = path.as_deref().and_then(managed_installer);
            let exe = path.filter(|p|package_cli(p).is_some());
            if !manual && exe.is_none() && managed.is_none(){return None;}
            let current=Version::parse(env!("CARGO_PKG_VERSION")).ok()?;
            let latest = latest();
            if !manual && !latest.as_ref().is_some_and(|v| v.cmp_precedence(&current).is_gt()) { return None; }
            let stage=match latest {
                Some(v) if v.cmp_precedence(&current).is_gt() && exe.is_some()=>Stage::Offer(v),
                Some(v) if v.cmp_precedence(&current).is_gt() && managed.is_some()=>Stage::Notice(format!("Version {v} is available. {} {RECONNECT}", managed.unwrap())),
                Some(v) if v.cmp_precedence(&current).is_gt()=>Stage::Notice(format!("Version {v} is available. This development build cannot update itself.")),
                Some(_) => Stage::Notice(format!("You're up to date (version {current}).")),
                None => Stage::Notice("Couldn't check GitHub Releases. Check your connection and try again.".into()),
            };
            let offered=matches!(stage,Stage::Offer(_));
            send.send(stage).ok()?;
            wake();
            if !offered{return None;}
            wait.recv().ok()?;
            send.send(Stage::Finished(install(&exe?))).ok()?;
            wake();
            Some(())
        });
        if worker.is_ok() { self.inbox = Some(recv); self.consent = Some(approve); }
        else if manual {self.stage=Stage::Notice("Could not start the update check.".into());}
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
    fn signed_app_uses_managed_installer() {
        let exe = Path::new("/Applications/Renamed.app/Contents/MacOS/neochat-desktop");
        assert!(managed_installer(exe).unwrap().contains("DMG"));
        assert!(package_cli(exe).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn portable_handoff() {
        use std::os::unix::fs::PermissionsExt;
        // Only the disposable test subprocess may exec; its parent observes which
        // binary replaced it, the working directory and the post-update notice.
        const CHILD: &str = "NEOBOARD_TEST_HANDOFF_BUNDLE";
        if let Some(root) = std::env::var_os(CHILD) {
            let result = install(&Path::new(&root).join("neochat-desktop"));
            if std::env::var_os("NEOBOARD_TEST_NO_REPLACEMENT").is_some() {
                assert!(result.unwrap_err().contains("No replacement desktop"));
                return;
            }
            panic!("handoff returned: {result:?}");
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let dir = root.join("installed bundle");
        fs::create_dir(&dir).unwrap();
        for name in ["neochat-desktop", "neoboard", "neochat", "neochatd", "neochat-mcp", "neochat-share"] {
            let file = dir.join(name);
            fs::write(&file, "#!/bin/sh\nexit 91\n").unwrap();
            fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(dir.join("LICENSE"), "fixture").unwrap();
        fs::write(dir.join("neoboard"), "#!/bin/sh\n[ \"$1\" = update ] || exit 1\n").unwrap();
        let test = "release_update::tests::portable_handoff";
        let noop = Command::new(std::env::current_exe().unwrap()).args(["--exact", test])
            .env(CHILD, &dir).env_remove("NEOBOARD_PORTABLE_HANDOFF").env("NEOBOARD_TEST_NO_REPLACEMENT", "1").status().unwrap();
        assert!(noop.success());
        fs::write(dir.join("neoboard"), "#!/bin/sh\nset -e\n[ \"$1\" = update ]\nmv ../replacement neochat-desktop\necho fake-swap >&2\n").unwrap();
        fs::write(root.join("replacement"), "#!/bin/sh\nprintf '%s\\n' \"$0\" \"$PWD\" \"$NEOBOARD_DESKTOP_UPDATE_HANDOFF\" \"$@\" > observed\n").unwrap();
        fs::set_permissions(root.join("replacement"), fs::Permissions::from_mode(0o755)).unwrap();
        let supervised = Command::new(std::env::current_exe().unwrap()).args(["--exact", test])
            .env(CHILD, &dir).env("NEOBOARD_PORTABLE_HANDOFF", "1").current_dir(&root).status().unwrap();
        assert_eq!(supervised.code(), Some(75));
        assert!(root.join("replacement").exists(), "supervised desktop must not invoke updater");
        let status = Command::new(std::env::current_exe().unwrap()).args(["--exact", test])
            .env(CHILD, &dir).env_remove("NEOBOARD_PORTABLE_HANDOFF").current_dir(&root).status().unwrap();
        assert!(status.success());
        assert_eq!(fs::read_to_string(root.join("observed")).unwrap(), format!("{}/neochat-desktop\n{}\nfake-swap\n--exact\n{test}\n",dir.display(),root.display()));
    }

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
            assert_eq!(stable_version(json.as_bytes()).is_some_and(|v|v.cmp_precedence(&current).is_gt()), offer, "{tag}");
        }
        let valid = br#"{"tag_name":"v2.0.0","draft":false,"prerelease":false}"#;
        let mut oversized = valid.to_vec();
        oversized.resize(LIMIT as usize + 1, b' ');
        assert!(stable_version(&oversized[..]).is_none());
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