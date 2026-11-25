//! A check only offers an action. Only local consent can release its worker.
mod managed;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
pub use managed::Installer;
use std::{fs, io::Read, path::{Path, PathBuf}, process::Stdio, sync::mpsc, time::Duration};
#[cfg(unix)]
use std::process::Command;
use semver::Version;

const LATEST: &str = "https://api.github.com/repos/parkers0405/neoboard/releases/latest";
const LIMIT: u64 = 256 * 1024;
const HANDOFF: &str = "NEOBOARD_DESKTOP_UPDATE_HANDOFF";
pub const RECONNECT: &str = "External MCP connections are not verified. Reconnect Neoboard in each MCP client to load the updated adapter.";

// WiX installs in ProgramFiles64Folder/NeoBoard. Other Program Files paths
// remain excluded from portable staging, but are not evidence of an MSI install.
#[cfg(any(windows, test))]
fn program_files_location(dir: &Path, root: &Path, x64: bool) -> Option<bool> {
    let eq = |a: &Path, b: &Path| a == b || a.as_os_str().eq_ignore_ascii_case(b.as_os_str());
    dir.ancestors().any(|p| eq(p, root)).then(|| x64 && eq(dir, &root.join("NeoBoard")))
}

#[cfg(windows)]
fn in_program_files(dir: &Path) -> Option<bool> {
    [("ProgramW6432", true), ("ProgramFiles", cfg!(target_pointer_width = "64")), ("ProgramFiles(x86)", false)]
        .into_iter().filter_map(|(var, x64)| Some((PathBuf::from(std::env::var_os(var)?).canonicalize().ok()?, x64)))
        .find_map(|(root, x64)| program_files_location(dir, &root, x64))
}

fn managed_installer(exe: &Path) -> Option<Installer> {
    let dir = exe.parent()?;
    // The DMG's signed app also contains all six portable executables.
    if cfg!(target_os = "macos") && dir.ends_with("Contents/MacOS") && dir.ancestors().nth(2)?.extension().is_some_and(|s| s.eq_ignore_ascii_case("app")) {
        return Some(Installer::Dmg);
    }
    #[cfg(windows)]
    if in_program_files(dir) == Some(true) {
        return Some(Installer::Msi);
    }
    None
}

const RELEASES: &str = "https://github.com/parkers0405/neoboard/releases";

struct Release { version: Version, v_prefix: bool, assets: Vec<String> }

impl Release {
    fn tag(&self) -> String { format!("{}{}", if self.v_prefix { "v" } else { "" }, self.version) }

    fn offer(&self, action: Option<Update>) -> Stage {
        match action {
            Some(Update::Managed(installer)) if !installer.available(self) => (),
            Some(action) => return Stage::Offer(self.version.clone(), action),
            None => (),
        }
        // Never use html_url or browser_download_url supplied by release metadata.
        Stage::ManualRelease(self.version.clone(), format!("{RELEASES}/tag/{}", self.tag()))
    }
}

fn stable_release(body: impl Read) -> Option<Release> {
    #[derive(serde::Deserialize)]
    struct Metadata { tag_name: String, draft: bool, prerelease: bool, #[serde(default)] assets: Vec<Asset> }
    #[derive(serde::Deserialize)]
    struct Asset { name: String }
    let mut data = Vec::new();
    body.take(LIMIT + 1).read_to_end(&mut data).ok()?;
    if data.len() as u64 > LIMIT { return None; }
    let release: Metadata = serde_json::from_slice(&data).ok()?;
    let tag = release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name);
    let version = Version::parse(tag).ok()?;
    (!release.draft && !release.prerelease && version.pre.is_empty()).then_some(Release {
        version, v_prefix: release.tag_name.starts_with('v'), assets: release.assets.into_iter().map(|a| a.name).collect(),
    })
}

fn latest() -> Option<Release> {
    let client = reqwest::blocking::Client::builder().https_only(true)
        .redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(8))
        .user_agent(concat!("neoboard-desktop/", env!("CARGO_PKG_VERSION"))).build().ok()?;
    let response = client.get(LATEST).send().ok()?;
    if response.status() != reqwest::StatusCode::OK { return None; }
    stable_release(response)
}

