use sha2::{Digest, Sha256};
pub(super) mod portable;
use std::{
    collections::HashSet,
    error::Error,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    time::Duration,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const BINS: [&str; 6] = [
    "neoboard",
    "neochat",
    "neochatd",
    "neochat-share",
    "neochat-mcp",
    "neochat-desktop",
];
const BASE: &str = "https://github.com/parkers0405/neoboard/releases/latest/download";
const MIB: u64 = 1024 * 1024;
const ZIP_LIMIT: u64 = 256 * MIB;

fn asset(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Ok("neoboard-linux-x86_64.zip"),
        ("macos", "aarch64") => Ok("neoboard-darwin-aarch64.zip"),
        ("windows", "x86_64") => Ok("neoboard-windows-x86_64.zip"),
        _ => Err("no release bundle for this platform".into()),
    }
}

fn digest(text: &str, asset: &str) -> Result<String> {
    let fields: Vec<_> = text.split_ascii_whitespace().collect();
    match fields.as_slice() {
        [hash] | [hash, _]
            if hash.len() == 64
                && hash.bytes().all(|c| c.is_ascii_hexdigit())
                && fields
                    .get(1)
                    .is_none_or(|s| s.strip_prefix('*').unwrap_or(s) == asset) =>
        {
            Ok(hash.to_ascii_lowercase())
        }
        _ => Err("invalid SHA-256 sidecar or wrong archive name".into()),
    }
}

fn binary(name: &str, ext: &str) -> bool {
    name.strip_suffix(ext).is_some_and(|s| BINS.contains(&s))
}

fn allowed(name: &str, ext: &str) -> bool {
    binary(name, ext)
        || matches!(
            name,
            "LICENSE"
                | "LICENSE-Rune"
                | "LICENSE-winit"
                | "OFL.GeistMono.txt"
                | "MCP.md"
                | "LICENSE.txt"
                | "README"
                | "README.md"
                | "NOTICE"
                | "NOTICE.txt"
        )
}

fn complete(names: &HashSet<String>, ext: &str) -> Result<()> {
    if !BINS.iter().all(|s| names.contains(&format!("{s}{ext}"))) || !names.contains("LICENSE") {
        return Err("not a complete release bundle (six programs and LICENSE required)".into());
    }
    Ok(())
}

fn check_not_dev(dir: &Path) -> Result<()> {
    for parent in dir.ancestors() {
        for marker in [".git", "Cargo.toml", ".rustc_info.json"] {
            match fs::symlink_metadata(parent.join(marker)) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
                Ok(_) => return Err("refusing to update inside a development tree".into()),
            }
        }
    }
    Ok(())
}

fn check_install(dir: &Path, ext: &str) -> Result<()> {
    check_not_dev(dir)?;
    let mut names = HashSet::new();
    for entry in dir.read_dir()? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "unexpected install filename")?;
        if !allowed(&name, ext) || !entry.file_type()?.is_file() {
            return Err(
                "install contains non-release files; use a dedicated release directory".into(),
            );
        }
        names.insert(name);
    }
    complete(&names, ext)
}

fn bounded(mut src: impl Read, dest: &mut impl io::Write, limit: u64) -> Result<u64> {
    let n = io::copy(&mut (&mut src).take(limit + 1), dest)?;
    if n > limit {
        return Err("release size limit exceeded".into());
    }
    Ok(n)
}

// Bound directory allocations before passing the file to the general ZIP reader.
// Ordinary single-disk ZIP, including a bounded archive comment; no ZIP64 locator.
fn directory(file: &mut File) -> Result<(usize, u64)> {
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(65535 + 22 + 20)))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    let i = tail
        .windows(4)
        .rposition(|s| s == b"PK\x05\x06")
        .ok_or("missing ZIP directory")?;
    let end = &tail[i..];
    if end.len() < 22
        || end.len() != 22 + u16::from_le_bytes([end[20], end[21]]) as usize
        || end[4..8] != [0; 4]
        || end[8..10] != end[10..12]
        || (i >= 20 && &tail[i - 20..i - 16] == b"PK\x06\x07")
    {
        return Err("unsupported ZIP directory".into());
    }
    let count = u16::from_le_bytes([end[10], end[11]]) as usize;
    if !(7..=16).contains(&count) {
        return Err("unexpected ZIP entry count".into());
    }
    let start = u32::from_le_bytes(end[16..20].try_into()?) as u64;
    let size = u32::from_le_bytes(end[12..16].try_into()?) as u64;
    if start == 0 || size > 65535 || start + size != len - end.len() as u64 {
        return Err("invalid ZIP directory bounds".into());
    }
    file.seek(SeekFrom::Start(start))?;
    let mut metadata = Vec::new();
    file.read_to_end(&mut metadata)?;
    if metadata.windows(4).filter(|s| *s == b"PK\x05\x06").count() != 1 {
        return Err("ambiguous ZIP footer".into());
    }
    file.rewind()?;
    Ok((count, start))
}

// ZIP readers can search backwards for an alternate footer after a parse error.
// Allow local-header checks, but don't let the reader parse a fallback footer
// from the compressed payload. Bound work even on a deliberately malformed ZIP.
struct ArchiveReader<'a> {
    file: &'a mut File,
    floor: &'a std::cell::Cell<u64>,
    budget: usize,
}

impl Seek for ArchiveReader<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.file.seek(from)
    }
}

