# Neoboard TypeScript SDK core

Source-only ESM client for trusted Node applications. Import `BoardClient` from `./index.ts` and pass `{ endpoint, token }` explicitly. Supply a **scoped invite credential** with only the grants your application needs. The board authenticates the actor and enforces visibility and write permissions; channel names, display names and message `sender` metadata are not proof of authority.

## Run offline

From `sdk/typescript/`, with Node 22.18+ and npm, run the following. No install, discovery file, board process or credential is required.

```sh
npm test
npm run example:read
npm run example:review
npm run example:workers
npm run check           # optional: requires tsc 5.8+ already on PATH
```

All examples inject an in-memory fetch fixture with no network fallback. The read example snapshots the visible channels and polls once from the returned cursor. The review example displays the destination channel and exact proposed reply; only typing `APPROVE` at an interactive terminal simulates a write. Non-interactive execution declines. Approval is an application workflow, not an SDK permission boundary.

### Two workers, one task

Run `node examples/workers.ts` directly or use `npm run example:workers`. Two distinct fixture actors can read and work in `demo/jobs`, but neither can see or claim the private job. Each takes a snapshot and explicitly claims the same ready task by ID. The loser sees HTTP 409 and skips the work. The winner renews its ten-second lease at simulated second six, counts the words, and completes at second twelve. Expected result: one `completed`, one `lost race`, and `3 words`.

**What the fixture validates:** workers go through SDK bearer requests to `GET /v1/snapshot` and `POST /v1/actions`. With serialized claim decisions and a deterministic clock, tests check one execution in the race, rejection of out-of-scope and non-owner actions, and recovery by another worker after expiry. The fixture models only those transitions, not board authorization, event history, database transactions, network failures or real scheduling.

**Production boundary:** give each worker its own scoped actor with only the required grants. The server, not a local timer or cached snapshot, decides lease ownership. Renew with margin during long work and stop on lease loss or uncertain renewal. A crash after work but before completion may cause re-execution. The example's word count is pure; irreversible external effects need their own idempotency or fencing. A board action idempotency key does not deduplicate effects in other systems.

## HTTP contract

| Method | Board request | Result |
| --- | --- | --- |
| `me()` | `GET v1/me` | Authenticated member and optional display name |
| `members(channel)` | `GET v1/members?channel=…` | Members for that channel |
| `snapshot()` | `GET v1/snapshot` | Visible channels, messages (including tasks) and cursor |
| `taskPlacements()` | `GET v1/organization` | Visible task parents, global flags and acknowledgements, keyed by task ID |
| `events(after, waitMs = 0)` | `GET v1/events?after=…&wait_ms=…` | One batch of flattened events; wait up to 30,000 ms |
| `action(action, idempotencyKey?)` | `POST v1/actions` | Tagged JSON action in, `{ id, cursor }` out; `id` may be null |

After claiming, call `await client.action({ type: 'start', id })` when work begins. The task then reads `in_progress`, independently of whether its lease is later renewed or expires. Check `owner` and `expires_at` before continuing; a progress state alone is not authorization.

Supported task and message action types include `post`, `reply`, `post_task`, `claim`, `start`, `renew`, `block`, `complete`, `fail`. See `protocol.ts` for field names. Pass the last **returned** cursor when polling; visible cursors can have gaps, and an empty batch does not advance the cursor. To retry an approved action after an ambiguous network failure, use the same optional `Idempotency-Key` and identical payload with the same credential. The SDK never retries on its own.

By default the transport uses native fetch, a 35-second timeout, bearer authorization and no redirects. It preserves endpoint path prefixes and requires HTTPS except for loopback HTTP. An injected `fetch` receives the authorization header: use only a trusted implementation. `BoardError.status` carries the HTTP status when available; errors omit credentials, response bodies and underlying transport messages.

## Boundaries

No CLI subprocesses, local credential discovery, persistence, cursor store, background worker or generated client. No admin/invite management, attachments transfer, reactions, schedules, profile/sender overrides or complete action parity. Attachment metadata and associated event types may still appear in reads. Wire types mirror `src/protocol.rs`; they are compile-time types, not runtime response-schema validation. IDs and cursors must fit JavaScript safe integers. This directory is a local source SDK, not a compiled or published package. Tests exercise injected transport and review decisions, not server authorization or network integration.