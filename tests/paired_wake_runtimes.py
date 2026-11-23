"""Private runtime observers for the native wake drill (POSIX). Never discover sessions."""
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import secrets
import socket
import subprocess as sp
import sys
from threading import Thread
import uuid

from roadmap27_provider import provider


def ids():
    return [str(uuid.uuid4()) for _ in range(2)]


def executable(path, source):
    path.write_text('#!' + sys.executable + '\n' + source)
    path.chmod(0o700)
    return path


class CliFixture:
    """Proves explicit resume argv/RPC and destination, not installed runtime behavior."""
    def __init__(self, root, name):
        self.mode = 'protocol fixture'
        self.target, self.decoy = ids()
        self.workspace = root / 'workspace'
        for sid in (self.target, self.decoy):
            (self.workspace / sid).touch()
        self.binary = executable(root / name, Path(__file__).with_name('paired_wake_stub.py').read_text())
        self.pair_args = ['--workspace', self.workspace, '--executable', self.binary]

    def history(self, sid):
        return [json.loads(row) for row in (self.workspace / sid).read_text().splitlines()]


@contextmanager
def http_fixture(handler):
    with ThreadingHTTPServer(('127.0.0.1', 0), handler) as server:
        worker = Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            yield f'http://127.0.0.1:{server.server_port}'
        finally:
            server.shutdown()
            worker.join()


def claude_provider(key):
    class Messages(BaseHTTPRequestHandler):
        def do_POST(self):
            if self.headers.get('x-api-key') != key or self.path.split('?')[0] != '/v1/messages':
                return self.send_error(404)
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            text = 'Offline drill receipt.'
            msg = dict(id='msg_fixture', type='message', role='assistant', model=body['model'],
                       content=[], stop_reason=None, stop_sequence=None, usage=dict(input_tokens=1, output_tokens=1))
            if body.get('stream'):
                frames = [('message_start', dict(message=msg)), ('content_block_start', dict(index=0,
                           content_block=dict(type='text', text=''))), ('content_block_delta', dict(index=0,
                           delta=dict(type='text_delta', text=text))), ('content_block_stop', dict(index=0)),
                          ('message_delta', dict(delta=dict(stop_reason='end_turn', stop_sequence=None),
                           usage=dict(output_tokens=1))), ('message_stop', {})]
                data = ''.join(f'event: {event}\ndata: {json.dumps(dict(type=event, **fields))}\n\n'
                               for event, fields in frames).encode()
                mime = 'text/event-stream'
            else:
                data = json.dumps(dict(msg, content=[dict(type='text', text=text)], stop_reason='end_turn')).encode()
                mime = 'application/json'
            self.send_response(200)
            self.send_header('Content-Type', mime)
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, *args):
            pass
    return http_fixture(Messages)


class Claude:
    def __init__(self, root, env, installed, stack):
        self.target, self.decoy = ids()
        self.config = root / 'claude-config'
        key = 'offline-' + secrets.token_hex(12)
        origin = stack.enter_context(claude_provider(key))
        # The wake worker need not inherit provider env. An exec-only launcher pins
        # the installed binary, minimal flags and this run's complete environment.
        child = dict(env, CLAUDE_CONFIG_DIR=str(self.config), ANTHROPIC_API_KEY=key,
                     ANTHROPIC_BASE_URL=origin, CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1',
                     DISABLE_AUTOUPDATER='1', DISABLE_TELEMETRY='1', DISABLE_ERROR_REPORTING='1')
        self.mode = sp.check_output([installed, '--version'], env=child, cwd=root, text=True, timeout=8).strip()
        flags = [str(installed), '--bare', '--strict-mcp-config', '--tools', '', '--model', 'sonnet']
        self.binary = executable(root / 'claude', f'import os, sys\nos.execve({str(installed)!r}, {flags!r} + sys.argv[1:], {child!r})\n')
        self.pair_args = ['--executable', self.binary, '--workspace', root / 'workspace']
        for sid in (self.target, self.decoy):
            raw = sp.check_output([self.binary, '--session-id', sid, '-p', 'Offline setup.', '--output-format', 'json'],
                                  env=env, cwd=root/'workspace', text=True, timeout=40)
            result = json.loads(raw)
            assert (result['session_id'], result['result']) == (sid, 'Offline drill receipt.')

    def history(self, sid):
        [file] = (self.config/'projects').glob('*/' + sid + '.jsonl')
        users = [m for line in file.read_text().splitlines() if (m := json.loads(line)).get('type') == 'user']
        assert all(m['sessionId'] == sid for m in users)
        return [m['message']['content'] for m in users]


