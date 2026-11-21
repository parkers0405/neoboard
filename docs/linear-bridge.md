# Linear issue connector

`scripts/linear_bridge.py` (Python 3.9+, standard library) maps one Linear organization to one NeoBoard channel. Each issue UUID gets one thread with a `linear_id` label. The root text mirrors the current identifier, title, description and workflow state name/UUID. A changed issue identifier does not change the thread. Linear comments become replies labeled `linear_comment_id`; a comment arriving before its issue creates a placeholder root. Older or equal `updatedAt` snapshots do not overwrite newer ones.

New board replies under mapped roots become Linear comments. To request a state change, reply with exactly `!linear-state <workflow-state-uuid>` using a hyphenated UUID valid for the issue's team. Linear's subsequent Issue update supplies the authoritative root text. Other reply text, including malformed commands, is sent as a comment. Board task leases and completion are independent and never changed. There is no board-originated issue creation, historical Linear backfill, comment edit/delete sync, attachment transfer or deleted-thread recovery.

## Bind and operate

Use a private persistent DB per binding. Example JSON config:

```json
{"organization_id":"<Linear organization UUID>","channel":"project/dev","board_url":"https://board.example.org","board_actor":"linear-bridge"}
```

Choose a channel whose readers may see **all** content in the webhook subscription and whose writers may publish comments and change states in Linear. There is no per-issue ACL translation. Use a dedicated stable channel-scoped NeoBoard credential (events, post, reply with labels, edit own messages) in `NEOCHAT_TOKEN`, a dedicated personal Linear API key authorized for comments and issue updates in `LINEAR_API_KEY` (poll only), and a unique `LINEAR_WEBHOOK_SECRET` for intake. Do not store credentials in the binding. Credential replacement can invalidate board-side idempotency history; drain and reconcile in-flight operations before rotation.

Subscribe Issue create/update and Comment create. Use a trusted HTTPS ingress, not a shell or a public invocation of this script. Preserve the raw body, verify the Linear HMAC-SHA256 signature, enforce body size/time limits and freshness of signed `webhookTimestamp` at ingress (within one minute). Durably enqueue before acknowledging. Invoke with `Linear-Delivery` and `Linear-Signature` as distinct argv values and unmodified bytes on stdin; the worker verifies the signature again. Retain failed jobs for investigation/redelivery. Use an organization/team subscription that matches the binding's audience.

```text
python3 scripts/linear_bridge.py state.sqlite binding.json webhook DELIVERY SIGNATURE
python3 scripts/linear_bridge.py state.sqlite binding.json poll
```

Schedule `poll` repeatedly. It starts from cursor zero, follows returned event cursors, and can export existing board replies to mapped roots. Events outside the configured channel, unmapped replies, bridge-authored events, and subsequent upserts for known message IDs are not exported. The bridge DB is durable operational state, not a cache. Stop workers for backup/restore; stale backups can replay writes. Do not share a DB across bindings.

## Duplicate delivery and uncertain writes

Inbound delivery receipts, source UUIDs and thread/reply mappings commit together. Stable action keys allow retry after a board write succeeds but its response is lost. An outbound comment reserves a deterministic Linear UUID before sending, so its webhook does not echo. An unconfirmed outbound GraphQL mutation leaves the message pending and stops polling. Stop workers and inspect the `exports` table (`message`, `target`, `done=0`), board reply, and Linear comment UUID or issue state history. Only if the write landed, run `python3 scripts/linear_bridge.py state.sqlite binding.json confirm MESSAGE`. Only if it definitely did not land and is no longer in flight, use `retry-absent MESSAGE` instead. Resume polling afterward. Current state alone is not proof that a state mutation landed; leave uncertain records pending.

Offline verification: `python3 -B -m unittest discover -s tests -p 'test_linear_bridge*.py' -v`. The suite uses temporary databases and in-process service fixtures; it does not contact either service.