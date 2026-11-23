"""Offline inbox-to-session acceptance test; pass explicit --board-bin-dir and --neoism-agent."""
import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import re
import secrets
import signal
import socket
import subprocess as sp
import sys
import tempfile
from urllib import request

from roadmap27_drill import ROOT, client, eventually
from roadmap27_provider import provider


DELEGATE = ('Delegate a square-check task in general to writer. Register a completed callback from the creation '
            'cursor. When it arrives, verify the writer\'s square from the completed thread, post one review there, '
            'and acknowledge the notification.')
TITLE = 'Compute a square'


def call(step, tool, args):
    fn = dict(name='execute', arguments=json.dumps(dict(action='call', tool='neoboard.'+tool, arguments=args)))
    return dict(tool_calls=[dict(index=0, id=f'call_{step}', type='function', function=fn)])


def completion(body):
    if not body.get('tools'):
        return {'content': 'Inbox drill'}
    messages = body['messages']
    i = max(i for i, m in enumerate(messages) if m['role'] == 'user')
    prompt = messages[i]['content']
    results = [json.loads(m['content']) for m in messages[i+1:] if m['role'] == 'tool']
    step = len(results)
    if prompt == DELEGATE:
        if not results:
            return call(step, 'tasks_create', dict(channel='general', title=TITLE, assignee='writer',
                        description='Choose an integer and report it and its square.'))
        task = results[0]
        filt = dict(thread=task['id'], kind='completed', after_cursor=task['cursor'])
        if step == 1:
            return call(step, 'callbacks_register', filt)
        assert step == 2 and all(results[1][k] == v for k, v in filt.items())
        return {'content': 'Delegated and subscribed.'}

    key, kind, thread, mid = re.search(r'NeoBoard inbox ([a-f0-9]+:\d+): (mention|completed) on thread #(\d+), message #(\d+)', prompt).groups()
    assert 'nonce=' not in prompt
    thread, mid = int(thread), int(mid)
    if not results:
        return call(step, 'inbox_list', {})
    page = results[0]
    [item] = page['items']
    assert page['actor'] == 'reviewer' and item['actor'] == 'writer' and item['kind'] == kind and not item['read']
    assert (item['thread'], item['message_id'], item['id']) == (thread, mid, int(key.split(':')[1]))
    ack = dict(id=item['id'], read=True)
    if step == 1:
        return call(step, 'read', dict(thread=thread))
    if kind == 'mention':
        [source] = results[1]['messages']
        assert source['id'] == mid and source['author'] == 'writer' and source['sender'].startswith('mcp:')
        nonce = re.fullmatch(r'@reviewer nonce=([0-9a-f]+)', source['text'])[1]
        done = 'Acknowledged ' + nonce
        ack_step = 2
    else:
        assert any(m['role'] == 'user' and m['content'] == DELEGATE for m in messages[:i])
        root, result = results[1]['messages']
        assert root['id'] == thread and root['task']['state'] == 'completed' and root['author'] == 'reviewer'
        assert root['task']['assignee'] == 'writer' and result['parent'] == thread
        assert result['author'] == 'writer' and result['sender'].startswith('mcp:')
        n, square = map(int, re.fullmatch(r'Square: (\d+) -> (\d+)', result['text']).groups())
        assert n*n == square and result['text'] not in prompt
        done = f'Verified {n} squared is {square}.'
        if step == 2:
            return call(step, 'reply', dict(thread=thread, text=done))
        assert results[2]['id'] > result['id']
        ack_step = 3
    if step == ack_step:
        return call(step, 'inbox_set_read', ack)
    assert step == ack_step+1 and results[-1] == ack
    return {'content': done}


def peer_call(binary, base, env, name, tool, args):
    frames = [dict(id=1, method='initialize', params=dict(protocolVersion='2025-06-18', capabilities={},
                                                          clientInfo=dict(name=name, version='1'))),
              dict(method='notifications/initialized'), dict(id=2, method='tools/call', params=dict(
                  name=tool, arguments=args))]
    raw = ''.join(json.dumps(dict(jsonrpc='2.0', **r))+'\n' for r in frames)
    out = sp.check_output([binary], input=raw, text=True, cwd=base, env=env, timeout=15)
    init, result = map(json.loads, out.splitlines())
    assert not result['result']['isError'], result
    return init['result']['serverInfo']['version'], result['result']['structuredContent']


