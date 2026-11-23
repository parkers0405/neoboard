use super::*;

#[test]
fn explicit_home_must_be_absolute() {
    assert!(!isolated(None).unwrap());
    assert!(isolated(Some(tempfile::tempdir().unwrap().path().as_os_str())).unwrap());
    for p in ["", "../relative"] { assert!(isolated(Some(p.as_ref())).is_err()); }
}

#[test]
fn launcher_encodings() {
    let p = Path::new(r#"/app space/"`$\%f<&>/neoboard"#);
    let xdg = desktop_entry(p).unwrap();
    assert!(xdg.lines().any(|l| l == r#"Exec="/app space/\\"\\`\\$\\\\%%f<&>/neoboard" wake run"#));
    let xml = plist(p).unwrap();
    assert!(xml.contains(r#"<array><string>/app space/"`$\%f&lt;&amp;&gt;/neoboard</string><string>wake</string><string>run</string></array>"#));
    for result in [plist(Path::new("/line\nbreak")), desktop_entry(Path::new("/line\nbreak"))] { assert!(result.is_err()); }
}

#[cfg(windows)]
#[test]
fn windows_run_data_is_literal() {
    let p = Path::new(r"C:\Program Files\NeoBoard & %user%\neochat-desktop.exe");
    let value = login::run_value(p).unwrap();
    assert_eq!(String::from_utf16(&value).unwrap(), format!("\"{}\" --wake-login\0", p.display()));
    assert!(login::run_value(Path::new(&"A".repeat(260))).is_err());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn private_login_and_immediate_launch_fixture() {
    use std::{fs, os::unix::fs::PermissionsExt, thread, time::{Duration, Instant}};
    // Re-exec a COPY next to a fake CLI. Never call the public entry points
    // in the host environment. Even a regression is confined to the temp home.
    const FIXTURE: &str = "NEOBOARD_TEST_WAKE_SERVICE";
    if env::var_os(FIXTURE).is_some() && env::current_exe().unwrap().file_name().unwrap() == "wake-fixture" {
        let root = env::current_exe().unwrap().parent().unwrap().to_owned();
        assert_eq!(env::var_os("HOME").unwrap(), root);
        assert_eq!(env::var_os("XDG_CONFIG_HOME").unwrap(), root);
        assert_eq!(env::var_os("NEOBOARD_WAKE_HOME").unwrap(), root.join("private home"));
        let entry = login::entry().unwrap();
        // Fail closed even if a future directory API starts ignoring HOME.
        assert!(entry.starts_with(&root));
        ensure_running().unwrap();
        uninstall().unwrap();
        assert_eq!(fs::read_to_string(&entry).unwrap(), "existing login entry");

        let cli = root.join("neoboard");
        login::install(&cli).unwrap();
        #[cfg(target_os = "macos")]
        {
            let plist = Command::new("/usr/bin/plutil").args(["-extract", "ProgramArguments", "json", "-o", "-"]).arg(&entry).output().unwrap();
            assert!(plist.status.success());
            assert_eq!(serde_json::from_slice::<Vec<String>>(&plist.stdout).unwrap(), [cli.to_str().unwrap(), "wake", "run"]);
        }
        #[cfg(target_os = "linux")]
        assert_eq!(fs::read_to_string(&entry).unwrap(), desktop_entry(&cli).unwrap());
        login::remove().unwrap();
        login::remove().unwrap();
        assert!(!entry.exists());
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("space $ `% &");
    let home = root.join("private home");
    fs::create_dir_all(&home).unwrap();
    let cli = root.join("neoboard");
    fs::write(&cli, "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$#\" \"$@\" \"$NEOBOARD_WAKE_HOME\" > \"$HOME/result.tmp\"\nmv \"$HOME/result.tmp\" \"$HOME/result\"\n").unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
    #[cfg(target_os = "macos")]
    let entry = root.join("Library/LaunchAgents/ai.neoism.neoboard.wake.plist");
    #[cfg(target_os = "linux")]
    let entry = root.join("autostart/ai.neoism.neoboard.wake.desktop");
    fs::create_dir_all(entry.parent().unwrap()).unwrap();
    fs::write(&entry, "existing login entry").unwrap();
    let harness = root.join("wake-fixture");
    fs::copy(env::current_exe().unwrap(), &harness).unwrap();
    let output = Command::new(harness).args(["--exact", "wake::service::tests::private_login_and_immediate_launch_fixture"])
        .env_clear().env("PATH", "/usr/bin:/bin").env("HOME", &root).env("XDG_CONFIG_HOME", &root)
        .env("NEOBOARD_WAKE_HOME", &home).env(FIXTURE, "1").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join("result").exists() && Instant::now() < deadline { thread::sleep(Duration::from_millis(10)); }
    assert_eq!(fs::read_to_string(root.join("result")).unwrap(), format!("2\nwake\nrun\n{}\n", home.display()));
    assert!(!entry.exists());
}