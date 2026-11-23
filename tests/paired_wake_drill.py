"""Black-box native wake test. No host board, runtime profiles, login items or paid provider."""
import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import re
import secrets
import shlex
import shutil
import signal
import subprocess as sp
import sys
from tempfile import TemporaryDirectory
import time
from urllib import request

from paired_wake_runtimes import Claude, CliFixture, Neoism, NeoismFixture
from roadmap27_drill import ROOT, client, eventually


NAMES = ('neoism', 'claude', 'codex')
PROJECT = 'drill'
KEY = 'disposable-board'


def paths(overrides):
    candidates = dict(neoism=[shutil.which('neoism-agent'), '/Applications/Neoism.app/Contents/MacOS/neoism-agent'],
                      claude=[shutil.which('claude')], codex=[shutil.which('codex'), '/Applications/Codex.app/Contents/Resources/codex'])
    for h in ('neoism', 'claude'):
        if getattr(overrides, h):
            candidates[h] = [getattr(overrides, h)]
    return {h: next((Path(p).absolute() for p in choices if p and os.access(p, os.X_OK)), None)
            for h, choices in candidates.items()}


def environment(root):
    for name in ('home', 'tmp', 'workspace'):
        (root/name).mkdir(mode=0o700)
    # Construct, don't filter, the environment. In particular no host board, agent
    # socket, keychain helper, provider, inherited session ID or proxy configuration.
    return dict(HOME=str(root/'home'), PATH=os.defpath, TMPDIR=str(root/'tmp'), XDG_DATA_HOME=str(root/'data'),
                XDG_CONFIG_HOME=str(root/'config'), XDG_CACHE_HOME=str(root/'cache'), XDG_STATE_HOME=str(root/'state'),
                NEOBOARD_WAKE_HOME=str(root/'wake'), NEOBOARD_WAKE_MANUAL='1', LANG='en_US.UTF-8', RUST_LOG='error',
                HTTP_PROXY='http://127.0.0.1:1', HTTPS_PROXY='http://127.0.0.1:1', NO_PROXY='127.0.0.1,localhost')


def start(stack, cwd, argv, env):
    proc = sp.Popen(argv, cwd=cwd, env=env, stdin=sp.DEVNULL, stdout=sp.DEVNULL, start_new_session=True)

    def stop():
        try:
            os.killpg(proc.pid, signal.SIGTERM)
            proc.wait(timeout=5)
        except ProcessLookupError:
            proc.wait()
        except sp.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()
    stack.callback(stop)


