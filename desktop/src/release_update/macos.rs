//! In-place replacement for the bundle produced by scripts/package-release.py.
use semver::Version;
use std::{
    env,
    ffi::{CString, c_char, c_uint},
    fs, io,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tempfile::{Builder, TempDir};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const APP: &str = "NeoBoard.app";
const EXE: &str = "Contents/MacOS/neochat-desktop";
const ID: &str = "com.parkers0405.neoboard";

fn output(cmd: &mut Command) -> Result<Vec<u8>> {
    let out = cmd.stdin(Stdio::null()).output()?;
    if !out.status.success() {
        return Err(format!("{cmd:?}: {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()).into());
    }
    Ok(out.stdout)
}

fn private_dir(parent: &Path) -> io::Result<TempDir> {
    Builder::new().prefix(".neoboard-update-")
        .permissions(fs::Permissions::from_mode(0o700)).tempdir_in(parent)
}

fn installed_bundle(exe: &Path) -> Result<&Path> {
    let app = exe.ancestors().nth(3).filter(|p| {
        p.extension().is_some_and(|s| s.eq_ignore_ascii_case("app")) && p.join(EXE) == exe
    }).ok_or("not running from an installed NeoBoard bundle")?;
    if ["/Volumes", "/private/var/folders", "/private/tmp", "/private/var/tmp"].iter().any(|p| app.starts_with(p))
        || app.components().any(|c| c.as_os_str().eq_ignore_ascii_case("AppTranslocation"))
        || env::temp_dir().canonicalize().is_ok_and(|p| app.starts_with(p))
    {
        return Err("copy NeoBoard to Applications and launch that copy before updating".into());
    }
    Ok(app)
}

fn check_key(app: &Path, key: &str, expected: &str) -> Result<()> {
    let value = output(Command::new("/usr/bin/plutil").args(["-extract", key, "raw", "-expect", "string", "-n", "-o", "-", "--"])
        .arg(app.join("Contents/Info.plist")))?;
    if value != expected.as_bytes() {
        return Err(format!("unexpected {key} in {} (expected {expected})", app.display()).into());
    }
    Ok(())
}

fn validate(app: &Path, version: &Version, requirement: &str) -> Result<()> {
    for dir in ["", "Contents", "Contents/MacOS"] {
        if !fs::symlink_metadata(if dir.is_empty() { app.to_owned() } else { app.join(dir) })?.is_dir() {
            return Err("unexpected bundle directory or symlink".into());
        }
    }
    if !fs::symlink_metadata(app.join("Contents/Info.plist"))?.is_file() {
        return Err("expected regular Info.plist".into());
    }
    for name in ["neochat-desktop", "neoboard", "neochat", "neochatd", "neochat-mcp", "neochat-share"] {
        let meta = fs::symlink_metadata(app.join("Contents/MacOS").join(name))?;
        if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
            return Err(format!("expected packaged executable: {name}").into());
        }
    }
    output(Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict", "--all-architectures"]).arg(app))?;
    // A leading '=' makes codesign parse the requirement as text, not a filename.
    output(Command::new("/usr/bin/codesign").args(["--verify", "--all-architectures", "-R"]).arg(format!("={requirement}")).arg(app))?;
    let version = version.to_string();
    for (key, value) in [("CFBundleIdentifier", ID), ("CFBundleExecutable", "neochat-desktop"),
        ("CFBundlePackageType", "APPL"), ("CFBundleShortVersionString", &version), ("CFBundleVersion", &version)] {
        check_key(app, key, value)?;
    }
    Ok(())
}

// Production builds must continue to satisfy the installed Developer ID requirement.
// Local ad-hoc builds have a per-build cdhash instead; the caller verified the DMG.
fn requirement(app: &Path) -> Result<String> {
    let info = Command::new("/usr/bin/codesign").args(["-d", "-r-"]).arg(app).output()?;
    if !info.status.success() { return Err("cannot inspect installed NeoBoard signature".into()); }
    let text = String::from_utf8(info.stdout)?;
    let dr = text.lines().find_map(|line| line.trim_start_matches("# ").strip_prefix("designated => ")).ok_or("missing installed signing requirement")?;
    if dr.starts_with("cdhash ") { Ok(format!("identifier \"{ID}\"")) } else { Ok(dr.to_owned()) }
}

struct Mount { point: PathBuf, attached: bool }
impl Drop for Mount {
    fn drop(&mut self) {
        if self.attached {
            let _ = output(Command::new("/usr/bin/hdiutil").args(["detach", "-force"]).arg(&self.point));
        }
    }
}

fn stage(dmg: &Path, staged: &Path, requirement: &str, version: &Version) -> Result<()> {
    let tmp = private_dir(&env::temp_dir())?;
    let point = tmp.path().join("volume");
    fs::create_dir(&point)?;
    output(Command::new("/usr/bin/hdiutil").args(["attach", "-readonly", "-nobrowse", "-noautoopen", "-mountpoint"]).arg(&point).arg(dmg))?;
    let mut mount = Mount { point, attached: true };
    let source = mount.point.join(APP);
    validate(&source, version, requirement)?;
    output(Command::new("/usr/bin/ditto").arg(source).arg(staged))?;
    output(Command::new("/usr/bin/hdiutil").arg("detach").arg(&mount.point))?;
    // Disarm the fallback detach only after hdiutil reports success.
    mount.attached = false;
    tmp.close()?;
    validate(staged, version, requirement)
}

fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    unsafe extern "C" { fn renamex_np(a: *const c_char, b: *const c_char, flags: c_uint) -> i32; }
    let a = CString::new(a.as_os_str().as_bytes())?;
    let b = CString::new(b.as_os_str().as_bytes())?;
    // Darwin <stdio.h>: RENAME_SWAP atomically exchanges both existing names.
    if unsafe { renamex_np(a.as_ptr(), b.as_ptr(), 0x2) } != 0 { return Err(io::Error::last_os_error()); }
    Ok(())
}

fn replace(target: &Path, slot: TempDir, launch: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let staged = slot.path().join(APP);
    exchange(&staged, target)?;
    if let Err(error) = launch(slot.path()) {
        if let Err(rollback) = exchange(&staged, target) {
            let backup = slot.keep().join(APP);
            return Err(format!("relaunch failed: {error}; rollback failed: {rollback}; original retained at {}", backup.display()).into());
        }
        return Err(format!("relaunch failed; original restored: {error}").into());
    }
    Ok(())
}

pub(super) fn apply(dmg: &Path, version: &Version) -> Result<()> {
    let exe = env::current_exe()?.canonicalize()?;
    let app = installed_bundle(&exe)?;
    let parent = app.parent().ok_or("no installation directory")?;
    let lock = fs::File::open(parent)?;
    lock.try_lock()?;
    check_key(app, "CFBundleIdentifier", ID)?;
    let slot = private_dir(parent)?;
    stage(dmg, &slot.path().join(APP), &requirement(app)?, version)?;
    // exec skips the caller's TempDir destructor. Remove only the downloaded file
    // and its empty private download directory, never an arbitrary subtree.
    fs::remove_file(dmg)?;
    if let Some(dir) = dmg.parent().filter(|p| p.file_name().is_some_and(|n| n.as_bytes().starts_with(b"neoboard-installer-"))) {
        fs::remove_dir(dir)?;
    }
    let cwd = env::current_dir()?;
    replace(app, slot, |backup_dir| {
        // Successful exec closes the parent's pipe (CLOEXEC). Until then keep
        // the old app available for rollback. The helper sees no shell path input.
        let mut cleaner = Command::new("/bin/sh").env_clear().args(["-c", "IFS= read -r cancel || /bin/rm -rf -- \"$1\"", "neoboard-cleanup"])
            .arg(backup_dir).current_dir("/").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        let error = Command::new(&exe).args(env::args_os().skip(1)).current_dir(cwd)
            .env("NEOBOARD_DESKTOP_UPDATE_HANDOFF", format!("Updated to {version}." )).exec();
        // Keep stdin open until the helper is stopped, so it cannot delete the backup.
        let _ = cleaner.kill();
        let pipe = cleaner.stdin.take();
        let _ = cleaner.wait();
        drop(pipe);
        Err(error.into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_packaged_ad_hoc_dmg_and_reject_wrong_version() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("image");
        let app = root.join(APP);
        let bin = app.join("Contents/MacOS");
        fs::create_dir_all(&bin)?;
        for name in ["neochat-desktop", "neoboard", "neochat", "neochatd", "neochat-mcp", "neochat-share"] {
            let file = bin.join(name);
            fs::copy("/bin/echo", &file)?;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o755))?;
            output(Command::new("/usr/bin/codesign").args(["-f", "-s", "-"]).arg(file))?;
        }
        let plist = app.join("Contents/Info.plist");
        output(Command::new("/usr/bin/plutil").args(["-create", "xml1"]).arg(&plist))?;
        for (key, value) in [("CFBundleIdentifier", ID), ("CFBundleExecutable", "neochat-desktop"), ("CFBundlePackageType", "APPL"),
            ("CFBundleShortVersionString", "1.2.3"), ("CFBundleVersion", "1.2.3")] {
            output(Command::new("/usr/bin/plutil").args(["-insert", key, "-string", value]).arg(&plist))?;
        }
        output(Command::new("/usr/bin/codesign").args(["-f", "-s", "-"]).arg(&app))?;
        let dmg = tmp.path().join("test.dmg");
        output(Command::new("/usr/bin/hdiutil").args(["create", "-format", "UDZO", "-srcfolder"]).arg(root).arg(&dmg))?;
        let req = requirement(&app)?;
        let staged = tmp.path().join(APP);
        stage(&dmg, &staged, &req, &Version::new(1, 2, 3))?;
        assert!(validate(&staged, &Version::new(1, 2, 4), &req).is_err());
        Ok(())
    }

    #[test]
    fn real_bundle_exchange_rolls_back_only_on_launch_failure() {
        for fail in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let app = root.path().join("Installed.app");
            let slot = private_dir(root.path()).unwrap();
            let backup_dir = slot.path().to_owned();
            for (bundle, bytes) in [(&app, "old"), (&backup_dir.join(APP), "new")] {
                fs::create_dir(bundle).unwrap();
                fs::write(bundle.join("payload"), bytes).unwrap();
            }
            let result = replace(&app, slot, |backup| {
                assert_eq!(fs::read(app.join("payload"))?, b"new");
                assert_eq!(fs::read(backup.join(APP).join("payload"))?, b"old");
                if fail { return Err("simulated exec failure".into()); }
                Ok(())
            });
            assert_eq!(result.is_err(), fail);
            assert_eq!(fs::read(app.join("payload")).unwrap(), if fail { b"old" } else { b"new" });
            assert!(!backup_dir.exists());
        }
    }
}