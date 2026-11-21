# GitHub issue and PR conversation bridge

`github_bridge.py` is a Python 3 standard-library adapter, not a public HTTP server. One SQLite database binds one board URL, channel, repository and dedicated pair of actors. An `issues.opened` or `pull_request.opened` delivery creates a board thread plus a reply containing the title and body. New issue/PR discussion comments and inline PR review comments become replies in the same thread, even if a comment arrives first. A board root with label `github=issue` creates a GitHub issue. New replies under a mapped root create GitHub discussion comments (not inline code reviews). Unmapped replies and unlabeled roots are not exported.

## Deployment contract

Example binding file (identifiers are placeholders):

```json
{"board_url":"https://board.example.org","channel":"project/dev","board_actor":"github-bridge","repo":"example/project","repo_id":123,"github_actor_id":456}
```

Use a dedicated board actor with channel-scoped read/post/reply access and a dedicated GitHub identity with repository issue write and PR comment permissions. Configure `NEOCHAT_TOKEN`, `GITHUB_TOKEN`, and (on the webhook receiver) `GITHUB_WEBHOOK_SECRET` privately. Use a private directory for the SQLite database. Do not share it across boards or reset it on restart. Stop workers before backup/restore; stale backups can replay historical writes. Treat the binding as immutable; a repository transfer, board replacement or actor change needs a reviewed migration.

Subscribe a trusted HTTPS receiver to Issues, Pull requests, Issue comments and Pull request review comments. Have it invoke the following command with the three header values as separate arguments (no shell interpolation) and the **unaltered request body** on stdin. Acknowledge only after exit zero. Retain and redeliver failed requests: GitHub does not automatically retry every failed delivery. Apply ingress size/time limits and keep the secret unique to this webhook. Signature validation covers the raw body, not the event and delivery headers; object IDs also guard against replays under new delivery IDs.

```text
python3 scripts/github_bridge.py state.sqlite binding.json webhook EVENT DELIVERY SIGNATURE
python3 scripts/github_bridge.py state.sqlite binding.json poll
```

Use `X-GitHub-Event`, `X-GitHub-Delivery`, and `X-Hub-Signature-256`, respectively. Schedule `poll` repeatedly; each invocation processes one events response. The first poll starts at cursor zero and can export **existing** labeled roots and mapped replies, so review channel history first. Label-based opt-in is not a separate authorization boundary: only grant board access to people permitted to publish to this repository. Text travels in both directions, including mentions. Edits, closures, review summaries, attachments, deletions, reactions, and thread recreation are not synchronized.

## Recovery and loop prevention

Optional immutable binding field `issue_task_assignee`: set this to a board actor to import issue roots as assigned tasks (also when a comment precedes the opening event). This requires the `post_task` grant. Pull requests remain conversations. The assignee comes from operator configuration, never the webhook body. Assignment does not itself claim the task.

Imports use stable board idempotency keys and commit delivery ID, source object ID, returned board ID and mapping together. Exports deduplicate by board message ID, not event cursor. The bridge ignores its own GitHub object author ID, its authenticated board event actor, and subsequent updates of imported messages. The HTML marker `<!-- neoboard:ID -->` aids inspection; it is not trusted as proof of origin.

GitHub does not provide an exactly-once comment POST. The bridge records a pending export **before** sending. If an attempt is interrupted, it will not retry it blindly. Stop poll workers, inspect the pending message, the remote issue/comment, marker, text and authenticated creator. If it landed, run `python3 scripts/github_bridge.py state.sqlite binding.json confirm MESSAGE REMOTE` (issue **number** for roots, comment **ID** for replies). Only after verifying the request is no longer in flight and definitely did not land, run `retry-absent MESSAGE` instead. Resume polling and redeliver failed webhooks. If uncertain, leave it pending. While issue creation is pending, inbound deliveries needing a new root are deferred to avoid a second mapping. No background service, automatic reconciliation, historical GitHub backfill or live credentials are included.

Offline checks: `python3 -B -m unittest discover -s tests -p 'test_github_bridge*.py' -v`.