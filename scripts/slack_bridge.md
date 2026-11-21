# Slack conversation bridge (#20)

One reviewed binding pairs exactly one Slack workspace/app/conversation ID with one exact NeoBoard channel. Roots map to roots and replies stay in their mapped thread, including replies authored on the opposite side. Message text is mirrored; attachments, reactions, task metadata, bot/system messages and Slack message subtypes other than ordinary edits/deletes are not. A human message containing a bot name or marker is not proof of origin.

## Authorize before enabling

Use `tests/test_slack_bridge_fixtures/binding.json` as a non-secret binding template. An operator must authorize **both audiences** to see the other's text, including channel history. A token's ability to read a conversation is not that authorization. Never select conversations by name, wildcard, incoming message text, or channel discovery. Do not bind private channels, DMs, multi-party DMs or Slack Connect conversations unless every affected audience and history transfer is approved. Prefer a dedicated empty channel pair. Each additional pair needs its own immutable binding and database; do not reuse either endpoint in another binding. Retire the bridge before membership changes, a channel rename/deletion/recreation, workspace migration, or permission revocation. Stop ingress and worker together.

Use a dedicated Slack app and bot identity with `chat:write` and only the history scope and `message.*` event subscription for the approved conversation kind. Invite the app explicitly. Use a dedicated board identity with channel-scoped read/post/reply/edit grants; verify `board_actor` matches its authenticated actor, not its display name. Do not use either identity for ordinary posts. Provide `SLACK_SIGNING_SECRET` only to ingress; provide `SLACK_BOT_TOKEN` and `NEOCHAT_TOKEN` only to the worker. `auth.test` checks the worker's Slack team and bot user before sync. Board traffic requires HTTPS except on loopback; neither client follows redirects. Store binding, database, worker lock and backups together in an operator-owned private local directory; queued events contain plaintext. Do not copy stale state over a running worker.

## Receiver and worker

This POSIX Python 3 standard-library CLI is not a listener. Have a trusted HTTPS receiver invoke the following with separate argv values (no shell), Slack's `X-Slack-Request-Timestamp` and `X-Slack-Signature`, and the unmodified request bytes on stdin. Bound body size (1 MiB), execution time and concurrency at ingress. Return the printed challenge for signed `url_verification`. For event callbacks acknowledge success only after exit zero; the signed, five-minute freshness-checked, scope-filtered event is then durably enqueued or ignored. Return retryable failure on local storage errors. Keep clocks synchronized. Do not log message bodies, signing headers or tokens.

```text
python3 scripts/slack_bridge.py state.sqlite binding.json receive TIMESTAMP SIGNATURE
python3 scripts/slack_bridge.py state.sqlite binding.json work
```

Schedule `work` repeatedly with capped exponential backoff and jitter on errors. Each run consumes a board events page and drains queued Slack deliveries. The first poll starts at **cursor zero** and can export existing board history. There is no Slack backfill. Late replies wait durably for a mapped root; the worker reports the waiting count. If a root predates installation, was unsupported or never arrives, its replies remain queued rather than posting in the wrong place. Monitor backlog, failures and disk usage. Only run one worker/reconciler per DB; the CLI enforces a local advisory lock. Do not run across machines, hard-linked paths or network filesystems.

## Mutations, retry and recovery

Slack imports use stable board idempotency keys, event-ID deduplication, message timestamps and edit revisions. Deletion wins over late replay. Deleting a Slack-origin message **edits** the board mirror to `[Deleted on Slack]`: the board cannot delete an individual reply, and deleting a root would also erase other people's replies. A board upsert with changed text updates the same Slack bot post; metadata-only upserts are silent. Board thread deletion deletes only the bridge's Slack posts in that thread, never a Slack user's originals. Source-side edits are authoritative; editing/removing a mirror manually is not a supported reverse edit. Deleting a mirror does not purge historical logs, event copies or backups.

The worker advances the board cursor only after handling each event. Board action retries reuse the idempotency key; Slack updates repeat the desired text, and deletes tolerate `message_not_found`. A Slack HTTP 429 preserves the cursor and records the `Retry-After` deadline for export. For `chat.postMessage`, the database records intent **before** sending. A crash, lost response, or unclassified error stops polling with the pending board ID instead of blindly posting a duplicate. Stop the scheduler, inspect the source text, exact conversation/thread, bot identity, timestamps, and Slack history. If independently confirmed, supply that post's exact Slack timestamp; only use `absent` when certain the request is no longer in flight and never landed:

```text
python3 scripts/slack_bridge.py state.sqlite binding.json reconcile BOARD_ID SLACK_TS
python3 scripts/slack_bridge.py state.sqlite binding.json reconcile BOARD_ID absent
```

Resume `work` after reconciliation; leave ambiguous cases pending. This is not a distributed exactly-once guarantee. Unsupported/oversized content, persistent API errors or lost credentials can require operator intervention. There is no automatic audience verification, full-history reconciliation, or embedded scheduler. Test offline with `python3 -B -m unittest discover -s tests -p 'test_slack_bridge*.py' -v`; no credentials, listener, or live endpoints required.