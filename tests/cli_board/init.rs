use super::{cli, start};
use neochat::{client::Client, protocol::Invite};
use std::{io::Write, path::Path, process::{Command, Output, Stdio}};

fn isolated(program: &str, dir: &Path) -> Command {
    let mut cmd = Command::new(program);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("NEOCHAT_") { cmd.env_remove(key); }
    }
    cmd.env_remove("NEOBOARD_PUBLIC_URL").env("NEOCHAT_DATA_DIR", dir);
    cmd
}

fn init(dir: &Path, password: &str) -> Output {
    let mut child = isolated(env!("CARGO_BIN_EXE_neochat"), dir)
        .env("NEOBOARD_PUBLIC_URL", "https://boards.invalid/prefix/")
        .args(["init", "--board", "QA_42", "--password-stdin"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    writeln!(child.stdin.take().unwrap(), "{password}").unwrap();
    child.wait_with_output().unwrap()
}

fn join(daemon: &Client, board: &str, password: &str) -> reqwest::blocking::Response {
    // The published HTTPS address belongs to the operator's proxy; exercise its
    // real upstream locally, without publishing a tunnel or contacting a host.
    reqwest::blocking::Client::builder().no_proxy().build().unwrap()
        .post(format!("{}v1/join/password", daemon.endpoint()))
        .json(&serde_json::json!({"project": board, "password": password})).send().unwrap()
}

#[test]
fn init_prints_desktop_address_and_preserves_existing_board_and_password() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let (_daemon, client) = start(dir);
    let password = neochat::board::random_token();
    let other = neochat::board::random_token();
    let created = init(dir, &password);
    let stdout = String::from_utf8(created.stdout).unwrap();
    assert!(created.status.success(), "{}", String::from_utf8_lossy(&created.stderr));
    assert!(created.stderr.is_empty() && !stdout.contains(&password));
    assert_eq!(stdout.lines().next().unwrap(), "Desktop join address: https://boards.invalid/prefix/#neoboard=QA_42");
    assert!(stdout.contains("Cloudflare") && stdout.contains("separately"));

    cli(dir, &["post", "QA_42/keep", "before second init"]);
    let conflict = init(dir, &other);
    assert!(!conflict.status.success() && conflict.stdout.is_empty());
    let err = String::from_utf8(conflict.stderr).unwrap();
    assert!(err.contains("already exists") && !err.contains("#neoboard=") && !err.contains(&other));
    assert_eq!(join(&client, "QA_42", &other).status(), 401);
    let invite: Invite = join(&client, "QA_42", &password).error_for_status().unwrap().json().unwrap();
    assert_eq!(invite.channels, ["QA_42/*"]);
    let member = Client::new(client.endpoint(), &invite.token, None).unwrap();
    assert!(member.snapshot().unwrap().messages.iter().any(|m| m.text == "before second init"));
}

#[cfg(unix)]
#[test]
#[ignore = "requires the expect PTY driver"]
fn init_interactive_pty_checks_confirmation_and_no_echo() {
    let tmp = tempfile::tempdir().unwrap();
    let (_daemon, client) = start(tmp.path());
    for mismatch in [true, false] {
        let out = isolated("expect", tmp.path()).env("CLI", env!("CARGO_BIN_EXE_neochat"))
            .env("MISMATCH", if mismatch { "1" } else { "0" }).args(["-c", r#"
                set timeout 8
                proc prompt {text} {
                    expect {
                        -exact $text {}
                        timeout {exit 90}
                        eof {exit 91}
                    }
                }
                spawn -noecho $env(CLI) init
                prompt {dots): }
                send -- "pty-team\r"
                prompt {separately): }
                send -- "https://boards.invalid\r"
                prompt {New board password: }
                send -- "pty-fixture-pass\r"
                prompt {Confirm board password: }
                if {$env(MISMATCH)} {send -- "pty-wrong-pass\r"} else {send -- "pty-fixture-pass\r"}
                expect {
                    timeout {exit 92}
                    eof {}
                }
                exit [lindex [wait] 3]
            "#]).output().unwrap();
        let transcript = String::from_utf8(out.stdout).unwrap();
        assert!(out.stderr.is_empty());
        assert!(!transcript.contains("pty-fixture-pass") && !transcript.contains("pty-wrong-pass"));
        if mismatch {
            assert_eq!(out.status.code(), Some(1));
            assert!(transcript.contains("passwords did not match") && !transcript.contains("#neoboard="));
        } else {
            assert!(out.status.success(), "{transcript}");
            assert!(transcript.contains("Desktop join address: https://boards.invalid/#neoboard=pty-team"));
        }
    }
    assert_eq!(join(&client, "pty-team", "pty-fixture-pass").status(), 200);
}