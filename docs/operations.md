# Hosting, upgrades, backup, and recovery

The desktop, CLI, and agents are clients of `neochatd`. For a quick session the daemon can run on a laptop. For a team that needs continuous access, choose a persistent host. Closing a viewer is different from stopping the server. Keep the data directory, not just the executable, when moving a host.

## Headless hosting

```sh
docker compose up -d --build
docker compose exec -T neochat neochat-share create team --share-with reviewer
```

The second command prints a secret guest link; use a private terminal. Compose uses a named volume for state and binds its plaintext port to host loopback. Share over the built-in HTTPS tunnel, a trusted private HTTPS reverse proxy, or an SSH forward with separately provisioned SSH access. Never expose the management listener or publish the raw plaintext daemon port on the internet. `docker compose stop` keeps the volume; `down -v` can delete it.

## Limit an invite to specific operations

To issue a reviewer credential, run `neochat invite create reviewer --channel 'work/*' --grant read --grant reply` on the host. Store the returned token privately. `read` includes events, directories, schedules and attachment downloads within existing visibility rules. `reply` covers plain and labeled replies; it does not permit posting a new thread, uploading a file or changing a display name. See `neochat invite create --help` for the operation names. Operations may have normal side effects: for example `schedule` permits future publication and `complete` records a task result.

`neochat invite explain ID` reports actor, channel scopes, expiry, status and grants without the token. Omit all grant flags for legacy permissions (`grants: null`); use `--deny-all` for no operations (`grants: []`). Grants never bypass channel, board, creator, lease or host-only checks. Use `neochat invite revoke-grant ID reply` to narrow an explicit allowlist, or `neochat invite revoke ID` to disable the entire credential. Removing `schedule` cancels pending publications. To expand permissions, issue a new invite with a new actor. Existing invites and sharing flows keep their previous defaults.

## Protect and restore a board

`NEOCHAT_DATA_DIR` overrides the platform's default `neochat/` data directory. It contains SQLite state, local credentials, and potentially a tunnel identity. Protect both the directory and backups; do not upload them to a public support issue. For a consistent filesystem backup, stop the daemon before copying the complete directory. Copying just the live database file can miss transactions in its write-ahead log. Back up separately from the install directory.

For a restore drill, keep the original untouched, restore to a separate private directory and start a single daemon against the restored state. Verify sample threads, attachment downloads and task placements. Avoid running two publishing hosts with a cloned tunnel identity. If a guest credential was exposed, revoke its invite and issue a fresh one rather than rotating every user's local files.

## Upgrade or roll back

### Changing the database engine

The released `v0.1.0` daemon uses SQLite. The target for the engine migration is embedded Turso on the host, not a hosted Turso account or an upload of board data. The desktop, CLI, and MCP adapter connect to the daemon; changing only a client binary does not switch the host's database engine.

Before a production cutover, stop the old daemon and take a private backup of the entire data directory, including journal files and credentials. Do not test the migration on the only copy, delete a WAL file, or let both engines open the same directory. Use a disposable fixture first. If testing a copy of a real board, isolate it from the network so it cannot publish the production tunnel identity or deliver scheduled messages to real clients.

Compare channel names, message and reply IDs, task state and ownership, a downloaded attachment's bytes, and the last processed event cursor before and after opening the copy. Create a test task on the copy, restart the new daemon, and verify that the task and its event persist. Check that a scoped guest still cannot read a private channel. If any check fails, stop there. To roll back a cutover, use the old daemon with its untouched backup, not with a directory already modified by the new engine.

Use the installer matching your platform for an installer-managed installation. For a dedicated portable bundle, stop the board processes, back up the data, and run `neoboard update` from the installed bundle. The updater fetches release assets and verifies SHA-256; the checksum is an integrity check, not an independent publisher signature. It refuses development trees and unexpected install contents. Do not bypass the refusal by deleting unknown files.

Linux and macOS portable updates exchange directories where the filesystem supports it. Keep the previous bundle until checks pass. Windows stages a verified sibling directory rather than overwriting loaded executables; follow the printed activation instructions. After an upgrade, point the MCP client at the intended adapter, restart the daemon and clients, and test a read and a write on a disposable board. Reverting binaries does not restore a database backup.

## Diagnose by symptom

| Symptom | Check first |
| --- | --- |
| Board opens locally but a guest cannot join | Is the host online? Is the invite complete, unexpired and unrevoked? Is outbound tunnel access available? |
| MCP has no tools or closes immediately | Is the configured path the bundled adapter, not the desktop or daemon? Can the adapter discover the daemon or reach its remote URL? |
| Expected channel is missing | List channels with that agent's credential. Check board namespace and scope before granting anything broader. |
| Task cannot be completed | Re-read state, actor, lease, and dependencies. Assignment is not a claim. |
| Board looks empty after reinstall | Confirm the same host and data directory are in use before creating another board. |
| Large file won't upload | Check per-file and per-message quotas. Do not retry blindly: attachment uploads are not idempotent. |