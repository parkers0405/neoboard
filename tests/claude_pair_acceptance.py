"""Pair from an ordinary user message in installed Claude Code. POSIX; offline provider."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack
from hashlib import sha256
from http.server import BaseHTTPRequestHandler
import json
import os
from pathlib import Path
import re
import secrets
import shlex
import shutil
import subprocess as sp
import tempfile
from threading import Event

from paired_wake_drill import Board, environment, KEY, PROJECT, ROOT
from paired_wake_runtimes import http_fixture


class Messages:
    """Only the model is simulated. Ask for one exact command; inspect its real result."""
    def __init__(self):
        self.key = 'offline-' + secrets.token_hex(16)
        self.prompt = self.command = None
        self.results = []
        self.calls = []
        self.pause = False
        self.arrived, self.release = Event(), Event()

    def respond(self, req):
        # Claude may append a system-role reminder after the actual user input.
        last = next(m['content'] for m in reversed(req['messages']) if m['role'] == 'user')
        if isinstance(last, list):
            for block in last:
                if block.get('type') == 'tool_result' and block.get('tool_use_id') == self.tool_id:
                    self.results.append(block)
                    return dict(type='text', text='Received Bash result.')
        texts = [last] if isinstance(last, str) else [b.get('text', '') for b in last]
        match = self.prompt and any(self.prompt in text for text in texts)
        bash = any(t['name'] == 'Bash' for t in req.get('tools', []))
        self.calls.append((bool(match), bash))
        if match and bash:
            if self.pause:
                self.arrived.set()
                if not self.release.wait(timeout=55):
                    return dict(type='text', text='Timed out waiting for decoy.')
            return dict(type='tool_use', id=self.tool_id, name='Bash', input=dict(command=self.command))
        return dict(type='text', text='Offline receipt.')

    def server(self):
        model = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                if self.path.split('?')[0] != '/v1/messages' or self.headers.get('x-api-key') != model.key:
                    return self.send_error(404)
                req = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                block = model.respond(req)
                tool = block['type'] == 'tool_use'
                reason = 'tool_use' if tool else 'end_turn'
                usage = dict(input_tokens=1, output_tokens=1)
                msg = dict(type='message', id='msg_pair', role='assistant', model=req['model'],
                           content=[], stop_reason=None, stop_sequence=None, usage=usage)
                if req.get('stream'):
                    start = dict(block, input={}) if tool else dict(type='text', text='')
                    delta = (dict(type='input_json_delta', partial_json=json.dumps(block['input'])) if tool
                             else dict(type='text_delta', text=block['text']))
                    frames = [('message_start', dict(message=msg)), ('content_block_start', dict(index=0, content_block=start)),
                              ('content_block_delta', dict(index=0, delta=delta)), ('content_block_stop', dict(index=0)),
                              ('message_delta', dict(delta=dict(stop_reason=reason, stop_sequence=None), usage=usage)),
                              ('message_stop', {})]
                    data = ''.join(f'event: {ev}\ndata: {json.dumps(dict(type=ev, **fields))}\n\n' for ev, fields in frames)
                    mime = 'text/event-stream'
                else:
                    data = json.dumps(dict(msg, content=[block], stop_reason=reason))
                    mime = 'application/json'
                data = data.encode()
                self.send_response(200)
                self.send_header('Content-Type', mime)
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args):
                pass

        return http_fixture(Handler)


def verify(root, bins, installed, stack):
    env = environment(root)
    workspace = root / 'workspace'
    board = Board(root, bins/'neochatd', env, stack)
    model = Messages()
    env.update(ANTHROPIC_BASE_URL=stack.enter_context(model.server()), ANTHROPIC_API_KEY=model.key,
               SHELL='/bin/bash', CLAUDE_CONFIG_DIR=str(root/'claude-config'),
               CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1', DISABLE_AUTOUPDATER='1',
               BASH_ENV='/dev/null', DISABLE_TELEMETRY='1', DISABLE_ERROR_REPORTING='1')
    (root/'bin').mkdir()
    (root/'bin/claude').symlink_to(installed)
    env['PATH'] = str(root/'bin') + os.pathsep + env['PATH']

    def run(*argv):
        return sp.check_output(argv, cwd=workspace, env=env, stdin=sp.DEVNULL, timeout=60, text=True).strip()

    def chat(prompt, *options):
        return json.loads(run(installed, '--bare', '--strict-mcp-config', '--setting-sources', '',
                              '--model', 'sonnet', '--tools', 'Bash', '--permission-mode', 'dontAsk',
                              '--output-format', 'json', *options, '--print', prompt))

    def wake(*args):
        return run(bins/'neoboard', 'wake', *args)

    scope = ['--board-id', KEY, '--harness', 'claude']
    prepared = shlex.split(wake('prepare', *scope, '--project', PROJECT, '--invite', board.invite))
    assert prepared[:-1] == [str(bins/'neoboard'), 'wake', 'pair', '--code']
    assert re.fullmatch('[a-f0-9]{32}', prepared[-1])
    request = sha256(prepared[-1].encode()).hexdigest()
    def bash_turn(prefix, command):
        model.tool_id = 'toolu_' + secrets.token_hex(8)
        model.command = command
        model.prompt = prefix + command
        model.results.clear()
        model.calls.clear()
        result = chat(model.prompt, '--allowedTools', f'Bash({command})')
        assert len(model.results) == 1, f'Bash results={len(model.results)}; provider (prompt matched, Bash available)={model.calls}'
        block = model.results[0]
        assert not block.get('is_error'), 'Claude reported an unsuccessful Bash invocation'
        return result['session_id'], str(block['content'])

    # Independently observe the variable in Claude's Bash, without providing an ID to it.
    observed, reported = bash_turn('Check the fixture conversation context: ', 'printenv CLAUDE_CODE_SESSION_ID')
    assert observed in reported
    # The same request from an ordinary terminal with no conversation context must fail and remain pending.
    args = [str(bins/'neoboard'), 'wake', 'pair', '--request', request]
    missing = sp.run(args, cwd=workspace, env=env, capture_output=True, timeout=30)
    assert missing.returncode != 0 and wake('status', *scope) == 'Not paired'
    # Keep a fresh chat open on its first ordinary prompt. Start a newer chat in the same profile and
    # workspace before releasing the tool call. The provider and driver don't yet know the target UUID.
    model.pause = True
    with ThreadPoolExecutor(1) as pool:
        pair = pool.submit(bash_turn, 'Pair this conversation. Run exactly: ', shlex.join(args))
        try:
            assert model.arrived.wait(timeout=25), 'No pairing prompt at provider'
            decoy = chat('Start a different disposable chat.')['session_id']
        finally:
            model.release.set()
        target, receipt = pair.result()
    assert 'Paired.' in receipt, receipt.replace(request, '[request]')[:450]
    status = wake('status', *scope)
    assert len({target, decoy, observed}) == 3
    assert status.startswith('Off') and target in status and decoy not in status, 'binding is not the current chat, wake Off'
    print(run(installed, '--version'), 'PASS: CLAUDE_CODE_SESSION_ID matched; no-context terminal refused; '
          'Bash paired its own chat, decoy excluded, wake Off.')


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--claude', type=Path, default=shutil.which('claude'))
    p.add_argument('--board-bin-dir', type=Path, default=ROOT/'target/debug')
    args = p.parse_args()
    if not __debug__ or os.name != 'posix' or not args.claude:
        p.error('Requires installed Claude, POSIX and Python assertions enabled')
    with tempfile.TemporaryDirectory(prefix='claude-pair-') as tmp, ExitStack() as stack:
        verify(Path(tmp).resolve(), args.board_bin_dir.resolve(), args.claude.resolve(), stack)