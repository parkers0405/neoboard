# Roadmap 27: offline acceptance drill

Use the board checkout and a paired Neoism checkout containing `POST /v2/sessions/{id}/prompt-if-idle` and the built-in `neoism/stub` model. With Rust dependencies already cached and a Python environment containing `scripts/board_watcher_requirements.txt`, run:

```sh
cargo build --offline --locked --bin neochat --bin neochatd
(cd ../neoism && cargo build --offline --locked -p neoism-agent --bin neoism-agent)
python -B tests/roadmap27_drill.py --neoism-agent ../neoism/target/debug/neoism-agent
```

The command emits a JSON receipt only after assertions pass. No account, deployment, shared board or existing session is used. Both services bind to loopback, with fresh state under a temporary directory and no inherited credentials. The harness terminates the processes and removes their stores on exit. Do not use Python `-O`.

**Real:** signed webhook CLI in a fresh process per delivery; SQLite bridge mappings and export cursor; board daemon, scoped grants, claim and replies via the tagged CLI; both watcher subscriptions, Turso queues and assignment wake; Neoism conditional admission, session store and two completed assistant messages. The imported issue's task root is the thread ID throughout. The hidden channel contains an export-eligible canary, and a second session has no binding. Neither should receive any work. The fake GitHub sink must see exactly two comments on the fixture issue: the question and one response; it must never receive the imported human answer back.

**Fixtures and integration limits:** GitHub ingress is generated locally, and the poller's outbound GitHub callable records writes instead of contacting the REST API. The human reply is synthetic. Neoism's real runtime runs its offline stub model, not a reasoning model: the harness acts as its board-tool driver, claiming and asking after the assignment turn, then posting the resumed turn's actual text. An ordinary board question is tested, not a pending Neoism `question` tool call. The second turn requires a drill-only reply consumer: the assignment watcher correctly does not wake again. It matches the imported comment ID to the question thread, uses the opted-in binding and calls the real conditional prompt endpoint. This is a second turn in the same session, not a continuation of a suspended tool call.

For unattended integration, connect the scoped board tools to the agent, and implement durable pending-question correlation (session, task, question and authorized respondent), reply delivery deduplication/reservation, stale-lease and binding checks, and idempotent result posting. Do not use an arbitrary thread reply as authorization to prompt a session. The drill does not promise exactly-once GitHub writes following an ambiguous remote POST; see `scripts/github_bridge.md` for reconciliation.