def drill(base, bins, neo, stack, case):
    auth = secrets.token_urlsafe()
    env = dict(PATH=os.defpath, HOME=str(base), XDG_DATA_HOME=str(base/'data'), XDG_CONFIG_HOME=str(base/'config'),
               XDG_CACHE_HOME=str(base/'cache'), XDG_STATE_HOME=str(base/'state'), NEOISM_CONFIG_HOME=str(base/'config/neoism'),
               NEOISM_AGENT_DISABLE_MODELS_FETCH='true', NEOISM_AGENT_DISABLE_EMBEDDINGS='1', NEOISM_AGENT_TOKEN=auth,
               NEOISM_AGENT_PROVIDER='fixture', NEOISM_AGENT_MODEL='drill', RUST_LOG='error', HTTPS_PROXY='http://127.0.0.1:1',
               HTTP_PROXY='http://127.0.0.1:1', NO_PROXY='127.0.0.1,localhost')

    def start(argv, environment=env, stdout=sp.DEVNULL):
        p = sp.Popen(argv, env=environment, cwd=base, stdout=stdout, start_new_session=True)
        def stop():
            try:
                os.killpg(p.pid, signal.SIGTERM)
                p.wait(timeout=5)
            except ProcessLookupError:
                pass
            except sp.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
                p.wait()
        stack.callback(stop)
        return p

    start([bins/'neochatd', '--data-dir', base/'board', '--bind', '127.0.0.1:0'])
    endpoint = eventually(lambda: json.loads((base/'board/endpoint.json').read_text()))
    url = endpoint['url']
    admin = client(url, (base/'board/admin-token').read_text().strip())
    def issue(name, *grants):
        return admin('/v1/admin/restricted-invites', dict(invite=dict(actor=name, channels=['general']), grants=grants))['token']
    delegation = case == 'completed'
    reviewer = issue('reviewer', 'read', 'notifications', *(['post_task','assign','reply'] if delegation else []))
    writer = issue('writer', 'read', *(['claim','start','complete'] if delegation else ['post']))
    board = client(url, reviewer)
    mock = stack.enter_context(provider(completion))
    settings = base/'config/agent'
    settings.mkdir(parents=True)
    mcp = dict(neoboard=dict(type='local', enabled=True, command=[str(bins/'neochat-mcp')],
                             environment=dict(NEOCHAT_TOKEN=reviewer, NEOCHAT_URL=url)))
    fixture = dict(npm='@ai-sdk/openai-compatible', auth='none', options=dict(baseURL=f'http://127.0.0.1:{mock.server_port}/v1'),
                   models=dict(drill=dict(name='Drill', toolCall=True, limit=dict(context=65536, output=8192))))
    (settings/'agent.json').write_text(json.dumps(dict(provider=dict(fixture=fixture), enabledProviders=['fixture'], model='fixture/drill', mcp=mcp)))
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    origin = f'http://127.0.0.1:{port}'
    start([neo, 'serve', '--hostname', '127.0.0.1', '--port', str(port)])
    api = client(origin, auth)
    eventually(lambda: (api('/v2/sessions/status'), True))
    sid, decoy = [api('/v2/sessions', {'model': {'providerId':'fixture','id':'drill'}})['id'] for _ in range(2)]
    def history(s):
        return api('/v2/sessions/'+s+'/messages?order=asc')['items']

    watch = [sys.executable, '-B', ROOT/'scripts/inbox_watcher.py', '--db', base/'inbox.db']
    watch_env = dict(env, NEOBOARD_INBOX_TOKEN=reviewer, NEOISM_WAKE_TOKEN=auth)
    def cli(*args):
        raw = sp.check_output(watch+list(args), env=watch_env, cwd=base, timeout=20)
        return json.loads(raw) if raw else None
    cli('init', '--allow-inbox-wake', '--board', url, '--actor', 'reviewer', '--runtime', origin, '--session', sid)
    assert cli('run', '--once') == dict(status='empty', watermark=0) and not history(sid) and not history(decoy)
    log = base/'cycles.jsonl'
    service = start(watch+['run', '--interval', '1'], watch_env, stack.enter_context(log.open('wb')))
    def finished(text):
        assert not mock.errors, mock.errors
        assert service.poll() is None, 'inbox watcher exited: '+log.read_text()
        msgs = history(sid)
        for m in msgs:
            assert not m['info'].get('error'), m['info']['error']
            for p in m['parts']:
                if p['type'] == 'tool':
                    assert p['state']['status'] != 'error', p['state']
        if msgs and msgs[-1]['info']['role'] == 'assistant' and msgs[-1]['info']['time'].get('completed'):
            if any(p.get('text') == text for p in msgs[-1]['parts']):
                return msgs

    def peer(who, tool, **kwargs):
        token = writer if who == 'writer' else reviewer
        return peer_call(bins/'neochat-mcp', base, dict(env, NEOCHAT_URL=url, NEOCHAT_TOKEN=token), 'drill-'+who, tool, kwargs)

    details = {}
    if delegation:
        # Only this setup turn is injected by the operator; the watcher alone submits the review turn.
        req = request.Request(origin+'/v2/sessions/'+sid+'/prompt-if-idle', json.dumps(dict(prompt=DELEGATE)).encode(),
                              {'Authorization':'Bearer '+auth, 'Content-Type':'application/json'})
        with request.build_opener(request.ProxyHandler({})).open(req, timeout=5) as r:
            assert r.status == 204
        setup = eventually(lambda: finished('Delegated and subscribed.'))
        [sub] = board('/v1/subscriptions')
        task = sub['thread']
        assert sub['kind'] == 'completed' and client(url,writer)('/v1/subscriptions') == []
        [root] = board('/v1/snapshot')['messages']
        assert (root['id'], root['author'], root['task']['assignee'], root['task']['state']) == (task,'reviewer','writer','ready')
        n = 10000 + secrets.randbelow(90000)
        expected = f'Verified {n} squared is {n*n}.'
        version, _ = peer('writer', 'tasks_claim', id=task, lease_secs=60)
        peer('writer', 'tasks_start', id=task)
        peer('writer', 'tasks_complete', id=task, text=f'Square: {n} -> {n*n}')
        mid = task  # transition notification points at the task, not the result reply
        expected_tools = ['tasks_create','callbacks_register','inbox_list','read','reply','inbox_set_read']
    else:
        nonce = secrets.token_hex(8)
        version, post = peer('writer', 'post', channel='general', text='@reviewer nonce='+nonce)
        mid, expected = post['id'], 'Acknowledged '+nonce
        expected_tools = ['inbox_list','read','inbox_set_read']

    before = eventually(lambda: finished(expected))
    users = [m for m in before if m['info']['role']=='user']
    assert len(users) == (2 if delegation else 1) and all(m['info']['sessionId']==sid for m in before)
    tools = [p['state']['input']['tool'] for m in before for p in m['parts'] if p['type']=='tool']
    assert tools == ['neoboard.'+t for t in expected_tools], tools
    [item] = board('/v1/inbox?read=true')['items']
    assert item['message_id'] == mid and item['kind'] == case and item['read'] and not board('/v1/inbox')['items']
    eventually(lambda: log.read_bytes().endswith(b'\n'))
    service.terminate()
    service.wait(timeout=5)
    [accepted] = list(map(json.loads, log.read_text().splitlines()))
    state = cli('status')
    ident = item['id']
    key = state['namespace']+':'+str(ident)
    assert accepted == dict(status='accepted', reservation=key)
    assert state['reservations'] == [dict(id=ident,key=key,status='accepted')]
    assert any('NeoBoard inbox '+key+': '+case in p.get('text','') for p in users[-1]['parts'])
    events = board('/v1/events?after=0&wait_ms=0')
    if delegation:
        assert before[:len(setup)] == setup
        changes = [(e['message']['task']['state'],e['message']['task']['owner'],e['cursor']) for e in events
                   if e.get('message',{}).get('id') == task]
        assert [s for s,_,_ in changes] == ['waiting','ready','claimed','in_progress','completed'], changes
        assert changes[0][2] <= sub['after_cursor'] <= changes[1][2] <= sub['registered_cursor'] < changes[2][2]
        assert changes[2][1] == changes[3][1] == 'writer' and item['cursor'] == changes[-1][2]
        root, result, review = board('/v1/snapshot')['messages']
        assert (review['parent'],review['author'],review['text']) == (task,'reviewer',expected)
        assert result['author'] == 'writer' and all(m['sender'].startswith('mcp:') for m in (root,result,review))
        details = dict(callback=sub, states=changes, result_message=result['id'], review_message=review['id'],
                       review_replies=1, setup_turns=1, wake_turns=1)
        # Re-register from BEFORE completion: a true board-side historical replay, with no extra session prompt.
        peer('reviewer', 'callbacks_cancel', id=sub['id'])
        _, replay_sub = peer('reviewer', 'callbacks_register', thread=task, kind='completed', after_cursor=sub['after_cursor'])
        assert replay_sub['registered_cursor'] >= changes[-1][2] and board('/v1/inbox?read=true')['items'] == [item]
    # Re-expose the exact delivery. An empty unread inbox would not test the watermark.
    req = request.Request(f'{url}/v1/inbox/{ident}/read', b'{"read":false}',
                          {'Authorization':'Bearer '+reviewer,'Content-Type':'application/json'})
    with request.build_opener(request.ProxyHandler({})).open(req, timeout=5) as r:
        assert 200 <= r.status < 300
    [replayed] = board('/v1/inbox')['items']
    assert replayed == dict(item, read=False)
    restarts = [cli('run','--once') for _ in range(2)]
    assert restarts == [dict(status='empty', watermark=ident)] * 2
    assert cli('status') == state and history(sid) == before and history(decoy) == []
    assert board('/v1/events?after=0&wait_ms=0') == events
    return dict(case=case, board_bin_dir=str(bins), mcp_version=version, neoism_agent=str(neo), actor='reviewer', session=sid,
                decoy_session=decoy, message=mid, receipt=accepted, mcp_calls=tools, result=expected,
                restarts=restarts, restart_extra_turns=0, decoy_messages=0, **details)


if __name__ == '__main__':
    if not __debug__:
        sys.exit('assertions required')
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--board-bin-dir', required=True, type=Path)
    parser.add_argument('--neoism-agent', required=True, type=Path)
    parser.add_argument('--case', choices=['mention','completed','both'], default='both')
    args = parser.parse_args()
    for case in (('mention','completed') if args.case == 'both' else [args.case]):
        with tempfile.TemporaryDirectory() as tmp, ExitStack() as stack:
            print(json.dumps(drill(Path(tmp), args.board_bin_dir.resolve(), args.neoism_agent.resolve(), stack, case), indent=2))