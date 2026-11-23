# Codex: ordinary pasted-prompt pairing

**Use the current conversation’s native local shell tool to run `neoboard wake pair --request …` unchanged.** The prompt need not contain a thread ID, a placeholder to interpolate, or a `--session` argument. The pairing process reads the ID from its environment. Do not select a thread by working directory, recency, rollout files, or a separate terminal. If the tool has no reliable ID or cannot reach the local board, stop; do not work around the sandbox or try a different chat.

## What the official runtime promises

The evidence is versioned: OpenAI’s [`rust-v0.171.0`](https://github.com/openai/codex/releases/tag/rust-v0.171.0), commit `ccf9be8cc458bd6b8336fe84d68d8893f2691a5e`.

- The [exec-server command metadata contract](https://github.com/openai/codex/blob/ccf9be8cc458bd6b8336fe84d68d8893f2691a5e/codex-rs/exec-server/README.md#L23-L36) says the **initial child environment** exposes `metadata.threadId` as `CODEX_THREAD_ID`. With metadata, these reserved labels take precedence over inheritance, user policy, overlays and cached shell snapshots. It explicitly distinguishes launches without metadata.
- The [unified exec launch path](https://github.com/openai/codex/blob/ccf9be8cc458bd6b8336fe84d68d8893f2691a5e/codex-rs/core/src/unified_exec/process_manager.rs#L1483-L1500) inserts `context.session.thread_id` after constructing the configured environment. An [upstream integration test](https://github.com/openai/codex/blob/ccf9be8cc458bd6b8336fe84d68d8893f2691a5e/codex-rs/core/tests/suite/unified_exec.rs#L445-L516) runs native `exec_command` and compares the shell values to the independently observed configured thread ID. This is source evidence; we did not run that upstream suite.
- **Not a universal or authenticated ID.** This does not guarantee an ID in an ordinary terminal, arbitrary MCP tool, remote workspace on another device, or modified child environment. An executable wrapper or shell can overwrite it, and a valid-looking UUID alone is not proof of origin. In [upstream issue #15527](https://github.com/openai/codex/issues/15527), `0.116.0` reportedly leaked a parent ID into a nested launch. Do not infer a safe minimum version or universal Desktop/client compatibility from the current implementation. Pair only from the trusted, directly launched tool.

## Checks and interpretation

Build the **project’s** binaries, then run the default test on POSIX. It creates its own loopback board, scoped throwaway invite, private wake state and two fixture IDs. It never looks up or launches installed Codex. The real `neoboard` CLI receives only the request handle: absent and malformed shell IDs leave it pending and unbound; the expected ID binds once; status remains Off and no diagnostic is sent. The fixture supplies `CODEX_THREAD_ID` itself, so a pass **is not** runtime injection evidence.

```sh
cargo build --locked --bin neoboard --bin neochatd
python3 -B tests/codex_pair.py
```

Observed on Darwin arm64: the native fixture run passed (target matched, malformed/absent ID rejected, Off, replay rejected). Separately exercised the provider’s decoy, single namespaced tool call and receipt over loopback. No supported disposable Linux runner was found on this host. The real-mode Darwin guard refused execution; no macOS Codex executable was downloaded, probed or run.

**Opt-in real-runtime acceptance (not run on the verification Mac):** use an approved disposable Linux VM with an unprivileged test account, working Linux sandbox/user namespaces, no personal profiles, credentials, mounted host directories or agent sockets. Stage this checkout, built board binaries, Python 3.9+, bash and a verified official **Linux** Codex `0.171.0` installation including its normal sandbox resources. Use your trusted image or installation process, not a copied macOS binary. Detach all NICs; leave `lo` up. Do not push to CI, use a live account, or disable sandboxing if setup fails.

```sh
python3 -B tests/codex_pair.py --offline-linux-codex /absolute/installed/path/to/codex
```

The real mode refuses non-Linux, root, non-ELF entrypoints and non-loopback interfaces before starting Codex; guest provenance and absence of host mounts are operator preconditions. It constructs the environment from scratch and uses a temporary `CODEX_HOME`. The only model provider is a deterministic loopback Responses fixture; no provider token is used. The workspace-write sandbox permits networking **inside this loopback-only guest** for the local inbox check. A shell policy or guest that denies required local access is a failed/inconclusive acceptance, not a reason to bypass it.

The target receives an ordinary user message with the shell command as its first turn. Its provider waits while a newer decoy finishes in the same workspace/profile. Then it requests exactly one native `exec_command` of the precomputed command. No code sets the real target’s `CODEX_THREAD_ID`; neither ID is passed into pairing. Success requires the command’s completed exit-zero event, the stored binding and Off status matching the target’s independent `thread.started` event (not the decoy), and single use. Keep the final JSON receipt with the runtime version, then destroy the guest. This tests a text turn through `codex exec`, not an actual GUI clipboard, a model’s willingness to follow an instruction, or later wake delivery. CLI pairing only probes `codex --version`; that probe alone does not validate thread identity or resumability.