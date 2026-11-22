# Wake one existing Neoism session for one actor's private inbox

Receiving a NeoBoard notification does **not** start a turn by default. This optional foreground service polls [the private notification API](../docs/notification-api.md) and can admit one new turn per unread delivery. It only reads the board: it does not mark anything read, register subscriptions, post replies, claim work or manage leases. Use the separate [assignment watcher](board_watcher.md) for assignment/answer routing; neither service enables the other.

## Before enabling

1. Use a daemon with the #190 API and a **real restricted board credential for the intended recipient**. For example, a board operator can use `neochat invite create reviewer --channel project/dev --grant read --grant notifications` on the intended daemon and deliver the returned token privately. Check expiry (CLI default: one day). For mentions alone the watcher needs only `read`; `notifications` in addition to `read` is needed for the actor to register a subscription or change read state. Do not hand the service the admin/discovery token, a desktop host identity, or someone else's invite. The service rejects `local` and `system`. The inbox API authenticates the actor, not the grant list: the operator must provision the narrow scope.
2. Route **to the credential owner**. For example, a post by `writer` mentioning `@reviewer` can wake `reviewer`, not `writer`. A completion or blocked/failed transition does not automatically notify the assignee, creator or parent session. To receive it, register the thread and exact kind with `POST /v1/subscriptions` **using the reviewer's credential**. To follow replies use `kind: "reply"` and the exact board `author` byline. Register before delegation or supply the pre-delegation `after_cursor` for replay. See the API for mention resolution, scope, filters and visibility. The consumer checks the top-level page `actor` on init and on every poll, never item `actor` (the emitter), `author`, a profile, display name or text. No subscriptions or routing are inferred from board content.
3. Provide an **already existing** session on a trusted local Neoism server with bearer authentication enabled, the atomic `POST /v2/sessions/{id}/prompt-if-idle` route, and a configured provider if it is to run a model turn. Use the exact `ses_…` ID, not a title or “latest”. Configure that session's `neochat-mcp` with explicit `NEOCHAT_URL` and scoped `NEOCHAT_TOKEN` for the same board/recipient and verify it can read the target channel. The watcher cannot inspect the session's MCP identity. A separate runtime bearer must authorize `GET /v2/sessions/{id}` and conditional admission. No token guessing, daemon discovery, session creation, shell commands or unconditional prompt fallback. The admitted prompt contains the local reservation key, kind and validated channel/thread/message coordinates, not message text or credentials.
4. Use a Unix host, Python 3.10+ and the dependencies in `scripts/board_watcher_requirements.txt` (`python -m pip install -r scripts/board_watcher_requirements.txt` in a virtualenv). Keep the checkout, private state directory and environment under trusted ownership. Supply **both** `NEOBOARD_INBOX_TOKEN` (board bearer) and `NEOISM_WAKE_TOKEN` (runtime bearer) privately in the process environment, not arguments. Use an HTTPS board origin or numeric loopback HTTP. The runtime must be an HTTP(S) origin at `127.0.0.1` or `[::1]`. No credentials, path, query, fragment, proxies or redirects on either connection. Do not bind to a port that can be taken over by another local user.

## Opt in and run

With `BOARD_URL`, `NEOISM_ORIGIN`, `SESSION_ID`, and the two bearer variables set, from the checkout:

```sh
umask 077
mkdir -m 700 -p "$HOME/.local/state/neoboard-inbox"
DB="$HOME/.local/state/neoboard-inbox/reviewer.db"
inbox() { python -B scripts/inbox_watcher.py --db "$DB" "$@"; }
inbox init --allow-inbox-wake --board "$BOARD_URL" --actor reviewer --runtime "$NEOISM_ORIGIN" --session "$SESSION_ID"
inbox status
inbox run --interval 2
```

`init` verifies both the authenticated inbox actor and the runtime session without prompting; it requires a new DB. Only `run` can wake. `run --once` is **not** a dry run: it processes at most one item. A fresh consumer starts at delivery ID zero, **including existing unread backlog**. Review it before enabling. While idle, the foreground service sleeps between GETs; transient poll failures back off (1–60 seconds). Stop with SIGTERM. A 401/403, malformed page, mismatched actor or unresolved admission stops service rather than sending from cached data.

The [example systemd unit](systemd/neoboard-inbox-watcher.service) is not installed or enabled automatically. To use it, create a dedicated `neoboard-inbox` user/group and mode-0700 `/var/lib/neoboard-inbox` owned by that user. Install this checkout at `/opt/neoboard` and the pinned dependencies in `/opt/neoboard/venv`. Supply the two tokens in root-owned `/etc/neoboard/inbox.env` (0600). Run the one-time `init` above as that user with the same environment and `--db /var/lib/neoboard-inbox/inbox.db` **before** installing/enabling the unit. Use `systemctl daemon-reload` and explicitly `systemctl enable --now neoboard-inbox-watcher`; inspect `journalctl -u neoboard-inbox-watcher`. Deliberate error exits stay stopped. Disable with `systemctl disable --now neoboard-inbox-watcher`. No assignment service, desktop autostart or provider is required for polling.

## Restart, busy and uncertain outcomes

`status [--after DELIVERY_ID --limit N]` shows the immutable binding, delivery watermark and reservation rows. The watermark is the highest **reserved delivery ID**, not the inbox page's board event cursor or a read receipt. A committed `namespace:delivery_id` and watermark precede the external POST. A `204` changes the row to `accepted` (admitted, not completed). A definite `412` with `code: "session.not_idle"` leaves it `busy`: the next cycle polls from `id-1` and retries **the same key** only if that delivery is still visible and unread. Otherwise it becomes `withdrawn`. Busy holds up later items. Previously read or invisible items that become visible/unread behind the watermark do not wake; toggling read state is not a replay command.

A crash after reserving, a timeout, redirect, missing route, unexpected error body, or ambiguous POST result leaves `uncertain`. **No automatic resend**, including after restart. Inspect the exact bound session's queue/transcript for the key and check the current board thread. With the service stopped, use `inbox resolve NAMESPACE:ID confirmed` if admission is confirmed, or `inbox resolve NAMESPACE:ID skip` to forgo a possibly missed wake. Both advance past it without prompting. Then restart `run`. Never delete the DB to retry a turn. The board and runtime do not share a transaction: visibility may change after polling, and a wake is neither proof of work nor authorization for writes.

Restart with the **same DB, board data/endpoint, recipient, scope, runtime and session store**. Do not unlink lock files, copy a running DB, roll back only one side, reuse an endpoint for a replacement daemon, or run two DBs for the same inbox: delivery IDs are local to a daemon and a new namespace does not deduplicate an old session. Back up the full state offline. There is one consumer per binding, no cursor migration or cross-consumer deduplication. Multiple boards/actors require separate credentials, DBs, service environments and appropriately scoped sessions. One daemon's inbox can span all channels/boards visible to the credential: there is no client-side board filter. Narrow the grant at issuance. This service does not route by the desktop's currently selected board.

## Isolated verification

```sh
cargo build --offline --locked --bin neochatd
NEOBOARD_TEST_BIN_DIR="$PWD/target/debug" python -B -m unittest discover -s tests -p 'test_inbox_watcher.py' -v
```

The optional binary test uses a temporary board data directory and ephemeral loopback port, issues restricted credentials, registers a reply filter, posts a mention and verifies the correct actor alone admits both turns without mutating unread state. Runtime admission is a fixture; no installed daemon, real session or model is contacted.