class Neoism:
    def __init__(self, root, env, installed, stack, start, client, eventually):
        token = secrets.token_urlsafe()
        child = dict(env, NEOISM_CONFIG_HOME=str(root/'config/neoism'), NEOISM_AGENT_TOKEN=token,
                     NEOISM_AGENT_DISABLE_MODELS_FETCH='true', NEOISM_AGENT_DISABLE_EMBEDDINGS='1',
                     NEOISM_AGENT_PROVIDER='fixture', NEOISM_AGENT_MODEL='drill')
        mock = stack.enter_context(provider(lambda _: {'content': 'Offline drill receipt.'}))
        fixture = dict(npm='@ai-sdk/openai-compatible', auth='none', options=dict(baseURL=f'http://127.0.0.1:{mock.server_port}/v1'),
                       models=dict(drill=dict(name='Drill', toolCall=True, limit=dict(context=65536, output=8192))))
        settings = root/'config/agent'
        settings.mkdir(parents=True)
        (settings/'agent.json').write_text(json.dumps(dict(provider=dict(fixture=fixture), enabledProviders=['fixture'], model='fixture/drill')))
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        origin = f'http://127.0.0.1:{port}'
        self.mode = sp.check_output([installed, '--version'], env=child, cwd=root, text=True, timeout=8).strip()
        start([installed, 'serve', '--hostname', '127.0.0.1', '--port', str(port)], child)
        self.api = client(origin, token)
        self.eventually = eventually
        eventually(lambda: (self.api('/v2/sessions/status'), True))
        self.target, self.decoy = [self.api('/v2/sessions', dict(model=dict(providerId='fixture', id='drill')))['id'] for _ in range(2)]
        self.pair_args = ['--origin', origin]
        self.token = token

    def history(self, sid):
        # Do not let a partial/unanswered turn masquerade as an empty decoy or
        # an unchanged target. Only compare complete user/assistant histories.
        def settled():
            msgs = self.api('/v2/sessions/' + sid + '/messages?order=asc')['items']
            assert all(m['info']['sessionId'] == sid and not m['info'].get('error') for m in msgs)
            if len(msgs) % 2 or any(not a['info']['time'].get('completed') for a in msgs[1::2]):
                return None
            result = []
            for user, assistant in zip(msgs[::2], msgs[1::2]):
                assert user['info']['role'] == 'user' and assistant['info']['role'] == 'assistant'
                assert any(p.get('text') == 'Offline drill receipt.' for p in assistant['parts'])
                result.append('\n'.join(p['text'] for p in user['parts'] if p['type'] == 'text'))
            return (result,)
        return self.eventually(settled)[0]


class NeoismFixture:
    def __init__(self, stack):
        self.mode = 'authenticated HTTP fixture'
        self.token = secrets.token_urlsafe()
        self.target, self.decoy = ['ses_' + s for s in ids()]
        self.logs = {s: [] for s in (self.target, self.decoy)}
        runtime = self

        class Handler(BaseHTTPRequestHandler):
            def process(self, turn):
                if self.headers.get('Authorization') != 'Bearer ' + runtime.token:
                    return self.send_error(403)
                for sid, entries in runtime.logs.items():
                    if self.path == '/v2/sessions/' + sid + ('/prompt-if-idle' if turn else ''):
                        if turn:
                            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                            entries.append(body['prompt'])
                        self.send_response(204 if turn else 200)
                        self.end_headers()
                        if not turn:
                            self.wfile.write(json.dumps(dict(id=sid)).encode())
                        return
                self.send_error(404)

            def do_GET(self):
                self.process(False)

            def do_POST(self):
                self.process(True)

            def log_message(self, *args):
                pass
        self.pair_args = ['--origin', stack.enter_context(http_fixture(Handler))]

    def history(self, sid):
        return list(self.logs[sid])