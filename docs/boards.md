# Boards, channels, and sharing

A board groups channels in a namespace. For example, `team/release` is a channel on `team`. An invite scoped to a board can cover channels added later; a channel-only credential is narrower. Seeing the board name is not authorization to read every other board on that daemon.

## Invite a guest

For a reusable board password instead of an invite, the local host opens **Share → Manage board password…**, enters the new password twice, then confirms the change. Share the full HTTPS board address with teammates and send the password separately. The address identifies the board (`#neoboard=dev` selects `dev`); a bare server address defaults to `general`. The credential grants access to that board's namespace, not other boards on the server. Use scoped invites if a teammate needs narrower access. Once connected, **Setup → Neoism → Add board to Neoism** uses the personal membership, not the board password.

On the host desktop, open the board picker and select the share icon beside the local board. Create an invite, wait for OpenTunnel to provision and verify its HTTPS endpoint, and copy the link privately. The guest chooses Join and pastes the entire link. There is no second password: the fragment contains the credential. Do not paste links into issue trackers, public channels, screenshots, or agent prompts.

The host daemon must stay online. Closing its desktop window does not normally stop the daemon; shutting down the host does. A remote guest does not become a host just by joining. A folder in the picker denotes a local board; a cloud denotes a remote connection, not a guarantee that the host is currently reachable.

## Host from a terminal

```sh
neochat-share create team
neochat-share share team maya
```

The first command creates the board locally. The second provisions sharing and prints an expiring guest link. Issue a distinct guest identity when inviting another person. If an invite expires or is revoked, use a new credential; retrying the public URL without its invite fragment cannot fix authorization.

## Conversations and files

Post in a channel to start a topic. Reply to the parent thread to keep follow-up questions attached; create a subtask instead when work needs its own status or assignee. Labels describe messages but do not grant access. Scheduled posts use UTC: confirm the intended time before submitting one. Pending posts appear in Scheduled until published or canceled.

Attachments are stored with the board and require channel access to download. The limit is 8 MiB per attachment, 16 attachments per message, and 32 MiB total per message. A filename is metadata, not a trusted path. From the CLI, use an explicit output file and inspect a download before opening it:

```sh
neochat attach 42 ./notes.txt
neochat attachments 42
neochat download-attachment 7 ./received-notes.txt
```

Here `42` is a message ID and `7` is an attachment ID. If a guest cannot see an item the host can see, check channel scope and invite expiry before treating it as deleted.