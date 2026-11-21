"""Fake executable copied into an isolated PATH as claude, codex or opencode."""

import json
import os
from pathlib import Path
import sys


def emit(**fields):
    print(json.dumps(fields), flush=True)


def deliver(session, prompt):
    assert prompt == 'Check NeoBoard task #17 and verify its current assignment before acting.'
    path = Path(session)
    assert path.is_file()  # no implicit session creation
    with path.open('a') as history:
        history.write(prompt + '\n')


def receive(method):
    msg = json.loads(input())
    assert msg['method'] == method
    return msg


runtime = Path(sys.argv[0]).name
Path('launches').touch()
wrong = os.environ.get('WRONG_ID')
args = sys.argv[1:]
if runtime == 'claude':
    _, session, _, _, _, prompt = args
    assert args[:-1] == ['--resume', session, '--print', '--output-format', 'json']
    deliver(session, prompt)
    emit(type='result', subtype='success', is_error=False, session_id=wrong or session)
elif runtime == 'opencode':
    _, _, session, _, _, prompt = args
    assert args[:-1] == ['run', '--session', session, '--format', 'json']
    deliver(session, prompt)
    emit(type='step_finish', sessionID=wrong or session)
elif runtime == 'codex':
    assert args == ['app-server']
    init = receive('initialize')
    assert init['params']['clientInfo']['name']
    emit(id=init['id'], result={'userAgent': 'fixture'})
    receive('initialized')
    resume = receive('thread/resume')
    session = resume['params']['threadId']
    assert Path(session).is_file()
    emit(id=resume['id'], result={'thread': {'id': wrong or session}})
    start = receive('turn/start')
    assert start['params']['threadId'] == session
    part, = start['params']['input']
    assert part['type'] == 'text'
    deliver(session, part['text'])
    emit(id=start['id'], result={'turn': {'id': 'turn1'}})
    emit(method='turn/completed', params={'threadId': session, 'turn': {
        'id': os.environ.get('COMPLETED_TURN', 'turn1'), 'status': 'completed'}})