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
from urllib import error, request

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
               NEOISM_AGENT_PROVIDER='neoism', NEOISM_AGENT_MODEL='stub')

    def start(argv):
        p = sp.Popen(argv, cwd=base, env=env, stdout=sp.DEVNULL)
        def stop():
            p.terminate()
            try:
                p.wait(timeout=5)
            except sp.TimeoutExpired:
                p.kill()
                p.wait()
        stack.callback(stop)

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
    worker_token = issue('worker', ['read','claim','reply'])
    agent = client(url, worker_token)
    board = client(url, bt)
    host('/v1/actions', dict(type='create_channel', channel='outside'))
    host('/v1/actions', dict(type='post', channel='outside', task=True,
                             text='OUTSIDE_CANARY', labels={'github':'issue'}))
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    origin = f'http://127.0.0.1:{port}'
    start([neo, 'serve', '--hostname','127.0.0.1','--port',str(port)])
    api = client(origin, auth)
    eventually(lambda: (api('/v2/sessions/status'), True))
    model = {'model': {'providerId':'neoism','id':'stub'}}
    sid, decoy = [api('/v2/sessions', model)['id'] for _ in range(2)]
    session = '/v2/sessions/' + sid
    cfg = dict(repo_id=19, repo='fixture/drill', channel=channel, board_url=url,
               board_actor='bridge', github_actor_id=5, issue_task_assignee='worker')
    config = base/'binding.json'
    config.write_text(json.dumps(cfg))
    db = base/'bridge.sqlite'
    secret = secrets.token_hex()
    def cli(script, *args, raw=None, **extra):
        return sp.check_output([sys.executable, '-B', ROOT/'scripts'/script,*map(str,args)],
                               input=raw, env=dict(env, **extra), cwd=base, timeout=30).decode().strip()
    def watch(name, *args):
        out = cli('board_watcher.py','--db',base/name,*args, NEOBOARD_WATCH_TOKEN=wt, NEOISM_WAKE_TOKEN=auth)
        return json.loads(out) if out else None
    def hook(event, delivery, comment=None):
        item = dict(number=27, id=2700, title='Offline acceptance', body='Work item', user={'id':1})
        payload = dict(repository={'id':19}, issue=item, action='opened' if comment is None else 'created')
        if comment:
            payload['comment'] = comment
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

    watch('assign','init','--endpoint',url,'--channel',channel,'--match','assignment')
    watch('assign','bind','--actor','worker','--endpoint',origin,'--session',sid)
    assert hook('issues','open') == 'imported'
    assert hook('issues','open') == hook('issues','redelivery') == 'duplicate'
    with sqlite3.connect(db) as ledger:
        [(number, task)] = ledger.execute('SELECT number,thread FROM mapping')
        assert number == 27
    watch('assign','poll')
    assert watch('assign','wake')['status'] == 'accepted'
    [wake] = watch('assign','wake-status')
    assert (wake['task'],wake['session'],wake['actor']) == (task,sid,'worker')
    def transcript(n, needle):
        def check():
            msgs = api(session+'/messages?order=asc')['items']
            if len(msgs) != n or msgs[-1]['info']['role'] != 'assistant':
                return None
            text = ''.join(p['text'] for p in msgs[-1]['parts'] if p['type']=='text')
            if needle in text and msgs[-1]['info']['time'].get('completed'):
                assert all(m['info']['sessionId']==sid for m in msgs)
                return msgs[-1]['info']['id'], text
        return eventually(check)
    turn1, _ = transcript(2,f'task #{task}')
    def act(key, *args):
        return json.loads(sp.check_output([ROOT/'target/debug/neochat','--json','--request-id',key,*map(str,args)],
                          env=dict(env, NEOCHAT_URL=url, NEOCHAT_TOKEN=worker_token, NEOCHAT_AGENT_FRAMEWORK='Neoism'),
                          cwd=base, timeout=15))
    claim = act(turn1+':claim','tasks','claim',task,'--lease-secs','120')
    question = act(turn1+':question','reply',task,'Which color?')['id']
    poll_bridge()
    assert writes == [f'Which color?\n\n<!-- neoboard:{question} -->']
    comment = dict(id=2701, user={'id':2}, body='Use cobalt.')
    assert hook('issue_comment','answer',comment) == 'imported'
    assert hook('issue_comment','again',comment) == 'duplicate'
    # Explicit drill-only reply consumer: assignment mode alone must NOT resume.
    watch('assign','poll')
    assert watch('assign','wake') is None
    watch('reply','init','--endpoint',url,'--channel',channel,'--match','reply','--thread',task)
    watch('reply','poll')
    with sqlite3.connect(db) as ledger:
        answer, = ledger.execute("SELECT message FROM imported WHERE source='issue_comment:2701'").fetchone()
    while (taken := watch('reply','take')):
        delivery = taken['delivery']
        m = delivery['event']['message']
        assert (m['channel'],m['parent']) == (channel,task)
        if m['id'] == answer:
            assert m['author']=='bridge' and m['text']==comment['body']
            # Consume the reply only on the endpoint/session captured at opt-in.
            assert wake['endpoint'] == origin
            req = request.Request(origin+'/v2/sessions/'+wake['session']+'/prompt-if-idle',
                                  json.dumps({'prompt':m['text']}).encode(),
                                  {'Authorization':'Bearer '+auth,'Content-Type':'application/json'})
            with request.build_opener(request.ProxyHandler({})).open(req,timeout=5) as res:
                assert res.status == 204
        watch('reply','ack',delivery['id'],taken['receipt'])
    turn2, response = transcript(4,comment['body'])
    mid = act(turn2,'reply',task,response)['id']
    poll_bridge()
    poll_bridge()
    assert writes[1:] == [response+f'\n\n<!-- neoboard:{mid} -->']
    assert hook('issue_comment','echo',dict(id=9002,user={'id':5},body=writes[-1])) == 'echo'
    msgs = board('/v1/snapshot')['messages']
    [root] = [m for m in msgs if m['parent'] is None]
    assert root['id']==task and root['task']['owner']=='worker' and root['task']['state']=='claimed'
    assert len(msgs)==5 and all(m['channel']==channel for m in msgs)
    assert all(m['sender']=='cli:Neoism' and m['author']=='worker' for m in msgs if m['id'] in (question,mid))
    [canary] = [m for m in host('/v1/snapshot')['messages'] if m['channel']=='outside']
    assert canary['text']=='OUTSIDE_CANARY' and canary['task']['owner'] is None
    events = agent('/v1/events?after=0&wait_ms=0')
    assert [e['cursor'] for e in events if (e.get('message',{}).get('task') or {}).get('owner')=='worker']==[claim['cursor']]
    assert api('/v2/sessions/'+decoy+'/messages')['items']==[]
    assert all('OUTSIDE_CANARY' not in s for s in writes)
    with sqlite3.connect(db) as ledger:
        assert list(ledger.execute('SELECT message,remote FROM exported ORDER BY remote'))==[(question,9001),(mid,9002)]
    return dict(issue=number,task=task,claim_cursor=claim['cursor'],session=sid,turns=[turn1,turn2],
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