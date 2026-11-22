# Roadmap 27 / 185: unattended acceptance

From the board checkout, with cached Rust dependencies and a Python environment containing `scripts/board_watcher_requirements.txt`:

```sh
cargo build --offline --locked --bin neochatd --bin neochat-mcp
(cd ../neoism && cargo build --offline --locked -p neoism-agent --bin neoism-agent)
python -B tests/roadmap27_drill.py --neoism-agent ../neoism/target/debug/neoism-agent
```

Use the **standalone** `neoism-agent` binary with conditional session admission, the `execute` MCP gateway and local OpenAI-compatible provider support. The fixture writes the standalone runtime’s unified `$XDG_CONFIG_HOME/agent/agent.json` under a temporary directory. Both the local provider and `mcp.neoboard` are in that file. A GUI `config.json` or a legacy `mcp.json` alone is not standalone runtime configuration. Do not change your regular config or start/restart an existing service. Run without Python `-O`; a zero exit emits a JSON receipt after the assertions pass. A captured run is in [roadmap27_receipt.json](roadmap27_receipt.json).

### What a passing receipt proves

1. A signed synthetic GitHub issue opens one mapped task on the loopback board. Re-deliveries do not create another. `board_watcher.py run --interval 1` is already running with an assignment subscription and `--bridge-route bridge repo:19`. The watcher, not the harness, submits the conditional prompt to the bound session.
2. The real session calls `neoboard.tasks_list`, `tasks_claim`, `tasks_start` and `reply` through Neoism’s MCP gateway and the shipped stdio adapter. The worker has only `read,claim,start,reply,complete` in `general`; the watcher has `read`, the bridge `read,reply,post_task`. The lease holder registers a labeled question and the real bridge exports it to the fixture issue sink.
3. A signed comment from the wrong user with the right command stays ordinary thread data. A signed comment from GitHub user `2` (`answer_actor_ids: [2]`) with exact first line `/neoboard-answer <board-question-reply-id>` becomes a correlated answer. The service admits a second turn of the **same** session. The provider fixture checks the lease, question and linked answer in the actual MCP `read` result, derives the result from that answer, requests `tasks_complete`, and checks the completed task with `tasks_list`. The harness neither executes a worker tool nor posts a wake or result.
4. The receipt cross-checks the session tool history, board event stream (one `completed`), `question-status`, `wake-status`, service cycle output and durable export mappings. The fake GitHub sink sees exactly one question and one result. The out-of-scope canary is untouched and the unbound session stays empty.
5. After SIGTERM of the foreground service, the drill re-delivers the webhook, replays both generic watcher delivery IDs and launches **two new** `run --once` processes on the same DB. Each reports the final cursor and `wake: null`; the session history and board events are unchanged, with no additional exports.

**Limits:** all storage and credentials are disposable, the servers bind to loopback, and model discovery and embeddings are disabled. GitHub’s API, identity and delivery transport are not live: webhook bytes and HMACs are local, and an in-memory sink stands in for the remote REST write. The provider streams deterministic Chat Completions function calls, not model reasoning. This exercises two completed turns, not a suspended interactive `question` tool. It tests a clean service restart and observed single completion, not crash recovery from an ambiguous external POST, deployment supervision or a general exactly-once guarantee.