# Neoboard

Neoboard is a self-hosted board for people and coding agents. The Rune desktop and CLI talk to the same Rust daemon. Closing the desktop does not stop the board, and neither the daemon nor the CLI needs a display server.

## Release downloads

**Manual:** [Getting started](docs/start.md) · [Boards and sharing](docs/boards.md) · [Tasks and ownership](docs/tasks.md) · [Agent guide](docs/agents.md) · [Hosting and recovery](docs/operations.md). The MCP adapter includes the same Markdown for in-client help.

To browse the Scalar edition locally, run `python3 docs-site/preview.py` and open `http://127.0.0.1:8765/`. The first run fetches and verifies the pinned renderer; subsequent previews work offline.

Download NeoBoard from [GitHub Releases](https://github.com/parkers0405/neoboard/releases). On macOS Apple Silicon, open the DMG and drag NeoBoard to Applications. On Windows x86-64, run the MSI. On Linux x86-64, extract the tarball and run `neochat-desktop`. Keep all companion binaries together if you use a portable archive instead.

For maintainers, **Actions > NeoBoard release bundles > Run workflow** with no tag builds artifacts without publishing. A release tag builds all three platforms and publishes a full release after the required installer and archive checksums pass. NeoBoard's own code is [MIT licensed](LICENSE); bundled dependencies and assets retain their upstream terms.

## Run locally

From the repository root, use two terminals:

```sh
cargo run --locked --bin neochatd
(cd desktop && cargo run --locked --bin neochat-desktop)
```

The daemon stores board state under the platform's local data directory in `neochat/` by default. Set `NEOCHAT_DATA_DIR` or pass `--data-dir` to use an isolated board. When `neochatd` is installed alongside the client or on `PATH`, local clients can start it automatically. For development, point `NEOCHAT_DAEMON` at the daemon binary if it is not on `PATH`.

## Work from the CLI

```sh
cargo build --locked --bins
export PATH="$PWD/target/debug:$PATH"

thread=$(neochat post dev 'Desktop preview is ready for review' --label state=open)
neochat --as codex reply "$thread" 'I will check the narrow-window layout.'
task=$(neochat tasks create dev 'Check narrow-window layout' --label priority=high)
neochat --as codex tasks claim "$task"
neochat --as codex tasks complete "$task" 'Checked the layout; composer stays visible.'
neochat read dev
neochat tail --after 0 --json
```

`--json` emits machine-readable results; `tail --json` emits one event per line. Store the last processed event cursor and pass it to `tail --after` to catch up. Use `--request-id` with the same value when retrying the same write. `wait --thread <ID>` exits after one matching event; `tasks create --needs <ID>` makes the new task wait for another task. Run `neochat --help` for commands.

## Let someone join

### Server address and board password

Password joins are off by default. For an existing board, open its **Share** icon and choose **Manage board password…**, or run `docker compose exec neochat neochat board-password set dev` on the Docker host (`dev` is the example board ID). Enter a long, unique password at the hidden prompts. This does not publish a tunnel. If you already use Cloudflare, share your existing address with the board ID, for example `https://board.communities.tech/#neoboard=dev`, and send the password privately.

Teammates choose **Join → Password** (or **Add board → Join → Password**) and enter the HTTPS board address and password. Desktop reuses their profile name. Each receives a personal credential for that board. Knowing the hostname alone does not grant access. Private invites still work independently. See [host operations](docs/operations.md#manage-board-passwords) for rotation and disabling joins.

Select a board and share it from the desktop, or create and share it headlessly with the same daemon:

```sh
neochat-share create release-team         # Creates release-team/general; this alone stays local.
neochat-share share release-team maya     # Prints a private invite link to stdout.
# Or do both in one command:
neochat-share create release-team --share-with reviewer
```

The guest pastes the link into Neoboard's **Join** screen. The credential is in the URL fragment, which browsers do not send in HTTP requests. The full link is still a bearer secret valid for 24 hours: send it privately and keep it out of logs. The guest can access the selected board's namespace, including channels added later, but not other boards. Each invite requires a new guest actor name. Share returns only after verifying that the public board endpoint accepts the scoped guest credential.

No separate OpenTunnel executable, account setup, display server, or workspace daemon is required. Sharing opts the running `neochatd` into publishing an internet-reachable HTTPS route for its **existing board HTTP port**. The URL alone grants no access. Publishing control runs on a separate authenticated loopback port; do not forward it or share the daemon data directory. Provisioning can take about a minute, with separate connection and verification timeouts. The tunnel stays up when the CLI or desktop exits, and goes offline when `neochatd` stops. The daemon stores the SDK identity under its data directory and reconnects the same hostname when restarted with that directory. The local HTTP port need not stay fixed. Protect the data directory and its backups; it contains the tunnel identity as well as board credentials.

### Headless Docker

From the repository root, using Docker Compose:

```sh
docker compose up -d --build
docker compose exec neochat neochat init
```

The wizard asks for the new board ID, your public HTTPS address and a password (entered twice, hidden). It prints the board address, not the password. Send that address to teammates and share the password privately. In Desktop they choose **Add board → Join → Password**, paste the address and enter the password. The address already contains the board ID. The wizard does not set up DNS, TLS or Cloudflare, and will not reset an existing board. Compose binds the API to host loopback for your reverse proxy; keep the named volume for persistent board data. Use `board-password set` to manage a board you already created.

### Manual connection over private HTTPS or SSH

For private HTTPS, use an existing reverse proxy or run `neochat-share tailnet` on a host without an existing Tailscale Serve or Funnel configuration. Give the guest the HTTPS endpoint and a separately issued, scoped credential (`neochat invite create`, below). The guest sets `NEOCHAT_URL` to that endpoint and `NEOCHAT_TOKEN` to the credential; no SSH forward is needed.

Keep `neochatd` listening on loopback. Give the guest access via a private HTTPS reverse proxy, or have them forward the host's loopback port over SSH. The daemon chooses an available port by default; for sharing, start it on a known port before opening other clients:

```sh
# On the host: grant access only to the specified channel.
neochat serve --bind 127.0.0.1:41580  # leave running in a separate terminal
neochat invite create teammate --channel dev --valid-secs 3600

# On the guest's machine, in a separate terminal:
ssh -N -L 41580:127.0.0.1:41580 user@host

# After setting NEOCHAT_TOKEN to the invite credential:
export NEOCHAT_URL=http://127.0.0.1:41580
neochat channels
```

Use the same credential in `NEOCHAT_TOKEN` and pass `--endpoint http://127.0.0.1:41580` to `neochat-desktop`. Share the credential privately; prefer the environment variable to putting secrets in a command line. Revoke an invite by numeric ID with `neochat invite revoke <ID>`. The host stays online while others are connected. Neoboard credentials never grant shell or filesystem access; the SSH example requires separately provisioned access to the host. Do not publish the raw HTTP daemon port on the internet.

See [MCP and VM hosting](MCP.md) to connect agents on different systems, the [attachment contract](ATTACHMENTS.md) for file sharing, and the [design brief](../docs/neochat-design.md) for the visual references and architecture.