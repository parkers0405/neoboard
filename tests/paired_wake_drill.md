# Verify native paired wake locally

Run from the checkout on POSIX with Python 3.8+ (assertions enabled) and current debug binaries. No package installation, host board, login service or provider account is needed.

```sh
cargo build --locked --offline --bin neoboard --bin neochatd
python3 -B tests/paired_wake_drill.py --probe
python3 -B tests/paired_wake_drill.py
```

`--board-bin-dir DIR` selects another local build. `--harness neoism|claude|codex` selects one case. Neoism and Claude use real binaries if found; use `--neoism-agent PATH` or `--claude PATH` to select one. A detected runtime failing its offline setup **fails** the drill rather than falling back. `--fixtures` explicitly selects protocol fixtures for all three; the board and wake CLI remain real. The JSON receipt identifies which runtime handled each case.

Each case starts a separate numeric-loopback `neochatd`. Only the test controller reads its ephemeral admin credential, to provision private, expiring board-scoped grants. The CLI receives a mode-0600 reviewer invite; the writer cannot consume the reviewer's deliveries. Every command runs in a temporary workspace with a constructed environment, separate HOME/XDG directories, `NEOBOARD_WAKE_HOME` and `NEOBOARD_WAKE_MANUAL=1`. Nothing looks up the latest conversation. Each `run --once` starts a new process against the same private wake state.

The assertions cover single-use pairing; default-off; a diagnostic that reaches the target but is **not** acknowledged; a writer mention; pause with a queued, subscribed completion; on and delivery; and two restarted polls after making the original inbox IDs unread again. Target histories must contain exactly the expected turns, with thread/message references but no board-text canary. Decoy histories must remain unchanged. CLI-backed runtimes also test an absent executable at pairing, enable and dispatch, including no automatic resend after restoration. This drill tests delivery, not model reasoning, MCP task handling, service registration, or the GUI.

## Runtime evidence and session IDs

This wake-delivery drill supplies the session ID. It cannot verify the Desktop **Copy for agent** flow on its own. The [Claude Code chat-pairing check](claude_pair.md) runs the command through the real chat's Bash tool without supplying an ID. The [Codex chat-pairing check](codex_pair.md) separates the tested NeoBoard fixture from the opt-in real-runtime check; a fixture pass does not verify a real Codex chat.

| Harness | Offline isolation and evidence | Session ID hint |
| --- | --- | --- |
| Neoism | The installed `neoism-agent` 0.7.164 served a private authenticated loopback origin. A disposable `agent.json` selects only the loopback Chat Completions provider; both session histories are read via the private API. | Use the `id` returned by `POST /v2/sessions` for the intended conversation. No `NEOISM_SESSION_ID` was present in the invoking shell, and the standalone binary did not contain that literal. Do not assume it is exported. Pass `--session` and `--origin` explicitly when necessary; supply `NEOISM_WAKE_TOKEN` privately. |
| Claude Code | Installed Claude Code 2.1.330 created two sessions and resumed the target against a loopback Messages provider. An exec-only launcher pins the binary, temporary configuration, synthetic API key, `--bare` (no keychain auth), no tools and strict MCP configuration. Assertions read only its private transcripts. | `--session-id UUID` selects the new conversation's ID; `--print --output-format json` returned the same `session_id` locally. SessionStart hook input also carries `session_id`. `CLAUDE_CODE_SESSION_ID` may be available in a Claude-launched shell; neither that nor `CLAUDE_SESSION_ID` was set in the invoking shell. |
| Codex | Neither `codex` on PATH nor `/Applications/Codex.app/Contents/Resources/codex` was available on the verification host. The executable fixture responds to `--version` and implements `app-server` initialize, explicit `thread/resume`, `turn/start` and a matching completion. It records the literal prompt under the requested pre-existing thread ID. **Not** evidence of an installed Codex model turn or on-disk resume. | In a Codex-launched shell check `CODEX_THREAD_ID`; otherwise use the exact ID from the trusted thread-creation result. No value was present here. The drill never scans a live `CODEX_HOME`. |

`--probe` prints only binary paths, versions and whether the named environment variables are nonempty. It does not print session IDs, consult live runtime settings or open a conversation.