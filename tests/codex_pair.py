"""Codex pairing boundary check; real runtime is opt-in and offline Linux only."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import socket
import subprocess
import sys
from tempfile import TemporaryDirectory
from uuid import UUID

from paired_wake_drill import Board, KEY, PROJECT, ROOT, environment
from paired_wake_runtimes import CliFixture, http_fixture
from codex_pair_responses import responses


def real_preflight(binary):
    if sys.platform != 'linux' or os.geteuid() == 0:
        sys.exit('Real mode requires a non-root user in a disposable Linux VM.')
    if {name for _, name in socket.if_nameindex()} != {'lo'}:
        sys.exit('Refusing real mode: only the loopback interface may be present.')
    if not binary.is_absolute() or binary.name != 'codex' or not os.access(binary, os.X_OK):
        sys.exit('Supply the absolute installed native Linux codex executable.')
    with binary.open('rb') as f:
        if f.read(4) != b'\x7fELF':
            sys.exit('Refusing wrapper/non-Linux executable.')


def run(argv, cwd, env, *, ok=True):
    p = subprocess.run(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                       capture_output=True, text=True, timeout=120)
    # Never dump argv, program output, pending request, or board credentials.
    if (p.returncode == 0) != ok:
        raise AssertionError(f'{Path(argv[0]).name}: unexpected exit {p.returncode}')
    return (p.stdout if ok else p.stdout + p.stderr).strip()


def real_pair(binary, env, cwd, prompt, cmd, stack):
    handler = responses(prompt, cmd)
    url = stack.enter_context(http_fixture(handler))
    home = Path(env['CODEX_HOME'])
    # Workspace-write sandbox, loopback-only VM. No escalation, MCP or login.
    (home/'config.toml').write_text(f'''model = "gpt-5.4"
model_provider = "drill"
sandbox_mode = "workspace-write"
approval_policy = "never"
[features]
code_mode = false
[sandbox_workspace_write]
network_access = true
[model_providers.drill]
name = "Loopback fixture"
base_url = "{url}/v1"
wire_api = "responses"
supports_websockets = false
requires_openai_auth = false
[shell_environment_policy.set]
PATH = {json.dumps(env['PATH'])}
''')
    version = run([binary, '--version'], cwd, env)
    assert version == 'codex-cli 0.171.0', 'Real acceptance is pinned to 0.171.0'

    def turn(text):
        args = [binary, 'exec', '--skip-git-repo-check', '--json']
        events = [json.loads(s) for s in run([*args, text], cwd, env).splitlines()]
        [sid] = [e['thread_id'] for e in events if e['type'] == 'thread.started']
        assert str(UUID(sid)) == sid and any(e['type'] == 'turn.completed' for e in events)
        commands = [e['item'] for e in events if e['type'] == 'item.completed'
                    and e['item']['type'] == 'command_execution']
        return sid, commands

    # The target's first ordinary user message is the pairing prompt. While its
    # provider is waiting, finish a newer unrelated chat in exactly the same cwd.
    with ThreadPoolExecutor(max_workers=1) as pool:
        waiting = pool.submit(turn, prompt)
        try:
            assert handler.ready.wait(40), 'Codex did not request the exact user prompt from the loopback provider'
            decoy, actions = turn('Initialize the disposable decoy.')
            assert not actions
        finally:
            handler.proceed.set()
        target, actions = waiting.result()
    assert target != decoy and handler.calls == 1 and handler.receipts == 1
    [action] = actions
    assert action['exit_code'] == 0 and 'Paired' in action['aggregated_output']
    return target, decoy, version


def verify(root, bins, binary, stack):
    env = environment(root)  # constructed, not a copy of the host environment
    cwd = root/'workspace'
    env.update(CODEX_HOME=str(root/'codex-home'), NEOBOARD_WAKE_HOME=str(cwd/'wake'))
    Path(env['CODEX_HOME']).mkdir(mode=0o700)
    board = Board(root, bins/'neochatd', env, stack)
    fixture = None if binary else CliFixture(root, 'codex')
    codex = binary or fixture.binary
    env['PATH'] = str(codex.parent) + os.pathsep + os.defpath
    cli = bins/'neoboard'
    scope = ['--board-id', KEY, '--harness', 'codex']
    prepared = run([cli, 'wake', 'prepare', *scope, '--project', PROJECT, '--invite', board.invite], cwd, env)
    program, verb, action, opt, code = shlex.split(prepared)
    assert Path(program).samefile(cli) and (verb, action, opt) == ('wake', 'pair', '--code')
    assert re.fullmatch('[0-9a-f]{32}', code)
    argv = [program, 'wake', 'pair', '--request', hashlib.sha256(code.encode()).hexdigest()]
    cmd = f'NEOBOARD_WAKE_HOME={shlex.quote(env["NEOBOARD_WAKE_HOME"])} {shlex.join(argv)}'
    prompt = 'Pair this exact Codex conversation with NeoBoard. Run only this command in your local shell tool; leave wakes off.\n\n' + cmd
    assert all(s not in prompt for s in (code, board.key, board.writer_key, board.admin_key))

    def state():
        return json.loads((cwd/'wake/bindings.json').read_text())

    before = state()
    for extra in ({}, {'CODEX_THREAD_ID': 'not-a-thread-uuid'}):
        run(argv, cwd, {**env, **extra}, ok=False)
        assert state() == before and code in before['pending'] and not before['bindings']

    if binary:
        target, decoy, runtime = real_pair(binary, env, cwd, prompt, cmd, stack)
    else:
        # Test double only at the shell metadata boundary. No installed Codex is discovered.
        target, decoy, runtime = fixture.target, fixture.decoy, 'injected fixture (not Codex)'
        assert 'Paired' in run(['/bin/sh', '-c', cmd], cwd, {**env, 'CODEX_THREAD_ID': target})
        assert not fixture.history(target) and not fixture.history(decoy)

    after = state()
    [key] = after['bindings']
    binding = after['bindings'][key]
    assert key == KEY+':codex' and binding['target']['session'] == target != decoy
    assert binding['target']['workspace'] == str(cwd) and Path(binding['target']['executable']).samefile(codex)
    assert not after['pending'] and binding['enabled'] is False and binding['test'] is None
    assert binding['last'] == 'Paired; no turn sent'
    assert run([cli, 'wake', 'status', *scope], cwd, env).startswith(f'Off | @reviewer | {target} |')
    assert 'request not found' in run(argv, cwd, env, ok=False).lower()
    assert state() == after
    print(json.dumps(dict(runtime=runtime, exact_target=True, malformed_id_rejected=True, wake='off', replay='rejected')))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--board-bin-dir', type=Path, default=ROOT/'target/debug')
    parser.add_argument('--offline-linux-codex', type=Path, help='opt in; install pinned Codex in a disconnected, disposable guest first')
    args = parser.parse_args()
    if args.offline_linux_codex:
        real_preflight(args.offline_linux_codex)
    with TemporaryDirectory(prefix='codex-pair-') as tmp, ExitStack() as stack:
        verify(Path(tmp).resolve(), args.board_bin_dir.resolve(), args.offline_linux_codex, stack)


if __name__ == '__main__':
    if not __debug__ or os.name != 'posix':
        sys.exit('Requires POSIX and enabled assertions.')
    main()