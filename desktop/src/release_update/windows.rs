//! Only msiexec requests elevation. The helper and the reopened desktop keep
//! the caller's token. No script or executable is run out of staging.
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    env,
    error::Error,
    ffi::{OsStr, OsString},
    os::windows::{ffi::OsStrExt, ffi::OsStringExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

// ProcessStartInfo.Arguments is a single Windows command line, not an argv
// array. Double backslashes before a closing quote and escape literal quotes.
fn arguments(args: impl Iterator<Item = OsString>) -> OsString {
    let mut result = Vec::new();
    let slash = u16::from(b'\\');
    let quote = u16::from(b'"');
    for arg in args {
        if !result.is_empty() {
            result.push(u16::from(b' '));
        }
        result.push(quote);
        let mut pending = 0;
        for c in arg.encode_wide() {
            if c == slash {
                pending += 1;
            } else {
                let n = if c == quote { 2 * pending + 1 } else { pending };
                result.extend(std::iter::repeat_n(slash, n));
                result.push(c);
                pending = 0;
            }
        }
        result.extend(std::iter::repeat_n(slash, pending * 2));
        result.push(quote);
    }
    OsString::from_wide(&result)
}

pub(super) fn apply(msi: &Path, data: Option<&Path>) -> Result<(), Box<dyn Error>> {
    let dir = msi.parent().ok_or("missing MSI staging directory")?;
    if !msi.is_absolute()
        || msi.file_name() != Some(OsStr::new("NeoBoard-x86_64.msi"))
        || dir.parent() != Some(env::temp_dir().as_path())
        || !dir
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with("neoboard-installer-"))
    {
        return Err("unexpected MSI staging path".into());
    }
    let mut buf = [0u16; 32768];
    let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n == 0 || n >= buf.len() {
        return Err("cannot locate Windows system directory".into());
    }
    let system = PathBuf::from(OsString::from_wide(&buf[..n]));
    // Windows PowerShell expects UTF-16LE for -EncodedCommand. Pass only the
    // fixed helper this way so command-line parsing cannot consume its quotes.
    let script: Vec<_> = HELPER.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut helper = Command::new(system.join(r"WindowsPowerShell\v1.0\powershell.exe"));
    super::relaunch_daemon(&mut helper, data);
    helper
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
        ])
        .arg(STANDARD.encode(script))
        .env("NB_UPDATE_PARENT", std::process::id().to_string())
        .env("NB_UPDATE_MSI", msi)
        .env("NB_UPDATE_EXE", env::current_exe()?)
        .env("NB_UPDATE_ARGS", arguments(env::args_os().skip(1)))
        .env("NB_UPDATE_CWD", env::current_dir()?)
        .current_dir(system)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW: independent of the caller's console
        .spawn()?;
    Ok(())
}

// All variable inputs are environment data, never PowerShell source. Pin a
// process handle before waiting; a reused PID started after this helper.
const HELPER: &str = r#"
$ErrorActionPreference = 'Stop'
$msi = $env:NB_UPDATE_MSI
$dir = [IO.Path]::GetDirectoryName($msi)
$failure = $null
function Check-Staging {
    foreach ($path in $dir, $msi) {
        if ([IO.File]::GetAttributes($path) -band [IO.FileAttributes]::ReparsePoint) {
            throw 'Refusing to use redirected installer staging.'
        }
    }
}
try {
    $parent = try { [Diagnostics.Process]::GetProcessById([int]$env:NB_UPDATE_PARENT) } catch [ArgumentException] { $null }
    if ($null -ne $parent) {
        try {
            $null = $parent.Handle
            if (!$parent.HasExited -and $parent.StartTime -le [Diagnostics.Process]::GetCurrentProcess().StartTime) {
                if (!$parent.WaitForExit(120000)) { throw 'Desktop did not exit; nothing installed.' }
            }
        } catch [InvalidOperationException] {
            if (!$parent.HasExited) { throw }
        } finally { $parent.Dispose() }
    }
    Check-Staging
    $install = [Diagnostics.ProcessStartInfo]::new()
    $install.FileName = [IO.Path]::Combine([Environment]::SystemDirectory, 'msiexec.exe')
    $install.Arguments = '/i "' + $msi + '" /passive /norestart'
    $install.WorkingDirectory = [Environment]::SystemDirectory
    $install.UseShellExecute = $true
    $install.Verb = 'runas'
    $process = [Diagnostics.Process]::Start($install)
    try { $process.WaitForExit(); $code = $process.ExitCode } finally { $process.Dispose() }
    if ($code -notin 0,3010,1641) { throw "Windows Installer exited with code $code." }
    if ($code -ne 1641) { # 1641: the installer already initiated a system restart
        $app = [Diagnostics.ProcessStartInfo]::new()
        $app.FileName = $env:NB_UPDATE_EXE
        $app.Arguments = [string]$env:NB_UPDATE_ARGS
        $app.WorkingDirectory = $env:NB_UPDATE_CWD
        $app.UseShellExecute = $false
        foreach ($key in 'PARENT','MSI','EXE','ARGS','CWD') { $app.EnvironmentVariables.Remove('NB_UPDATE_' + $key) }
        [Diagnostics.Process]::Start($app).Dispose()
    }
} catch {
    $failure = $_.Exception.Message
    if ($env:NEOBOARD_UPDATE_LOCAL_DATA -and [IO.File]::Exists($env:NB_UPDATE_EXE) -and ($null -eq $parent -or !(Get-Process -Id $env:NB_UPDATE_PARENT -ErrorAction SilentlyContinue))) {
        try {
            $restore = [Diagnostics.ProcessStartInfo]::new($env:NB_UPDATE_EXE, [string]$env:NB_UPDATE_ARGS)
            $restore.WorkingDirectory = $env:NB_UPDATE_CWD
            $restore.UseShellExecute = $false
            $restore.EnvironmentVariables['NEOBOARD_UPDATE_ROLLBACK'] = '1'
            [Diagnostics.Process]::Start($restore).Dispose()
        } catch { $failure += ' Could not reopen the desktop; open it manually.' }
    }
}
try {
    Check-Staging
    [IO.File]::Delete($msi)
    [IO.Directory]::Delete($dir, $false) # never recursively remove unknown contents
} catch { $failure += " Cleanup failed for ${dir}: $($_.Exception.Message)" }
if ($failure -or $code -eq 3010) {
    if (!$failure) { $failure = 'Windows Installer completed. Restart Windows to finish the update.' }
    (New-Object -ComObject WScript.Shell).Popup($failure, 0, 'NeoBoard update', 48) | Out-Null
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaunch_preserves_empty_quotes_and_trailing_backslashes() {
        let args = ["", "board name", r#"a\"b"#, r"C:\a b\", "$env:x; &"].map(OsString::from);
        assert_eq!(
            arguments(args.into_iter()),
            OsStr::new(r#""" "board name" "a\\\"b" "C:\a b\\" "$env:x; &""#)
        );
    }
}
