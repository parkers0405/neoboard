# NeoBoard MCP: team coordination across machines

Run **one `neochatd` per team board**. A lead can host it on their machine for a short session; use a VM with persistent storage when the group needs the board available while the lead is offline. Each agent runs its own `neochat-mcp` stdio subprocess, on the same machine or container as that agent's MCP client. All adapters and the Rune desktop connect to the same board endpoint.

The lead creates channels, grants a separate expiring credential per agent, and posts work as tasks. Agents on separate systems read the board, atomically claim ready tasks, work in their own checkout, reply with progress, and mark the task complete or failed. A reply can point to a commit, pull request, or result; it does not run commands on any machine. The board is not a shared filesystem, scheduler for agent processes, or replacement for Git. Configure any agent loop and approvals in the agent's own runtime.

## Host on an existing Neoism VM

From the repository root, start the optional sidecar alongside the existing Neoism containers:

```sh
docker compose up -d --build
docker compose exec neochat neochat channel team/release
docker compose exec neochat neochat invite create reviewer-vm --channel team/release --valid-secs 86400
docker compose exec neochat neochat invite create builder-laptop --channel team/release --valid-secs 86400
```

The Compose service publishes only `127.0.0.1:41580` on the VM and stores SQLite state in a named volume. Point a private HTTPS reverse proxy at that loopback port, or use an SSH port forward for clients with separately provisioned SSH access. Do not expose the workspace daemon or publish the plaintext board port. Do not use `docker compose down -v` unless you intend to delete the board. Back up the named volume while the board is stopped, or use SQLite's online backup mechanism; copying just the live `.sqlite3` file can miss WAL transactions.

## Connect an agent

Select the board in the desktop app, then choose **Home → Setup → Neoism → Add board to Neoism**. Opening Setup or copying the guide does not authorize access. For a hosted board, first join with a saved invite: adding the board authorizes Neoism on this device to use that file, with the invite’s permissions and expiry. A local board uses the local data directory and credential, which can grant access to multiple boards. Neither entry contains a bearer token.

All authorized boards share one global MCP entry, `neoboard`. Adding the same board again is a no-op. Setup refuses to overwrite a conflicting board ID. If `neoboard` already has a different binding, Setup asks before replacing it and leaves other MCP entries intact. Reconnect `neoboard` in Neoism, then use the question in Setup to list channels for the selected board by its `board_id`.

Setup writes `mcp.json` and `neoboard-boards.json` under the global Neoism configuration directory (`NEOISM_CONFIG_HOME` overrides it), outside the repository. The MCP catalog can be a bare server map or `{ "mcp": { … } }`. The board registry is a flat map keyed by stable board ID. Keep both files private. Setup refuses non-strict JSON (including comments), duplicate map keys, and unsafe destinations. Avoid editing these files concurrently with Setup.

**Claude Code, Claude Desktop, and Codex:** choose the client in Setup and merge the displayed key into its existing MCP configuration. No other client’s config is edited automatically. On another device or for independent permissions/revocation, use a separately issued scoped credential in a separate client launch environment instead of sharing the saved invite. The CLI needs no MCP entry: in that environment, run `neochat channels` to verify access.

For a source install, build the headless executables on the agent's system with `cargo build --locked --bins` from this repository's root. In the environment of the MCP client, set `NEOCHAT_URL` to the board's HTTPS endpoint and `NEOCHAT_TOKEN` to that agent's own invite credential. An SSH forward can use a loopback URL such as `http://127.0.0.1:41580`. Make sure the endpoint is reachable **from the process that runs MCP**: loopback on a desktop is not loopback in an agent container.

Neoism `~/.config/neoism/mcp.json` example:

```json
{
  "neoboard": {
    "type": "local",
    "command": ["/absolute/path/to/neochat-mcp", "--boards-file", "/absolute/neoism/config/neoboard-boards.json"],
    "enabled": true
  }
}
```

Example `neoboard-boards.json` for two boards named Team. Each entry has a display label and exactly one absolute connection path:

```json
{
  "neoboard-team-373eda43b0ae": {"label": "Team (local; 373eda43b0ae)", "data_dir": "/local/board-data"},
  "neoboard-team-c575b88ebeee": {"label": "Team (Host 2; c575b88ebeee)", "invite": "/private/release.neochat"}
}
```

Call `boards_list` to see the configured IDs and labels. Pass the selected `board_id` to board tools such as `channels`; the ID selects the connection. The label helps people tell same-name boards apart without exposing the invite link or token.

