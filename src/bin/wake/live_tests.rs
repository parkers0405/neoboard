use super::*;

#[test]
fn claude_settings_merge_is_idempotent_and_keeps_the_users_own_entries() {
    let exe = "'/Applications/NeoBoard.app/Contents/MacOS/neoboard'";
    let fresh = claude_settings(None, exe, "neoboard-wake-0a1b2c3d").unwrap();
    let doc: Value = serde_json::from_slice(&fresh).unwrap();
    for event in ["SessionStart", "Stop"] {
        let hook = &doc["hooks"][event][0]["hooks"][0];
        assert_eq!(hook["command"], format!("{exe} wake watch"));
        assert_eq!(hook["asyncRewake"], true);
        assert_eq!(hook["type"], "command");
    }
    assert_eq!(doc["hooks"]["SessionStart"][0]["matcher"], "startup|resume|clear|compact");
    assert!(doc["hooks"]["Stop"][0].get("matcher").is_none());
    assert_eq!(doc["permissions"]["allow"], json!(["mcp__neoboard-wake-0a1b2c3d", format!("Bash({exe} wake ack --code *)")]));
    assert_eq!(claude_settings(Some(&fresh), exe, "neoboard-wake-0a1b2c3d").unwrap(), fresh);

    // An older install path is replaced, and unrelated hooks, rules and keys survive.
    let mine = json!({"model":"opus","permissions":{"allow":["Bash(git status)"],"deny":["Read(.env)"]},
        "hooks":{"Stop":[{"hooks":[{"type":"command","command":"say done"}]},{"hooks":[{"type":"command","command":"'/old/neoboard' wake watch","asyncRewake":true}]}],
                 "PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"lint"}]}]}});
    let merged: Value = serde_json::from_slice(&claude_settings(Some(mine.to_string().as_bytes()), exe, "neoboard-wake-0a1b2c3d").unwrap()).unwrap();
    assert_eq!(merged["model"], "opus");
    assert_eq!(merged["permissions"]["deny"], json!(["Read(.env)"]));
    assert_eq!(merged["permissions"]["allow"][0], "Bash(git status)");
    assert_eq!(merged["permissions"]["allow"].as_array().unwrap().len(), 3);
    assert_eq!(merged["hooks"]["PreToolUse"], mine["hooks"]["PreToolUse"]);
    let stop = merged["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 2);
    assert_eq!(stop[0]["hooks"][0]["command"], "say done");
    assert_eq!(stop[1]["hooks"][0]["command"], format!("{exe} wake watch"));

    for bad in ["not json", "[]", r#"{"hooks":[]}"#, r#"{"hooks":{"Stop":{}}}"#, r#"{"permissions":{"allow":"all"}}"#] {
        assert!(claude_settings(Some(bad.as_bytes()), exe, "x").is_err(), "{bad}");
    }
}

#[test]
fn open_window_delivery_needs_a_running_claude_and_an_armed_or_just_fired_watcher() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    let mut b: Binding = serde_json::from_value(json!({"source":{"board":"board","project":"team","origin":"http://127.0.0.1:1","harness":"claude",
        "invite":"/missing","actor":"first"},"target":{"session":"00000000-0000-4000-8000-000000000017","harness":"claude","executable":"/bin/sh",
        "origin":null,"workspace":"/tmp"},"enabled":true,"bearer":null,"watermark":0,"uncertain":null,"last":"","test":null,"live":true})).unwrap();
    let stem = stem(&b.target.session);
    let me = std::process::id();
    assert!(!active(dir, &b)); // nothing recorded
    mark(dir, &stem, me, "armed");
    assert!(!active(dir, &b)); // recorded, but no watcher holds the lock (it was killed)
    let held = store::lock(dir, &format!("{stem}.lock"), false).unwrap();
    assert!(active(dir, &b));
    drop(held);
    mark(dir, &stem, me, "delivered");
    assert!(active(dir, &b)); // the woken turn is still running
    mark(dir, &stem, me, "expired");
    assert!(!active(dir, &b));
    mark(dir, &stem, 0, "delivered");
    assert!(!active(dir, &b)); // that Claude Code process is gone
    mark(dir, &stem, me, "delivered");
    b.live = false;
    assert!(!active(dir, &b));
    b.live = true;
    b.target.harness = "codex".into();
    assert!(!active(dir, &b));
    assert!(connection(&b).starts_with("neoboard-wake-") && connection(&b).len() == "neoboard-wake-".len() + 8);
}

#[test]
fn codex_approval_touches_only_this_connections_table() {
    let before = "model = \"x\"\n\n[mcp_servers.other]\ncommand = \"o\"\n\n[mcp_servers.neoboard-wake-0a1b2c3d]\ncommand = \"/adapter\"\nargs = [\"--agent-invite\", \"/grant\"]\n\n[projects.\"/p\"]\ntrust_level = \"trusted\"\n";
    let after = codex_approval(before, "neoboard-wake-0a1b2c3d").unwrap().unwrap();
    assert_eq!(after, before.replace("[mcp_servers.neoboard-wake-0a1b2c3d]\n", "[mcp_servers.neoboard-wake-0a1b2c3d]\ndefault_tools_approval_mode = \"approve\"\n"));
    assert_eq!(codex_approval(&after, "neoboard-wake-0a1b2c3d").unwrap(), None);
    // Last table without a trailing newline.
    assert_eq!(codex_approval("[mcp_servers.w]", "w").unwrap().unwrap(), "[mcp_servers.w]\ndefault_tools_approval_mode = \"approve\"\n");
    // Another server's setting is not ours.
    let other = "[mcp_servers.w]\ncommand = \"a\"\n[mcp_servers.other]\ndefault_tools_approval_mode = \"prompt\"\n";
    assert!(codex_approval(other, "w").unwrap().unwrap().starts_with("[mcp_servers.w]\ndefault_tools_approval_mode = \"approve\"\ncommand"));
    assert!(codex_approval("[mcp_servers.other]\n", "w").is_err());
    assert!(codex_approval("[mcp_servers.w]\n[mcp_servers.w]\n", "w").is_err());
}