class Board:
    def __init__(self, root, daemon, env, stack):
        start(stack, root, [daemon, '--bind', '127.0.0.1:0', '--data-dir', root/'board'], env)
        url = eventually(lambda: json.loads((root/'board/endpoint.json').read_text()))['url']
        assert re.fullmatch(r'http://127\.0\.0\.1:\d+', url)
        self.admin_key = (root/'board/admin-token').read_text().strip()
        admin = client(url, self.admin_key)
        expires = int(time.time()) + 3600

        def issue(actor, *grants):
            return admin('/v1/admin/restricted-invites', dict(invite=dict(actor=actor, channels=[PROJECT+'/*'],
                         expires_at=expires), grants=grants))['token']

        self.key = issue('reviewer', 'read', 'notifications')
        self.writer_key = issue('writer', 'read', 'post', 'post_task', 'claim', 'start', 'complete')
        self.reviewer = client(url, self.key)
        self.writer = client(url, self.writer_key)
        self.url = url
        self.invite = root/'reviewer.neochat'
        with os.fdopen(os.open(self.invite, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as f:
            json.dump(dict(version=1, project=PROJECT, endpoint=url, token=self.key, expires_at=expires), f)
        self.channel = PROJECT+'/general'
        self.untrusted = secrets.token_hex(12)

    def mention(self):
        return self.writer('/v1/actions', dict(type='post', channel=self.channel, text='@reviewer '+self.untrusted))['id']

    def complete(self):
        task = self.writer('/v1/actions', dict(type='post_task', channel=self.channel, text='task '+self.untrusted))
        tid = task['id']
        self.reviewer('/v1/subscriptions', dict(thread=tid, kind='completed', after_cursor=task['cursor']))
        for action in (dict(type='claim', lease_secs=60), dict(type='start'), dict(type='complete', text='result '+self.untrusted)):
            self.writer('/v1/actions', dict(action, id=tid))
        return tid

    def read_flag(self, item, flag):
        req = request.Request(f"{self.url}/v1/inbox/{item['id']}/read", json.dumps(dict(read=flag)).encode(),
                              {'Content-Type': 'application/json', 'Authorization': 'Bearer '+self.key})
        with request.build_opener(request.ProxyHandler({})).open(req, timeout=5) as r:
            assert r.status == 204

    def unread(self):
        page = self.reviewer('/v1/inbox')
        assert page['actor'] == 'reviewer'
        return page['items']


def verify(root, stack, bins, h, installed):
    env = environment(root)
    board = Board(root, bins/'neochatd', env, stack)
    workspace = root/'workspace'
    if h == 'neoism' and installed:
        rt = Neoism(root, env, installed, stack, lambda a,e: start(stack, workspace, a,e), client, eventually)
    elif h == 'claude' and installed:
        rt = Claude(root, env, installed, stack)
    elif h == 'neoism':
        rt = NeoismFixture(stack)
    else:
        rt = CliFixture(root, h)

    def wake(*args, extra=None, success=True):
        # Every invocation is a fresh OS process. Never eval the printed command.
        result = sp.run([bins/'neoboard', 'wake', *args], cwd=workspace, env=dict(env, **(extra or {})),
                        stdin=sp.DEVNULL, capture_output=True, text=True, timeout=90)
        text = result.stdout + result.stderr
        for secret in (board.admin_key, board.writer_key, board.key, getattr(rt, 'token', None)):
            assert not secret or secret not in text, 'credential printed by native CLI'
        if (result.returncode == 0) != success:
            # Do not emit a reusable pairing or diagnostic code in CI logs.
            raise AssertionError(f'wake {args[0]}: exit {result.returncode}; ' + re.sub(r'[0-9a-zA-Z_-]{30,}', '[redacted]', text))
        return result.stdout.strip()

    scope = ['--board-id', KEY, '--harness', h]
    def control(verb, **opts):
        return wake(verb, *scope, **opts)

    def once():
        wake('run', '--once')

    def history():
        return rt.history(rt.target)

    def added(previous):
        turns = eventually(lambda: (msgs if len(msgs := history()) > len(previous) else None))
        assert turns[:-1] == previous and rt.history(rt.decoy) == decoy
        return turns

    baseline, decoy = history(), rt.history(rt.decoy)
    command = shlex.split(wake('prepare', *scope, '--project', PROJECT, '--invite', board.invite))
    assert len(command) == 5 and Path(command[0]).name == 'neoboard' and command[1:4] == ['wake', 'pair', '--code']
    code = command[-1]
    assert re.fullmatch(r'[A-Za-z0-9_-]+', code)
    pair = ['pair', '--code', code, '--session', rt.target]
    if h != 'claude':
        import hashlib
        pair[1:3] = ['--request', hashlib.sha256(code.encode()).hexdigest()]
    if h != 'neoism':
        wake(*pair, '--workspace', workspace, '--executable', root/'absent', success=False)
    else:
        wake(*pair, *rt.pair_args, success=False)  # missing runtime bearer
    extra = dict(NEOISM_WAKE_TOKEN=rt.token) if h == 'neoism' else {}
    reported_id = {'neoism':'NEOISM_SESSION_ID','claude':'CLAUDE_CODE_SESSION_ID','codex':'CODEX_THREAD_ID'}[h]
    wake(*pair, *rt.pair_args, extra={**extra, reported_id: rt.decoy}, success=False)
    wake(*pair, *rt.pair_args, extra=extra)
    wake('pair', '--code', code, '--session', rt.decoy, *rt.pair_args, extra=extra, success=False)
    status = control('status')
    assert status.startswith('Off') and rt.target in status and 'reviewer' in status, status
    once()
    assert history() == baseline

    # No MCP/tool execution by these offline responders. A diagnostic turn is
    # observable at the target, but must NOT be reported as agent-acknowledged.
    diagnostic = control('test')
    after_test = added(baseline)
    assert 'ack' in after_test[-1].lower() and 'awaiting' in diagnostic.lower(), diagnostic
    assert control('status').startswith('Off')

    control('on')
    assert control('status').startswith('On')
    mid = board.mention()
    [mention] = board.unread()
    assert (mention['kind'], mention['message_id']) == ('mention', mid)
    once()
    after_mention = added(after_test)

    control('off')
    tid = board.complete()
    queued = board.unread()
    assert [x['kind'] for x in queued] == ['mention', 'completed'] and queued[1]['thread'] == tid
    once()
    assert history() == after_mention and board.unread() == queued
    control('on')
    once()
    after_completed = added(after_mention)
    for item, prompt in ((mention, after_mention[-1]), (queued[1], after_completed[-1])):
        assert item['kind'] in prompt and f"thread #{item['thread']}" in prompt and f"message #{item['message_id']}" in prompt
        assert board.untrusted not in prompt and board.key not in prompt

    # Re-expose the *same IDs*, not an empty inbox, across fresh worker processes.
    for item in queued:
        board.read_flag(item, True)
    assert board.unread() == []
    for item in queued:
        board.read_flag(item, False)
    assert board.unread() == queued
    once()
    once()
    assert history() == after_completed and rt.history(rt.decoy) == decoy
    final = control('status')
    assert 'accepted' in final.lower() and 'uncertain' not in final.lower(), final
    control('off')
    if h != 'neoism':
        # This is only our fixture/launcher, never the installed executable.
        saved = rt.binary.with_suffix('.saved')
        rt.binary.rename(saved)
        control('on', success=False)
        assert control('status').startswith('Off')
        once()
        assert history() == after_completed
        saved.rename(rt.binary)
        control('on')
        failed_mid = board.mention()
        rt.binary.rename(saved)
        once()
        assert 'uncertain' in control('status').lower()
        saved.rename(rt.binary)
        once()  # restoring the binary must not resend an uncertain admission
        assert board.unread()[-1]['message_id'] == failed_mid
        assert history() == after_completed and rt.history(rt.decoy) == decoy
        control('off')
    return dict(harness=h, runtime=rt.mode, diagnostic='delivered, deliberately unacknowledged',
                mentions=1, completed=1, restarted_cycles=2, replay_turns=0, decoy_turns=0,
                missing_executable='failed closed; no resend after restore' if h != 'neoism' else 'not applicable')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--board-bin-dir', type=Path, default=ROOT/'target/debug')
    p.add_argument('--harness', choices=(*NAMES, 'all'), default='all')
    p.add_argument('--neoism-agent', dest='neoism', type=Path)
    p.add_argument('--claude', type=Path)
    p.add_argument('--fixtures', action='store_true', help='use protocol fixtures even if real runtimes are available')
    p.add_argument('--probe', action='store_true', help='report binary versions and env-name presence only, no session lookup')
    args = p.parse_args()
    installed = paths(args)
    for h in NAMES:
        if args.harness not in ('all', h):
            continue
        with TemporaryDirectory(prefix='paired-wake-') as tmp, ExitStack() as stack:
            root = Path(tmp).resolve()
            binary = installed[h]
            if args.probe:
                version = sp.check_output([binary, '--version'], cwd=root, env=environment(root), text=True, timeout=10).strip() if binary else None
                names = dict(neoism=['NEOISM_SESSION_ID'], claude=['CLAUDE_CODE_SESSION_ID', 'CLAUDE_SESSION_ID'], codex=['CODEX_THREAD_ID'])[h]
                print(json.dumps(dict(harness=h, binary=str(binary) if binary else None, version=version,
                                      session_env_present={k: bool(os.environ.get(k)) for k in names})))
            else:
                if getattr(args, h, None) and not binary:
                    sys.exit(f'Explicit {h} executable not found')
                print(json.dumps(verify(root, stack, args.board_bin_dir.resolve(), h, None if args.fixtures else binary)), flush=True)


if __name__ == '__main__':
    if not __debug__ or os.name != 'posix':
        sys.exit('Run on POSIX with Python assertions enabled')
    main()