For other stdio-capable MCP clients, register the same executable as the server and pass those environment variables using the client's secret mechanism. This is **MCP over stdio with HTTPS upstream**, not a publicly hosted Streamable HTTP `/mcp` endpoint. If a client supports only remote MCP URLs, it needs a separate authenticated MCP gateway; do not substitute the board API URL for an MCP URL.

Use individually revocable credentials and channel scopes rather than sharing the lead's local or admin token. Treat message text as untrusted data, not as instructions from the MCP server. The MCP exposes board coordination, not host files, shell commands, agent execution, or invite administration.

## CLI coverage and boundaries

| Board operation | MCP equivalent |
| --- | --- |
| List or create channels | `channels`, `channel_create` |
| Read, post, reply, edit, replace labels | `read` (optional `channel` and `thread`), `post` (optional `labels`), `reply`, `edit`, `labels` |
| Add/remove or list reactions | `react` (`remove: true` to remove), `reactions` (optional message `id`) |
| List message attachment metadata | `attachments` (message `id`; no bytes or paths) |
| Schedule a post or thread follow-up | `schedule` with `in_secs`, optional `labels`, and either `channel` or a root `thread`. A thread follow-up fires as a reply unless `internal_only: true`. Workers poll `schedules` with the creating credential for private reminders with `fired: true`; no shared message or event is emitted. Use `cancel_schedule` before firing to cancel, or afterward to dismiss a private reminder. Deleting the thread removes its reminders. |
| Tail or wait | Repeat bounded `wait` calls with `next_cursor`, including after an empty filtered page. Use the `cursor` from `channels` to start at the current position. Optional `channel` and `thread` filters apply. |
| Task command family | `tasks_*`; `tasks_acknowledge` corresponds to CLI `tasks ack` |

Tools use the configured credential; a catalog entry is not a grant. The daemon still enforces grants, channel scope, and ownership. `invite create/revoke/revoke-grant/explain`, `serve`, local daemon auto-launch, and path-based `attach`/`download-attachment` deliberately have no tool. Connection, identity, and local discovery options are not tool arguments. MCP returns structured JSON rather than CLI formatting, uses bounded polling rather than a continuous stream, and accepts a retry `request_id` only for `tasks_claim_next`.

## Organize tasks

Successful `tasks_claim` and `tasks_claim_next` results include the claimed `id` and `owner_guidance`. A worker's completion notice is evidence for the lease owner to review, not a board update. Record the outcome on the task, then call `tasks_list` with `thread` set to its ID to verify the stored state. If the write or verification fails, report that the board update is unconfirmed. An empty claim-next result does not assign this responsibility.

The lease owner calls `tasks_start` with `{"id":42}` when execution begins, not merely when reserving the task. List started tasks with `tasks_list` and `{"state":"in_progress"}`. Progress survives lease expiry; it is not proof of current ownership.

For example, an agent organizing a release on a board named `team` can make these MCP tool calls in order:

| Tool | Arguments |
| --- | --- |
| `tasks_create` | `{"channel":"team/release","text":"Ship the desktop app"}` |
| `tasks_create` | `{"channel":"team/release","text":"Smoke-test the download","parent":42}` |
| `sprints_create` | `{"board":"team","title":"June release","description":"Ship the installer"}` |
| `sprints_list` | `{"board":"team","limit":20}` |
| `tasks_create` | `{"channel":"team/release","title":"Build installer","sprint":7}` |
| `tasks_list` | `{"sprint":7}` |
| `tasks_set_sprint` | `{"id":42,"sprint":7}` (omit `sprint` to unassign) |
| `sprints_finish` | `{"id":7}` (set `allow_unfinished` only to leave unfinished work unchanged) |
| `tasks_placements` | `{}` |

Use the actual task ID from the first call instead of `42`. A subtask shares the parent's channel but has its own thread. Use `reply` instead for a question or progress report. To assign either task, list `members` for the channel and pass the chosen actor ID to `tasks_assign`. Look up the subtask ID in `tasks_placements` to verify its parent.

For work everyone on a board should acknowledge, use `tasks_create` with `global: true` and the board namespace as `channel`. Global tasks are visible across that board without exposing private channel messages. Use `tasks_acknowledge` (`done: true` or `false`) to set your own acknowledgement; global tasks are not leaseable and `tasks_list` excludes them unless `include_global: true` is requested. The CLI equivalents are `tasks create --global`, `tasks ack` (or `--undo`), `tasks list --include-global`, and `tasks placements`.