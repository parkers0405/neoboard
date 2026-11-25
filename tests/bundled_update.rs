use neochat::client::{connect_local, resume_after_update, stop_for_update};
use std::{fs, io::{BufRead, BufReader}, path::Path, process::{Command, Stdio}};

// Re-exec the test client beside a disposable daemon. Never discover a real
// endpoint, modify an installation, or depend on an ambient daemon override.
#[test]
fn disposable_update() {
    let root = tempfile::tempdir().unwrap();
    let client = root.path().join(format!("test-client{}", std::env::consts::EXE_SUFFIX));
    let daemon = client.with_file_name(format!("neochatd{}", std::env::consts::EXE_SUFFIX));
    fs::copy(std::env::current_exe().unwrap(), &client).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_neochatd"), &daemon).unwrap();
    let result = Command::new(client).args(["--ignored", "--exact", "client_scenario", "--nocapture"])
        .env("UPDATE_TEST_DATA", root.path().join("data")).env("NEOCHAT_BIND", "127.0.0.1:0")
        .env_remove("NEOCHAT_DAEMON").env_remove("NEOBOARD_PORTABLE_HANDOFF").status().unwrap();
    assert!(result.success());
}

#[test]
#[ignore = "runs only inside the temporary install"]
fn client_scenario() {
    let path = std::env::var_os("UPDATE_TEST_DATA").unwrap();
    let data = Path::new(&path);
    let connect = || connect_local(Some(data), None);
    assert!(!stop_for_update(data).unwrap());
    connect().unwrap();
    // A second install containing the same binary files is not the owner.
    let program = std::env::current_exe().unwrap().with_file_name(format!("neochatd{}", std::env::consts::EXE_SUFFIX));
    let other = tempfile::tempdir().unwrap();
    let other_client = other.path().join(std::env::current_exe().unwrap().file_name().unwrap());
    fs::copy(std::env::current_exe().unwrap(), &other_client).unwrap();
    fs::copy(&program, other.path().join(program.file_name().unwrap())).unwrap();
    assert!(Command::new(other_client).args(["--ignored", "--exact", "foreign_install"]).status().unwrap().success());
    assert!(stop_for_update(data).unwrap());
    assert!(!data.join("endpoint.json").exists());
    assert!(connect().err().unwrap().to_string().contains("paused for update"));
    // Simulate a child that was spawned before the hold but scheduled after it.
    assert!(!Command::new(&program).arg("--bundled-auto-v1").arg("--data-dir").arg(data).status().unwrap().success());
    resume_after_update(data).unwrap();
    connect().unwrap();
    assert!(stop_for_update(data).unwrap());
    // Simulate the previous release's hold, then an expired same-release hold.
    let hold = data.join("bundled-update-v1.json");
    for (key, value) in [("version", serde_json::json!("0.0.0")), ("expires", serde_json::json!(0))] {
        let mut h: serde_json::Value = serde_json::from_slice(&fs::read(&hold).unwrap()).unwrap();
        h[key] = value;
        fs::write(&hold, serde_json::to_vec(&h).unwrap()).unwrap();
        connect().unwrap();
        assert!(stop_for_update(data).unwrap());
    }
    // Manual/portable instance with stale auto-discovery must be ignored.
    let mut manual = Command::new(program).args(["--portable-v1", "--data-dir"]).arg(data)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut ready = String::new();
    BufReader::new(manual.stdout.take().unwrap()).read_line(&mut ready).unwrap();
    assert_eq!(ready.trim(), "READY");
    assert!(!stop_for_update(data).unwrap());
    assert!(manual.try_wait().unwrap().is_none());
    drop(manual.stdin.take());
    assert!(manual.wait().unwrap().success());
}

#[test]
#[ignore = "runs only inside the second temporary install"]
fn foreign_install() {
    let data = std::env::var_os("UPDATE_TEST_DATA").unwrap();
    assert!(!stop_for_update(Path::new(&data)).unwrap());
}