impl Read for ArchiveReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let floor = self.floor.get();
        if floor != 0 {
            if self.budget < buf.len() {
                return Err(io::Error::other("ZIP metadata read limit exceeded"));
            }
            self.budget -= buf.len();
            let pos = self.file.stream_position()?;
            if pos < floor {
                let mut sig = [0; 4];
                self.file.read_exact(&mut sig)?;
                if sig == *b"PK\x05\x06" {
                    self.budget = 0;
                    return Err(io::Error::other("unexpected ZIP footer"));
                }
                self.file.seek(SeekFrom::Start(pos))?;
            }
        }
        self.file.read(buf)
    }
}

fn extract(file: &mut File, stage: &Path, ext: &str) -> Result<()> {
    let (count, start) = directory(file)?;
    let floor = std::cell::Cell::new(start);
    let mut zip = zip::ZipArchive::new(ArchiveReader {
        file,
        floor: &floor,
        budget: 4 * MIB as usize,
    })?;
    floor.set(0);
    if zip.len() != count {
        return Err("ambiguous ZIP directory".into());
    }
    let mut names = HashSet::new();
    let mut remaining = 512 * MIB;
    for i in 0..count {
        let mut entry = zip.by_index(i)?;
        let name = std::str::from_utf8(entry.name_raw())?.to_owned();
        let kind = entry.unix_mode().unwrap_or(0) & 0o170000;
        if !allowed(&name, ext) || !names.insert(name.clone()) || !matches!(kind, 0 | 0o100000) {
            return Err("unexpected ZIP name or file type".into());
        }
        let size = entry.size();
        if size == 0 || size > remaining.min(192 * MIB) {
            return Err("invalid release entry size".into());
        }
        let mut output = File::create_new(stage.join(&name))?;
        if bounded(&mut entry, &mut output, size)? != size {
            return Err("truncated release entry".into());
        }
        remaining -= size;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            output.set_permissions(fs::Permissions::from_mode(if binary(&name, ext) {
                0o755
            } else {
                0o644
            }))?;
        }
        output.sync_all()?;
    }
    complete(&names, ext)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let a = CString::new(a.as_os_str().as_bytes())?;
    let b = CString::new(b.as_os_str().as_bytes())?;
    // Both strings remain valid throughout this single atomic directory exchange.
    #[cfg(target_os = "macos")]
    let rc = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) };
    #[cfg(target_os = "linux")]
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(super) fn run() -> Result<()> {
    let dir = install_directory()?;
    #[cfg(unix)]
    let _lease = lock_bundle(&dir)?;
    let stage = prepare(&dir)?;
    activate(&dir, stage)
}

#[cfg(unix)]
fn lock_bundle(dir: &Path) -> Result<File> {
    let file = File::open(dir)?;
    file.try_lock().map_err(|e|format!("bundle busy or filesystem cannot lock it: {e}"))?;
    Ok(file)
}

fn install_directory() -> Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    if exe.file_name() != Some(format!("neoboard{}", std::env::consts::EXE_SUFFIX).as_ref()) {
        return Err("updater must be the installed neoboard executable".into());
    }
    Ok(exe.parent().ok_or("no installation directory")?.into())
}

fn prepare(dir: &Path) -> Result<tempfile::TempDir> {
    use std::env::{
        consts::{ARCH, EXE_SUFFIX, OS},
    };
    let asset = asset(OS, ARCH)?;
    let parent = dir.parent().ok_or("cannot update filesystem root")?;
    check_install(dir, EXE_SUFFIX)?;
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .timeout(Duration::from_secs(300))
        .user_agent("neoboard-update")
        .build()?;
    let fetch = |name: &str| {
        client
            .get(format!("{BASE}/{name}"))
            .send()?
            .error_for_status()
    };
    let mut sidecar = Vec::new();
    bounded(fetch(&format!("{asset}.sha256"))?, &mut sidecar, 1024)?;
    let expected = digest(std::str::from_utf8(&sidecar)?, asset)?;
    let mut archive = tempfile::tempfile_in(parent)?;
    bounded(fetch(asset)?, &mut archive, ZIP_LIMIT)?;
    verify(&mut archive, &expected)?;
    let stage = tempfile::Builder::new()
        .prefix("neoboard-release-")
        .tempdir_in(parent)?;
    extract(&mut archive, stage.path(), EXE_SUFFIX)?;
    Ok(stage)
}

fn activate(dir: &Path, stage: tempfile::TempDir) -> Result<()> {
    check_install(dir, std::env::consts::EXE_SUFFIX)?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            stage.path(),
            fs::Permissions::from_mode(fs::metadata(dir)?.permissions().mode() & 0o777),
        )?;
        File::open(stage.path())?.sync_all()?;
        exchange(stage.path(), dir)?;
        let backup = stage.keep();
        eprintln!(
            "Updated {}. Previous whole bundle retained at {}. Restart board processes.",
            dir.display(),
            backup.display()
        );
        if let Err(e) = File::open(dir.parent().unwrap()).and_then(|p| p.sync_all()) {
            eprintln!("Warning: the directory swap succeeded, but syncing the parent failed: {e}");
        }
    }
    #[cfg(windows)]
    {
        eprintln!("Verified bundle ready at {}. Old install unchanged. Exit all board processes (including daemon and MCP); then point PATH, shortcuts and services to this new folder and restart. Do not copy over individual files.", stage.keep().display());
    }
    Ok(())
}

fn verify(file: &mut File, expected: &str) -> Result<()> {
    file.rewind()?;
    let mut hash = Sha256::new();
    io::copy(file, &mut hash)?;
    if format!("{:x}", hash.finalize()) != expected {
        return Err("archive checksum mismatch".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
