//! Native local-runtime delegation. An unsupported native dispatch is not
//! permission to try a different server, a guessed chat or a master credential.
use super::runtime::{validate, Target};
use serde::Deserialize;
use std::{
    env,
    fs::OpenOptions,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Handoff {
    session: String,
    origin: String,
    workspace: PathBuf,
    bearer: String,
}

const BAD: &str = "No valid private Neoism handoff. Use a patched, authenticated local Neoism runtime in the dedicated conversation.";

fn read(path: &Path) -> Result<Handoff, String> {
    if !cfg!(unix) || !path.is_absolute() {
        return Err(BAD.into());
    }
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = opts.open(path).map_err(|_| BAD)?;
    let meta = file.metadata().map_err(|_| BAD)?;
    if !meta.is_file() || meta.len() > 16 * 1024 {
        return Err(BAD.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(BAD.into());
        }
    }
    let mut bytes = vec![];
    file.take(16385).read_to_end(&mut bytes).map_err(|_| BAD)?;
    if bytes.len() > 16384 {
        return Err(BAD.into());
    }
    let handoff: Handoff = serde_json::from_slice(&bytes).map_err(|_| BAD)?;
    if !handoff
        .bearer
        .strip_prefix("neoism-wake-v1.")
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        return Err(BAD.into());
    }
    Ok(handoff)
}

pub(super) fn resolve(
    session: Option<String>,
    workspace: Option<PathBuf>,
    bin: Option<PathBuf>,
    origin: Option<String>,
) -> Result<(Target, Option<String>), String> {
    if bin.is_some() {
        return Err("Neoism pairs to its own local server, not an executable".into());
    }
    let (session, workspace, origin, bearer) = if let Some(path) =
        env::var_os("NEOISM_WAKE_HANDOFF_V1")
    {
        let ctx = read(Path::new(&path))?;
        if session.as_ref().is_some_and(|s| s != &ctx.session)
            || origin.as_ref().is_some_and(|s| s != &ctx.origin)
            || workspace.as_ref().is_some_and(|w| w != &ctx.workspace)
        {
            return Err("Neoism runtime and pairing arguments disagree; nothing was paired".into());
        }
        (ctx.session, ctx.workspace, ctx.origin, ctx.bearer)
    } else {
        // Legacy/manual mode; no endpoint files, session listings or daemon auth discovery.
        let session = session.ok_or(BAD)?;
        if env::var("NEOISM_SESSION_ID")
            .ok()
            .is_some_and(|id| !id.is_empty() && id != session)
        {
            return Err("Neoism conversation IDs disagree".into());
        }
        (
            session,
            workspace
                .or_else(|| env::current_dir().ok())
                .ok_or("Missing workspace")?,
            origin
                .or_else(|| env::var("NEOISM_AGENT_SERVER").ok())
                .ok_or("Missing explicit loopback origin")?,
            env::var("NEOISM_WAKE_TOKEN")
                .map_err(|_| "Missing private manual runtime credential")?,
        )
    };
    let target = Target {
        harness: "neoism".into(),
        session,
        workspace,
        executable: None,
        origin: Some(origin),
    };
    validate(&target)?;
    Ok((target, Some(bearer)))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        io::Write,
        os::unix::fs::{symlink, PermissionsExt},
    };

    #[test]
    fn handoff_is_bounded_private_and_not_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let mut file = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        let body = serde_json::json!({"session":"ses_fixture", "origin":"http://127.0.0.1:1", "workspace":"/", "bearer":format!("neoism-wake-v1.{}", "0".repeat(64))});
        file.write_all(body.to_string().as_bytes()).unwrap();
        assert!(read(file.path()).is_ok());
        let link = dir.path().join("link");
        symlink(file.path(), &link).unwrap();
        assert!(read(&link).is_err());
        fs::set_permissions(file.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read(file.path()).is_err());
        fs::set_permissions(file.path(), fs::Permissions::from_mode(0o600)).unwrap();
        file.as_file().set_len(16385).unwrap();
        assert!(read(file.path()).is_err());
        assert!(read(Path::new("unavailable")).is_err());
    }
}