//! One per-user login entry, never one entry per binding. The caller serializes
//! enable/disable with its binding updates; `wake run` owns the singleton lock.
use std::{env, io, path::Path, process::{Command, Stdio}};

fn isolated(home: Option<&std::ffi::OsStr>) -> Result<bool, String> {
    match home {
        None => Ok(false),
        Some(p) if Path::new(p).is_absolute() => Ok(true),
        Some(_) => Err("NEOBOARD_WAKE_HOME must be an absolute path".into()),
    }
}

/// Register for future logins and start now, without waiting for a turn. An
/// explicit wake home is private to this invocation: never change host login
/// settings for it. The immediate child inherits the override unchanged.
pub fn ensure_running() -> Result<(), String> {
    let private = isolated(env::var_os("NEOBOARD_WAKE_HOME").as_deref())?;
    let cli = env::current_exe().map_err(|e| e.to_string())?
        .with_file_name(format!("neoboard{}", env::consts::EXE_SUFFIX));
    if !cli.is_file() {
        return Err(format!("Missing wake CLI: {}", cli.display()));
    }
    if !private {
        login::install(&cli)?;
    }
    start(&cli).map_err(|e| format!("Starting {} wake run: {e}", cli.display()))
}

/// Call only after durably disabling the last binding. Do not kill a current
/// turn: the worker must consult committed bindings before claiming another.
pub fn uninstall() -> Result<(), String> {
    if isolated(env::var_os("NEOBOARD_WAKE_HOME").as_deref())? {
        return Ok(());
    }
    login::remove()
}

fn start(cli: &Path) -> io::Result<()> {
    let mut cmd = Command::new(cli);
    cmd.args(["wake", "run"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe; no allocation or shared state.
        unsafe { cmd.pre_exec(|| if libc::setsid() == -1 { Err(io::Error::last_os_error()) } else { Ok(()) }); }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW, also when invoked from a terminal
    }
    cmd.spawn().map(|_| ())
}

#[cfg(any(unix, test))]
fn path_text(cli: &Path) -> Result<&str, String> {
    let p = cli.to_str().ok_or("Login launcher requires a Unicode executable path")?;
    if p.chars().any(char::is_control) { return Err("Control character in executable path".into()); }
    Ok(p)
}

#[cfg(any(target_os = "macos", test))]
fn plist(cli: &Path) -> Result<String, String> {
    let p = path_text(cli)?.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    Ok(format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>ai.neoism.neoboard.wake</string>
<key>ProgramArguments</key><array><string>{p}</string><string>wake</string><string>run</string></array>
<key>RunAtLoad</key><true/>
</dict></plist>
"#))
}

#[cfg(any(target_os = "linux", test))]
fn desktop_entry(cli: &Path) -> Result<String, String> {
    let p = path_text(cli)?;
    if p.contains('=') { return Err("XDG Exec executable paths cannot contain '='".into()); }
    // Two distinct Desktop Entry spec layers: quote for Exec tokenization,
    // then escape the resulting backslashes for the entry's string value.
    let mut arg = String::new();
    for c in p.chars() {
        match c {
            '"' | '`' | '$' | '\\' => arg.push('\\'),
            '%' => arg.push('%'), // literal %, not a field code
            _ => (),
        }
        arg.push(c);
    }
    let arg = arg.replace('\\', "\\\\");
    Ok(format!("[Desktop Entry]\nType=Application\nName=NeoBoard Wake\nExec=\"{arg}\" wake run\nTerminal=false\n"))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod login {
    use super::*;
    use std::{fs, io::Write, path::PathBuf};

    #[cfg(target_os = "linux")]
    pub(super) fn entry() -> Result<PathBuf, String> {
        dirs::config_dir().filter(|p| p.is_absolute()).map(|p| p.join("autostart/ai.neoism.neoboard.wake.desktop"))
            .ok_or_else(|| "Cannot locate user XDG config directory".into())
    }

    #[cfg(target_os = "macos")]
    pub(super) fn entry() -> Result<PathBuf, String> {
        dirs::home_dir().filter(|p| p.is_absolute()).map(|p| p.join("Library/LaunchAgents/ai.neoism.neoboard.wake.plist"))
            .ok_or_else(|| "Cannot locate user LaunchAgents directory".into())
    }

    pub(super) fn install(cli: &Path) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        let content = plist(cli)?;
        #[cfg(target_os = "linux")]
        let content = desktop_entry(cli)?;
        let dest = entry()?;
        write_entry(&dest, &content).map_err(|e| format!("Installing {}: {e}", dest.display()))
    }

    fn write_entry(dest: &Path, content: &str) -> io::Result<()> {
        let dir = dest.parent().unwrap();
        fs::create_dir_all(dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        tmp.write_all(content.as_bytes())?;
        tmp.persist(dest)?;
        Ok(())
    }

    pub(super) fn remove() -> Result<(), String> {
        let dest = entry()?;
        match fs::remove_file(&dest) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("Removing {}: {e}", dest.display())),
        }
    }
}

#[cfg(windows)]
mod login {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::{core::w, Win32::{Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND}, System::Registry::{HKEY_CURRENT_USER, REG_SZ}, UI::Shell::{SHDeleteValueW, SHSetValueW}}};

    const RUN: *const u16 = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    const NAME: *const u16 = w!("NeoBoardWake");

    pub(super) fn install(cli: &Path) -> Result<(), String> {
        // Explorer would allocate a console for the CLI. Use the GUI sibling
        // as a no-window trampoline, not a second worker implementation.
        let desktop = cli.with_file_name("neochat-desktop.exe");
        if !desktop.is_file() { return Err(format!("Missing wake login helper: {}", desktop.display())); }
        let cmd = run_value(&desktop)?;
        // SAFETY: static NUL-terminated names, bounded NUL-terminated UTF-16 data.
        let err = unsafe { SHSetValueW(HKEY_CURRENT_USER, RUN, NAME, REG_SZ, cmd.as_ptr().cast(), (cmd.len() * 2) as u32) };
        if err == 0 { Ok(()) } else { Err(format!("Setting wake login entry: {}", io::Error::from_raw_os_error(err))) }
    }

    pub(super) fn run_value(desktop: &Path) -> Result<Vec<u16>, String> {
        let mut cmd = vec![b'"' as u16];
        cmd.extend(desktop.as_os_str().encode_wide());
        cmd.extend("\" --wake-login\0".encode_utf16());
        if cmd.len() > 261 { return Err("Wake login command exceeds Windows Run key's 260-character limit".into()); }
        Ok(cmd)
    }

    pub(super) fn remove() -> Result<(), String> {
        // SAFETY: static NUL-terminated names. Never delete the Run key itself.
        match unsafe { SHDeleteValueW(HKEY_CURRENT_USER, RUN, NAME) } {
            0 | ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(()),
            err => Err(format!("Removing wake login entry: {}", io::Error::from_raw_os_error(err as i32))),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod login {
    pub(super) fn install(_: &std::path::Path) -> Result<(), String> { Err("Unsupported wake login platform".into()) }
    pub(super) fn remove() -> Result<(), String> { Err("Unsupported wake login platform".into()) }
}

#[cfg(test)]
mod tests;