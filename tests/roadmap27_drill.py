"""Offline cross-process acceptance drill. Run directly; see roadmap27_drill.md."""
import argparse
import hashlib
import hmac
import json
import os
from pathlib import Path
import secrets
import socket
import sqlite3
import subprocess as sp
import sys
import tempfile
import time
from contextlib import ExitStack
from urllib import error
from roadmap27_provider import provider

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from scripts.github_bridge import Bridge, client


def eventually(fn):
    end = time.monotonic() + 20
    while True:
        try:
            result = fn()
            if result:
                return result
        except (ConnectionError, FileNotFoundError, error.URLError) as exc:
            if isinstance(exc, error.HTTPError):
                raise
        if time.monotonic() > end:
            raise TimeoutError(fn)
        time.sleep(.05)


def drill(base, neo, stack):
    auth = secrets.token_urlsafe()
    env = dict(PATH=os.defpath, HOME=str(base), XDG_DATA_HOME=str(base/'data'),
               XDG_CONFIG_HOME=str(base/'config'), XDG_CACHE_HOME=str(base/'cache'),
               XDG_STATE_HOME=str(base/'state'), NEOISM_AGENT_DISABLE_MODELS_FETCH='true',
               NEOISM_AGENT_DISABLE_EMBEDDINGS='1', NEOISM_AGENT_TOKEN=auth,
               NEOISM_CONFIG_HOME=str(base/'config/neoism'), RUST_LOG='error',
               NEOISM_AGENT_PROVIDER='fixture', NEOISM_AGENT_MODEL='drill',
               HTTPS_PROXY='http://127.0.0.1:1', HTTP_PROXY='http://127.0.0.1:1', NO_PROXY='127.0.0.1,localhost')

    def start(argv, stdout=sp.DEVNULL, **extra):
        p = sp.Popen(argv, cwd=base, env=dict(env, **extra), stdout=stdout)
        def stop():
            p.terminate()
            try:
                p.wait(timeout=5)
            except sp.TimeoutExpired:
                p.kill()
                p.wait()
        stack.callback(stop)
        return p, stop

    start([ROOT/'target/debug/neochatd', '--data-dir', base/'board', '--bind', '127.0.0.1:0'])
    endpoint = eventually(lambda: json.loads((base/'board/endpoint.json').read_text()))
    url = endpoint['url']
    host = client(url, endpoint['token'])
    admin = client(url, (base/'board/admin-token').read_text().strip())
    channel = 'general'
    def issue(actor, grants):
        return admin('/v1/admin/restricted-invites', dict(invite=dict(actor=actor, channels=[channel]), grants=grants))['token']

    bt = issue('bridge', ['read','reply','post_task'])
    wt = issue('watch', ['read'])
    worker_token = issue('worker', ['read','claim','start','reply','complete'])
    board = client(url, bt)
    host('/v1/actions', dict(type='create_channel', channel='outside'))
    host('/v1/actions', dict(type='post', channel='outside', task=True,
                             text='OUTSIDE_CANARY', labels={'github':'issue'}))
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    origin = f'http://127.0.0.1:{port}'
    mock = stack.enter_context(provider())
    settings = base/'config/agent'
    settings.mkdir(parents=True)
    mcp = dict(neoboard=dict(type='local', enabled=True, command=[str(ROOT/'target/debug/neochat-mcp')],
                             environment=dict(NEOCHAT_TOKEN=worker_token, NEOCHAT_URL=url)))
    local = dict(npm='@ai-sdk/openai-compatible', auth='none',
                 options=dict(baseURL=f'http://127.0.0.1:{mock.server_port}/v1'),
                 models=dict(drill=dict(name='Drill',toolCall=True,limit=dict(context=65536,output=8192))))
    (settings/'agent.json').write_text(json.dumps(dict(provider=dict(fixture=local),
                                                    enabledProviders=['fixture'],model='fixture/drill',mcp=mcp)))
    start([neo, 'serve', '--hostname','127.0.0.1','--port',str(port)])
    api = client(origin, auth)
    eventually(lambda: (api('/v2/sessions/status'), True))
    model = {'model': {'providerId':'fixture','id':'drill'}}
    sid, decoy = [api('/v2/sessions', model)['id'] for _ in range(2)]
    session = '/v2/sessions/' + sid
    cfg = dict(repo_id=19, repo='fixture/drill', channel=channel, board_url=url,
               board_actor='bridge', github_actor_id=5, issue_task_assignee='worker', answer_actor_ids=[2])
    config = base/'binding.json'
    config.write_text(json.dumps(cfg))
    db = base/'bridge.sqlite'
    secret = secrets.token_hex()
    def cli(script, *args, raw=None, **extra):
        return sp.check_output([sys.executable, '-B', ROOT/'scripts'/script,*map(str,args)],
                               input=raw, env=dict(env, **extra), cwd=base, timeout=30).decode().strip()
    def watch(*args):
        out = cli('board_watcher.py','--db',base/'assign',*args, NEOBOARD_WATCH_TOKEN=wt, NEOISM_WAKE_TOKEN=auth)
        return json.loads(out) if out else None
    def hook(event, delivery, comment=None):
        item = dict(number=27, id=2700, title='Offline acceptance', body='Work item', user={'id':1})
        payload = dict(repository={'id':19}, issue=item, action='opened' if comment is None else 'created')
        if comment:
            payload['comment'] = comment
            payload['sender'] = comment['user']
        raw = json.dumps(payload).encode()
        sig = 'sha256='+hmac.new(secret.encode(),raw,hashlib.sha256).hexdigest()
        return cli('github_bridge.py', db, config,'webhook',event,delivery,sig, raw=raw,
                   NEOCHAT_TOKEN=bt, GITHUB_TOKEN='unused', GITHUB_WEBHOOK_SECRET=secret)
    writes = []
    def github(path, body):
        assert path == '/repos/fixture/drill/issues/27/comments'
        writes.append(body['body'])
        return {'id':9000+len(writes)}
    def poll_bridge():
        bridge = Bridge(db,cfg,board,github)
        try:
            bridge.poll()
        finally:
            bridge.db.close()

    watch('init','--endpoint',url,'--channel',channel,'--match','assignment')
    watch('bind','--actor','worker','--endpoint',origin,'--session',sid,'--bridge-route','bridge','repo:19')
    # Start the actual foreground service before any work exists. Wait for cycle
    # output, not a fixed startup delay. Use --once only AFTER stopping it.
    service_log = base/'watcher.jsonl'
    log = stack.enter_context(service_log.open('wb'))
    running, stop_service = start([sys.executable,'-B',ROOT/'scripts/board_watcher.py','--db',base/'assign',
                                   'run','--interval','1'], log, NEOBOARD_WATCH_TOKEN=wt, NEOISM_WAKE_TOKEN=auth)
    def cycles():
        assert running.poll() is None, 'watcher service exited'
        return [json.loads(line) for line in service_log.read_bytes().splitlines(keepends=True) if line.endswith(b'\n')]
    def caught_up(cursor):
        return eventually(lambda: any(c['cursor']>=cursor for c in cycles()))
    eventually(cycles)
    assert hook('issues','open') == 'imported'
    assert hook('issues','open') == hook('issues','redelivery') == 'duplicate'
    with sqlite3.connect(db) as ledger:
        [(number, task)] = ledger.execute('SELECT number,thread FROM mapping')
        assert number == 27
    eventually(lambda: any(c['wake'] and c['wake']['status']=='accepted' for c in cycles()))
    [wake] = watch('wake-status')
    assert (wake['task'],wake['session'],wake['actor']) == (task,sid,'worker')
    def transcript(n, needle):
        def check():
            msgs = api(session+'/messages?order=asc')['items']
            assert not mock.errors, mock.errors
            for m in msgs:
                assert not m['info'].get('error'), m['info']['error']
                for p in m['parts']:
                    if p['type']=='tool':
                        assert p['state']['status']!='error', p['state']
            if sum(m['info']['role']=='user' for m in msgs) != n or msgs[-1]['info']['role'] != 'assistant':
                return None
            text = ''.join(p['text'] for p in msgs[-1]['parts'] if p['type']=='text')
            if needle in text and msgs[-1]['info']['time'].get('completed'):
                assert all(m['info']['sessionId']==sid for m in msgs)
                return msgs[-1]['info']['id'], text
        return eventually(check)
    turn1, _ = transcript(1,'Waiting for a linked answer.')
    caught_up(board('/v1/snapshot')['cursor'])
    [pending] = watch('question-status')
    assert (pending['task'],pending['session'],pending['status']) == (task,sid,'pending')
    question = pending['id']
    poll_bridge()
    assert writes == [f'Which color?\n\nBoard question reply ID: `{question}`\n\n<!-- neoboard:{question} -->'], writes
    # Valid signature and command, but no respondent authority. It stays ordinary thread data.
    outsider = dict(id=2699,user={'id':3},body=f'/neoboard-answer {question}\nUse beige.')
    assert hook('issue_comment','outsider',outsider)=='imported'
    caught_up(board('/v1/snapshot')['cursor'])
    assert watch('question-status')==[pending] and watch('wake-status')==[wake]
    color = 'cobalt'
    comment = dict(id=2701, user={'id':2}, body=f'/neoboard-answer {question}\nUse {color}.')
    assert hook('issue_comment','answer',comment) == 'imported'
    assert hook('issue_comment','again',comment) == 'duplicate'
    with sqlite3.connect(db) as ledger:
        answer, = ledger.execute("SELECT message FROM imported WHERE source='issue_comment:2701'").fetchone()
    turn2, _ = transcript(2,'Completion verified.')
    msgs = board('/v1/snapshot')['messages']
    response = f'Selected {color}.'
    [mid] = [m['id'] for m in msgs if m['text']==response and m['parent']==task]
    poll_bridge()
    poll_bridge()
    assert writes[1:] == [response+f'\n\n<!-- neoboard:{mid} -->']
    assert hook('issue_comment','echo',dict(id=9002,user={'id':5},body=writes[-1])) == 'echo'
    [root] = [m for m in msgs if m['parent'] is None]
    assert root['id']==task and root['task']['state']=='completed'
    assert len(msgs)==6 and all(m['channel']==channel for m in msgs)
    assert all(m['sender'].startswith('mcp:') and m['author']=='worker' for m in msgs if m['id'] in (question,mid))
    [canary] = [m for m in host('/v1/snapshot')['messages'] if m['channel']=='outside']
    assert canary['text']=='OUTSIDE_CANARY' and canary['task']['owner'] is None
    events = board('/v1/events?after=0&wait_ms=0')
    states = [(e['message']['task']['state'],e['cursor']) for e in events if e.get('message',{}).get('id')==task]
    assert [s for s,_ in states]==['waiting','ready','claimed','in_progress','completed'], states
    def history():
        return api(session+'/messages?order=asc')['items']
    before = history()
    caught_up(events[-1]['cursor'])
    service_wakes = [c['wake'] for c in cycles() if c['wake']]
    assert [w['status'] for w in service_wakes]==['accepted','accepted'], service_wakes
    stop_service()
    assert hook('issue_comment','replay-after-complete',comment)=='duplicate'
    reservations = [w['reservation'] for w in watch('wake-status')]
    for reservation in reservations:
        assert watch('replay',reservation)=={'id':reservation}
    restarts = [watch('run','--once') for _ in range(2)]
    assert all(r['cursor']==events[-1]['cursor'] and r['wake'] is None for r in restarts), restarts
    assert history()==before
    assert board('/v1/events?after=0&wait_ms=0')==events
    poll_bridge()
    assert len(writes)==2
    tools = [p['state']['input']['tool'] for m in before for p in m['parts'] if p['type']=='tool']
    assert tools==['neoboard.'+t for t in ('tasks_list','tasks_claim','tasks_start','reply','read','tasks_complete','tasks_list')], tools
    wakes = watch('wake-status')
    assert [(w['kind'],w['status'],w['session']) for w in wakes]==[(k,'accepted',sid) for k in ('assignment','answer')]
    assert [w['cursor'] for w in wakes]==[w['cursor'] for w in service_wakes]
    assert (wakes[1]['task'],wakes[1]['question'],wakes[1]['answer'])==(task,question,answer)
    [linked] = watch('question-status')
    assert (linked['status'],linked['id'],linked['answer'],linked['answer_route'],linked['answer_actor'])==('answered',question,answer,'repo:19','bridge')
    assert api('/v2/sessions/'+decoy+'/messages')['items']==[]
    assert all('OUTSIDE_CANARY' not in s for s in writes)
    with sqlite3.connect(db) as ledger:
        assert list(ledger.execute('SELECT message,remote FROM exported ORDER BY remote'))==[(question,9001),(mid,9002)]
    return dict(issue=number,task=task,state_events=states,session=sid,turns=[turn1,turn2],mcp_calls=tools,
                reservations=reservations,answer_route=linked['answer_route'],result=response,
                watcher_status=[(w['kind'],w['status']) for w in wakes],restart_cycles=restarts,restart_extra_turns=0,
                question=question,answer=answer,response=mid,remote_question=9001,remote_response=9002,
                decoy_session=decoy,decoy_turns=0)


if __name__=='__main__':
    if not __debug__:
        sys.exit('assertions required')
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--neoism-agent',type=Path,required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp, ExitStack() as stack:
        try:
            print(json.dumps(drill(Path(tmp),args.neoism_agent.resolve(),stack),indent=2))
        except error.HTTPError as exc:
            raise RuntimeError(exc.url, exc.code, exc.read()) from exc