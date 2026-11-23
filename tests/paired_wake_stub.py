"""Copied as a private executable in the test workspace. Only existing IDs are resumable."""
import json
from pathlib import Path
import sys


def emit(**message):
    print(json.dumps(message), flush=True)


def receive(method):
    msg = json.loads(input())
    assert msg['method'] == method
    return msg


def append(sid, prompt):
    path = Path(sid)
    assert path.parent == Path('.') and path.is_file() and prompt.strip()
    with path.open('a') as out:
        out.write(json.dumps(prompt) + '\n')


args = sys.argv[1:]
if args == ['--version']:
    print('wake drill fixture')
    sys.exit(0)
if Path(sys.argv[0]).name == 'claude':
    sid = args[args.index('--resume')+1]
    assert '--print' in args or '-p' in args
    assert args[args.index('--output-format')+1] == 'json'
    append(sid, args[-1])
    emit(type='result', subtype='success', is_error=False, session_id=sid)
else:
    assert args == ['app-server'], args
    emit(id=receive('initialize')['id'], result=dict(userAgent='fixture'))
    receive('initialized')
    req = receive('thread/resume')
    sid = req['params']['threadId']
    assert Path(sid).is_file()
    emit(id=req['id'], result=dict(thread=dict(id=sid)))
    req = receive('turn/start')
    assert req['params']['threadId'] == sid
    [part] = req['params']['input']
    assert part['type'] == 'text'
    append(sid, part['text'])
    emit(id=req['id'], result=dict(turn=dict(id='fixture-turn')))
    emit(method='turn/completed', params=dict(threadId=sid, turn=dict(id='fixture-turn', status='completed')))