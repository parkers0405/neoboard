"""POSIX acceptance: real local Neoism, actual bash, private board, offline model. No service install."""
import argparse
from contextlib import ExitStack
from hashlib import sha256
import json
import os
from pathlib import Path
import re
import secrets
import shlex
import socket
import subprocess as sp
from tempfile import TemporaryDirectory
from urllib.error import HTTPError
from urllib import request as http

from paired_wake_drill import Board, KEY, PROJECT, environment, start
from roadmap27_drill import ROOT, client, eventually
from roadmap27_provider import provider


def verify(root, bins, neo, stack):
    env = environment(root)
    env.update(SHELL='/bin/sh', XDG_RUNTIME_DIR=str(root/'runtime'))
    Path(env['XDG_RUNTIME_DIR']).mkdir(mode=0o700)
    ws = root/'workspace'
    board = Board(root, bins/'neochatd', env, stack)
    scope = ['--board-id', KEY, '--harness', 'neoism']

    def wake(*args, ok=True):
        r = sp.run([bins/'neoboard', 'wake', *args], cwd=ws, env=env, capture_output=True, text=True, timeout=30)
        assert (r.returncode == 0) == ok, f'unexpected wake {args[0]} exit {r.returncode}'
        return r.stdout.strip()

    prepared = wake('prepare', *scope, '--project', PROJECT, '--invite', board.invite)
    exe, code = prepared.rsplit(' wake pair --code ', 1)
    assert re.fullmatch('[a-f0-9]{32}', code)
    request = sha256(code.encode()).hexdigest()
    cmd = f"NEOBOARD_WAKE_HOME='{env['NEOBOARD_WAKE_HOME']}' {exe} wake pair --request {request}"
    prose = ('Pair this exact Neoism conversation with my pending NeoBoard wake request on this device. '
             'Run the command below in this conversation’s local shell tool, without supplying a session ID. '
             'Never guess or choose the latest chat. If local execution, session identity, or required private runtime configuration '
             'is unavailable, stop and explain what is missing. Never request or print credentials, start another chat, '
             'send a test, or turn wake on. Report the result so I can check the connection in NeoBoard.')
    pasted = prose + '\n\n' + cmd
    wake('pair', '--request', request, ok=False)
    assert wake('status', *scope) == 'Not paired'

    master = secrets.token_urlsafe()
    forbidden = [master, code, board.key, board.writer_key, board.admin_key, 'neoism-wake-v1.']
    commands = {}
    receipts = {}

    def respond(body):
        assert all(value not in json.dumps(body) for value in forbidden), 'credential entered provider context'
        messages = body['messages']
        idx = max((i for i, m in enumerate(messages) if m['role'] == 'user'), default=0)
        # OpenAI-compatible messages may carry typed text parts.
        text = messages[idx]['content']
        if isinstance(text, list):
            text = '\n'.join(part.get('text', '') for part in text)
        if body.get('tools') and text in commands:
            results = [m for m in messages[idx+1:] if m['role'] == 'tool']
            if not results:
                assert 'bash' in {t['function']['name'] for t in body['tools']}
                args = dict(command=commands[text], description='Pair the current conversation')
                return dict(tool_calls=[dict(index=0, id='call_'+secrets.token_hex(5), type='function',
                                             function=dict(name='bash', arguments=json.dumps(args)))])
            receipts[text] = str(results[-1]['content'])
        return {'content':'Offline receipt.'}

    mock = stack.enter_context(provider(respond))
    child = dict(env, NEOISM_CONFIG_HOME=str(root/'config/neoism'), NEOISM_AGENT_TOKEN=master,
                 NEOISM_AGENT_DISABLE_MODELS_FETCH='true', NEOISM_AGENT_DISABLE_EMBEDDINGS='1',
                 NEOISM_AGENT_PROVIDER='fixture', NEOISM_AGENT_MODEL='drill')
    settings = root/'config/agent'
    settings.mkdir(parents=True)
    fixture = dict(npm='@ai-sdk/openai-compatible', auth='none', options=dict(baseURL=f'http://127.0.0.1:{mock.server_port}/v1'),
                   models=dict(drill=dict(name='Drill', toolCall=True, limit=dict(context=65536, output=8192))))
    (settings/'agent.json').write_text(json.dumps(dict(provider=dict(fixture=fixture), enabledProviders=['fixture'], model='fixture/drill')))
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    origin = f'http://127.0.0.1:{port}'
    start(stack, ws, [neo, 'serve', '--hostname', '127.0.0.1', '--port', str(port)], child)
    api = client(origin, master)
    eventually(lambda: api('/v2/health'))
    opts = dict(model=dict(providerId='fixture', id='drill'), permission=[dict(permission=p, pattern='*', action='allow') for p in ['bash','external_directory']])
    target, decoy = [api('/v2/sessions', opts)['id'] for _ in range(2)]  # newest is deliberately wrong
    base = '/v2/sessions/'

    def messages(sid):
        return api(base + sid + '/messages?order=asc')['items']

    def turn(sid, text, command):
        commands[text] = command
        req = http.Request(origin+base+sid+'/prompt-async', json.dumps(dict(prompt=text)).encode(),
                           {'Authorization':'Bearer '+master, 'Content-Type':'application/json'})
        with http.build_opener(http.ProxyHandler({})).open(req, timeout=10) as response:
            assert response.status == 204
        eventually(lambda: text in receipts or mock.errors)
        assert not mock.errors, 'offline model or shell did not complete the expected pairing turn'
        eventually(lambda: any(m['info']['role'] == 'assistant' and m['info']['time'].get('completed') and any(p.get('text') == 'Offline receipt.' for p in m['parts']) for m in messages(sid)))
        return receipts[text]

    wrong = cmd + ' --session ' + shlex.quote(target)
    # Session mismatch inside the newer, controlled decoy must fail before consuming the request.
    err = turn(decoy, 'Reject the conflicting fixture target.\n\n'+wrong, wrong)
    assert 'disagree' in err and wake('status', *scope) == 'Not paired', 'wrong target did not fail closed'
    decoy_history = messages(decoy)
    assert target not in pasted and decoy not in pasted and 'Paired.' in turn(target, pasted, cmd)
    assert wake('status', *scope).startswith('Off') and target in wake('status', *scope)
    assert not list((root/'tmp').glob('neoism-wake-*')), 'handoff was not retired'
    bound = json.loads((root/'wake/bindings.json').read_text())['bindings'][KEY+':neoism']
    assert bound['target']['session'] == target and bound['target']['origin'] == origin and bound['bearer'] != master
    scoped = client(origin, bound['bearer'])
    for path, payload in [(base+decoy, None), (base+decoy+'/prompt-if-idle', dict(prompt='wrong target')),
                          (base+target+'/messages', None), (base+target+'/prompt-if-idle', dict(prompt='wrong shape', system='denied'))]:
        try:
            scoped(path, payload)
        except HTTPError as e:
            assert e.code in (400, 401, 403)
        else:
            raise AssertionError('session credential over-authorized')

    previous = messages(target)
    board.mention()
    wake('run', '--once')
    assert messages(target) == previous and messages(decoy) == decoy_history
    wake('on', *scope)
    wake('run', '--once')
    eventually(lambda: any('mention' in str(m['parts']) for m in messages(target)[len(previous):] if m['info']['role']=='user'))
    wake('off', *scope)
    assert messages(decoy) == decoy_history and wake('status', *scope).startswith('Off') and not mock.errors
    version = sp.check_output([neo, '--version'], cwd=ws, env=env, text=True).strip()
    print(version, 'PASS: copied prompt -> bash -> exact session, scoped auth, decoy unchanged, Off until enabled.')


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--neoism-agent', type=Path, required=True)
    p.add_argument('--board-bin-dir', type=Path, default=ROOT/'target/debug')
    a = p.parse_args()
    if not __debug__ or os.name != 'posix':
        p.error('Requires POSIX and assertions enabled')
    with TemporaryDirectory(prefix='neoism-pair-') as tmp, ExitStack() as stack:
        verify(Path(tmp).resolve(), a.board_bin_dir.resolve(), a.neoism_agent.resolve(), stack)