fn binary(name: &str) -> String { format!("{name}{}", std::env::consts::EXE_SUFFIX) }

fn package_cli(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    if managed_installer(exe).is_some() { return None; }
    #[cfg(windows)]
    if in_program_files(dir).is_some() { return None; }
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
    let mut command = crate::background_process::command(cli);
    command.arg("update").current_dir(exe.parent().unwrap()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Update { Portable, Managed(Installer) }

impl Update {
    pub fn label(self) -> &'static str {
        match self {
            Self::Managed(_) => "Update and restart",
            Self::Portable if cfg!(unix) => "Update and restart",
            Self::Portable => "Stage update",
        }
    }
    pub fn instructions(self) -> &'static str {
        match self {
            Self::Managed(_) => "Save your drafts before restarting. NeoBoard will verify the download, install it, and reopen. If you host a board, stop its daemon and back up its data first.",
            Self::Portable if cfg!(unix) => "Finish your drafts first. Updating will reopen the installed desktop. Reconnect external MCP clients after updating.",
            Self::Portable => "The update will stage a separate bundle. Switch paths and reconnect external MCP clients manually.",
        }
    }
}

#[derive(Default)]
pub enum Stage { #[default] Hidden, Checking, Notice(String), ManualRelease(Version, String), Offer(Version, Update), Running(&'static str), Finished(Result<String, String>) }

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
        if fixture { return; }
        self.wake = Some(wake.clone());
        if cfg!(any(test, debug_assertions)) || self.started { return; }
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
            if !manual && !latest.as_ref().is_some_and(|r| r.version.cmp_precedence(&current).is_gt()) { return None; }
            let action = match (std::env::consts::OS, std::env::consts::ARCH, managed) {
                ("windows", "x86_64", installer) => installer.map(Update::Managed),
                ("macos", "aarch64", _) | ("linux", "x86_64", _) =>
                    managed.map(Update::Managed).or_else(||exe.as_ref().map(|_|Update::Portable)),
                _ => None,
            };
            let stage=match &latest {
                Some(r) if r.version.cmp_precedence(&current).is_gt() => r.offer(action),
                Some(_) => Stage::Notice(format!("You're up to date (version {current}).")),
                None => Stage::Notice("Couldn't check GitHub Releases. Check your connection and try again.".into()),
            };
            let offered=matches!(stage,Stage::Offer(..));
            send.send(stage).ok()?;
            wake();
            if !offered{return None;}
            wait.recv().ok()?;
            let result = match action? {
                Update::Portable => Stage::Finished(install(&exe?)),
                Update::Managed(installer) => match installer.start(&latest?, || {
                    let _ = send.send(Stage::Running("Download verified. Installing the update…"));
                    wake();
                }) {
                    // The platform helper now owns installation and relaunch.
                    Ok(()) => std::process::exit(0),
                    Err(e) => Stage::Finished(Err(e.to_string())),
                },
            };
            send.send(result).ok()?;
            wake();
            Some(())
        });
        if worker.is_ok() { self.inbox = Some(recv); self.consent = Some(approve); }
        else if manual {self.stage=Stage::Notice("Could not start the update check.".into());}
    }
    pub fn accept(&mut self) {
        let Stage::Offer(_, action) = self.stage else { return; };
        self.stage = if self.consent.take().is_some_and(|tx| tx.send(()).is_ok()) { Stage::Running(match action {
            Update::Managed(_) => "Downloading and verifying the release…",
            Update::Portable => "Preparing the new desktop bundle…",
        }) }
            else { Stage::Finished(Err("No packaged updater is available.".into())) };
    }
    pub fn dismiss(&mut self) {
        if matches!(self.stage, Stage::Running(_)) { return; }
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
                    if matches!(self.stage, Stage::Running(_)) {
                        self.stage = Stage::Finished(Err("The updater stopped without a result.".into()));
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        if matches!(self.stage, Stage::Running(_)) {
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
    fn only_wix_folder_gets_the_msi_action() {
        let root = Path::new("Program Files");
        assert_eq!(program_files_location(&root.join("neoboard"), root, true), Some(true));
        for other in ["", "Other", "NeoBoard/portable"] {
            assert_eq!(program_files_location(&root.join(other), root, true), Some(false));
        }
        assert_eq!(program_files_location(&root.join("NeoBoard"), root, false), Some(false));
        assert_eq!(program_files_location(Path::new("portable/NeoBoard"), root, true), None);
    }

    #[test]
    fn missing_asset_or_unsupported_build_only_links_to_the_offered_release() {
        let mut release = stable_release(br#"{"tag_name":"v2.0.0","draft":false,"prerelease":false,"html_url":"https://example.org/wrong"}"#.as_slice()).unwrap();
        let msi = Some(Update::Managed(Installer::Msi));
        let mut app = crate::App::new();
        crate::fixture::load(&mut app, true);
        app.dialog = Some(crate::Dialog::Settings);
        release.assets.push("NeoBoard-x86_64.msi".into());
        app.release.stage = release.offer(msi); // missing checksum
        let mut rt = rune::Runtime::new(app, crate::view::root, (760., 520.), rune::Env::new(true, 1.));
        for _ in 0..5 { rt.frame(1./60.); }
        assert!(rt.find("start-update").is_none());
        assert!(rt.take_url_requests().is_empty());
        let (x,y) = rt.find("view-release").unwrap();
        rt.pointer_move(x,y); rt.pointer_down(x,y); rt.pointer_up(x,y); rt.frame(1./60.);
        assert_eq!(rt.take_url_requests(), ["https://github.com/parkers0405/neoboard/releases/tag/v2.0.0"]);
        release.assets.push("NeoBoard-x86_64.msi.sha256".into());
        assert!(matches!(release.offer(msi), Stage::Offer(_, Update::Managed(Installer::Msi))));
        assert!(matches!(release.offer(None), Stage::ManualRelease(..)));
        assert!(matches!(release.offer(Some(Update::Portable)), Stage::Offer(_, Update::Portable)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn signed_app_uses_managed_installer() {
        let exe = Path::new("/Applications/Renamed.app/Contents/MacOS/neochat-desktop");
        assert_eq!(managed_installer(exe), Some(Installer::Dmg));
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
            assert_eq!(stable_release(json.as_bytes()).is_some_and(|r|r.version.cmp_precedence(&current).is_gt()), offer, "{tag}");
        }
        let valid = br#"{"tag_name":"v2.0.0","draft":false,"prerelease":false}"#;
        let mut oversized = valid.to_vec();
        oversized.resize(LIMIT as usize + 1, b' ');
        assert!(stable_release(&oversized[..]).is_none());
    }

    #[test]
    fn dismiss_and_update_are_session_local_and_keep_drafts() {
        let mut app = crate::App::new();
        app.draft = crate::edit("unsent");
        let (approve, rx) = mpsc::channel();
        app.release.consent = Some(approve);
        app.release.stage = Stage::Offer(Version::new(2,0,0), Update::Managed(Installer::Msi));
        app.release.dismiss();
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected)));
        assert!(matches!(app.release.stage, Stage::Hidden));
        let (approve, rx) = mpsc::channel();
        app.release.consent = Some(approve);
        app.release.stage = Stage::Offer(Version::new(2,0,0), Update::Managed(Installer::Msi));
        // Exercise Rune hit testing with Settings still open beneath the offer.
        app.dialog = Some(crate::Dialog::Settings);
        crate::fixture::load(&mut app, true);
        let mut rt = rune::Runtime::new(app, crate::view::root, (760., 520.), rune::Env::new(true, 1.));
        for _ in 0..5 { rt.frame(1./60.); }
        assert!(rx.try_recv().is_err());
        let (x,y) = rt.find("start-update").unwrap();
        rt.pointer_move(x,y); rt.pointer_down(x,y); rt.pointer_up(x,y); rt.frame(1./60.);
        assert_eq!(rx.try_iter().count(), 1);
        rt.state.release.accept();
        assert_eq!(rx.try_iter().count(), 0);
        assert!(matches!(rt.state.release.stage, Stage::Running(_)));
        assert_eq!(rt.state.draft.text, "unsent");
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