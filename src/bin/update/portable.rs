//! The only stop capability is a pipe to a daemon we spawned. No PID, discovery
//! file, executable search or management API participates in the handoff.
use super::{check_install, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc, thread,
    time::{Duration, Instant},
};

#[derive(clap::Args)]
pub(crate) struct Options {
    /// Absolute path in a trusted parent; must not exist on first launch.
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long)]
    project: Option<String>,
}

// The supervisor executes siblings and trusts its session marker. Neither
// should be replaceable by another account in a shared writable directory.
#[cfg(unix)]
fn trusted(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let m = fs::symlink_metadata(path)?;
    if m.file_type().is_symlink() || m.mode() & 0o022 != 0 || (m.uid() != 0 && m.uid() != unsafe { libc::geteuid() }) {
        return Err(format!("untrusted portable path: {}",path.display()).into());
    }
    Ok(())
}

fn bind_data(dir: &Path, data: &Path) -> Result<File> {
    if !data.is_absolute() || data.file_name().is_none() {
        return Err("specify an absolute portable data directory".into());
    }
    let parent = data.parent().unwrap().canonicalize()?;
    let path = parent.join(data.file_name().unwrap());
    let default = neochat::default_data_dir();
    let default = default.canonicalize().unwrap_or(default);
    if path != data || path.starts_with(dir) || dir.starts_with(&path)
        || default.starts_with(&path) || path.starts_with(default)
    {
        return Err("use a dedicated canonical data path outside the installation and default board".into());
    }
    super::check_not_dev(&path)?;
    #[cfg(unix)]
    trusted(&parent)?;
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)] {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    let marker = path.join("portable-v1.lock");
    let binding = serde_json::to_vec(&(dir, data))?;
    match builder.create(&path) {
        Ok(()) => {
            let mut f = OpenOptions::new().create_new(true).write(true).open(marker)?;
            f.try_lock()?;
            f.write_all(&binding)?;
            f.sync_all()?;
            Ok(f)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(&path)?.is_dir() || !fs::symlink_metadata(&marker).is_ok_and(|m|m.is_file()) {
                return Err("not a managed data directory; will not adopt an existing board".into());
            }
            #[cfg(unix)] {
                trusted(&path)?;
                for entry in path.read_dir()? {
                    use std::os::unix::fs::MetadataExt;
                    let entry = entry?;
                    trusted(&entry.path())?;
                    if entry.file_type()?.is_file() && entry.metadata()?.nlink() != 1 {
                        return Err("unexpected hard link in portable data directory".into());
                    }
                }
            }
            let mut f = OpenOptions::new().read(true).write(true).open(marker)?;
            f.try_lock()?;
            let mut stored = Vec::new();
            (&mut f).take(binding.len() as u64 + 1).read_to_end(&mut stored)?;
            if stored != binding { return Err("portable directory belongs to another installation or requires migration".into()); }
            Ok(f)
        }
        Err(e) => Err(e.into()),
    }
}

fn binary(dir: &Path, name: &str) -> Command {
    Command::new(dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)))
}

fn compatibility(dir: &Path) -> Result<()> {
    // Probe only installed/verified code. No board arguments, credentials, or
    // discovery records are used to decide compatibility.
    let out = binary(dir, "neochatd").arg("--portable-v1-check").stdin(Stdio::null()).output()?;
    if !out.status.success() || out.stdout != b"neoboard-portable-v1\n" {
        return Err("no compatible portable data contract; back up and migrate manually".into());
    }
    Ok(())
}

fn stop(mut daemon: Child) -> Result<()> {
    drop(daemon.stdin.take()); // EOF goes only to our direct child, even after PID reuse.
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = daemon.try_wait()? {
            return if status.success() { Ok(()) } else { Err("companion did not exit cleanly".into()) };
        }
        if Instant::now() >= until { return Err("companion still draining; not activating or forcing a stop".into()); }
        thread::sleep(Duration::from_millis(30));
    }
}

fn check_hold(data: &Path) -> Result<()> {
    match fs::symlink_metadata(data.join("portable-update.hold")) {
        Ok(_) => Err("portable-update.hold exists; finish backup/migration review manually".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn session(dir: &Path, options: &Options, reopened: bool) -> Result<bool> {
    check_hold(&options.data_dir)?;
    let mut daemon = binary(dir,"neochatd").args(["--portable-v1", "--bind", "127.0.0.1:0", "--data-dir"])
        .arg(&options.data_dir).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
    let mut ready = daemon.stdout.take().unwrap();
    let (tx,rx) = mpsc::channel();
    thread::spawn(move || {
        let mut line = [0;6];
        let _ = tx.send(ready.read_exact(&mut line).is_ok() && &line == b"READY\n");
    });
    let result = (|| -> Result<bool> {
        if rx.recv_timeout(Duration::from_secs(15)) != Ok(true) || daemon.try_wait()?.is_some() {
            return Err("owned companion did not become ready; refusing to attach a desktop".into());
        }
        let mut desktop = binary(dir,"neochat-desktop");
        desktop.env("NEOBOARD_PORTABLE_HANDOFF","1").arg("--data-dir").arg(&options.data_dir);
        desktop.env_remove("NEOBOARD_DESKTOP_UPDATE_HANDOFF");
        if reopened { desktop.env("NEOBOARD_DESKTOP_UPDATE_HANDOFF", "Portable companion restarted."); }
        if let Some(p) = &options.profile { desktop.arg("--profile").arg(p); }
        if let Some(p) = &options.project { desktop.arg("--project").arg(p); }
        let status = desktop.stdin(Stdio::null()).status()?;
        if daemon.try_wait()?.is_some() { return Err("lost companion before handoff".into()); }
        match status.code() {
            Some(75) => Ok(true), Some(0) => Ok(false), _ => Err("desktop exited abnormally; not updating".into())
        }
    })();
    stop(daemon)?;
    result
}

pub(crate) fn run(options: Options) -> Result<()> {
    if !cfg!(any(all(target_os="macos",target_arch="aarch64"),all(target_os="linux",target_arch="x86_64"))) {
        return Err("portable supervision not supported on this platform; use manual update".into());
    }
    for key in ["NEOCHAT_URL","NEOCHAT_TOKEN","NEOCHAT_DAEMON","NEOCHAT_BIND","NEOCHAT_DATA_DIR", "NEOBOARD_PORTABLE_HANDOFF"] {
        if std::env::var_os(key).is_some() { return Err(format!("unset {key}; refusing ambiguous session configuration").into()); }
    }
    supervise(&super::install_directory()?, options, super::prepare)
}

fn supervise(dir: &Path, options: Options, mut prepare: impl FnMut(&Path) -> Result<tempfile::TempDir>) -> Result<()> {
    #[cfg(unix)]
    let mut lease = super::lock_bundle(dir)?;
    check_install(dir,std::env::consts::EXE_SUFFIX)?;
    #[cfg(unix)] {
        trusted(dir.parent().ok_or("no installation parent")?)?;
        trusted(dir)?;
        for entry in dir.read_dir()? { trusted(&entry?.path())?; }
    }
    compatibility(dir)?;
    let _data_lease = bind_data(dir,&options.data_dir)?;
    let mut reopened = false;
    while session(dir,&options,reopened)? {
        // Refuse a service that took over while the old companion exited. The
        // successor must acquire this lock itself before announcing readiness.
        let board_lock = OpenOptions::new().write(true).open(options.data_dir.join("service.lock"))?;
        board_lock.try_lock()?;
        check_hold(&options.data_dir)?;
        let stage = prepare(dir)?;
        compatibility(stage.path())?;
        check_hold(&options.data_dir)?;
        #[cfg(unix)]
        let next = super::lock_bundle(stage.path())?;
        super::activate(dir,stage)?;
        reopened = true;
        #[cfg(unix)]
        { drop(lease); lease = next; }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn release(dir: &Path, exit: u8, contract: &str) {
        for name in super::super::BINS { fs::write(dir.join(name), b"fixture").unwrap(); }
        fs::write(dir.join("LICENSE"), b"fixture").unwrap();
        for (name, body) in [
            ("neochatd", format!("if [ \"$1\" = --portable-v1-check ]; then echo '{contract}'; exit 0; fi\n[ \"$1 $2 $3 $4\" = '--portable-v1 --bind 127.0.0.1:0 --data-dir' ] || exit 8\ntouch \"$5/service.lock\"\necho ready >> \"$5/log\"\necho READY\nread -r line && exit 9\necho stopped >> \"$5/log\"")),
            ("neochat-desktop", format!("[ \"$NEOBOARD_PORTABLE_HANDOFF,$1,$3,$4,$5,$6\" = '1,--data-dir,--profile,Board User,--project,project' ] || exit 8\necho desktop-{exit} >> \"$2/log\"\nexit {exit}")),
        ] {
            fs::write(dir.join(name),format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(dir.join(name),fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    #[test]
    fn lifecycle_preserves_config_and_refuses_migration() {
        for compatible in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let root = root.path().canonicalize().unwrap();
            let install = root.join("install");
            let data = root.join("managed data");
            fs::create_dir(&install).unwrap();
            release(&install,75,"neoboard-portable-v1");
            let options = Options { data_dir:data.clone(), profile:Some("Board User".into()), project:Some("project".into()) };
            let result = supervise(&install,options,|dir| {
                assert!(super::super::lock_bundle(dir).is_err());
                let stage = tempfile::tempdir_in(&root)?;
                release(stage.path(),0,if compatible {"neoboard-portable-v1"} else {"migration-required"});
                Ok(stage)
            });
            assert_eq!(result.is_ok(),compatible,"{result:?}");
            let mut expected = String::from("ready\ndesktop-75\nstopped\n");
            if compatible { expected.push_str("ready\ndesktop-0\nstopped\n"); }
            assert_eq!(fs::read_to_string(data.join("log")).unwrap(),expected);
        }
    }

    #[test]
    fn refuses_unmanaged_busy_and_rebound_data() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let data = root.join("data");
        let install = root.join("release");
        fs::create_dir(&data).unwrap();
        assert!(bind_data(&install,&data).is_err());
        fs::remove_dir(&data).unwrap();
        let lease = bind_data(&install,&data).unwrap();
        assert!(bind_data(&install,&data).is_err());
        drop(lease);
        assert!(bind_data(&root.join("different"),&data).is_err());
        assert!(bind_data(&install,&data).is_ok());
        fs::write(data.join("portable-update.hold"),b"").unwrap();
        assert!(check_hold(&data).is_err());
    }

    #[test]
    #[ignore = "set NEOBOARD_TEST_DAEMON to an explicitly built local daemon"]
    fn real_companion_graceful_restart_and_busy_refusal() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let install = root.join("bundle");
        let data = root.join("data");
        fs::create_dir(&install).unwrap();
        let daemon = std::env::var_os("NEOBOARD_TEST_DAEMON").unwrap();
        let bundle = |dir: &Path, code| {
            release(dir,code,"");
            fs::copy(&daemon,dir.join("neochatd")).unwrap();
        };
        bundle(&install,75);
        let options = || Options { data_dir:data.clone(),profile:Some("Board User".into()),project:Some("project".into()) };
        supervise(&install,options(),|_| {
            assert!(!data.join("endpoint.json").exists());
            assert!(data.join("board.sqlite3").is_file());
            let next = tempfile::tempdir_in(&root)?;
            bundle(next.path(),0);
            Ok(next)
        }).unwrap();
        assert_eq!(fs::read_to_string(data.join("log")).unwrap(),"desktop-75\ndesktop-0\n");
        assert!(!data.join("endpoint.json").exists());

        // Only the test owns the independent service's shutdown pipe.
        let mut other = binary(&install,"neochatd").args(["--portable-v1","--bind","127.0.0.1:0","--data-dir"]).arg(&data)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let mut ready = [0;6];
        other.stdout.take().unwrap().read_exact(&mut ready).unwrap();
        assert_eq!(&ready,b"READY\n");
        assert!(supervise(&install,options(),|_|panic!("must not stage")).is_err());
        assert!(other.try_wait().unwrap().is_none());
        assert!(data.join("endpoint.json").exists());
        stop(other).unwrap();
        assert!(!data.join("endpoint.json").exists());
    }
}