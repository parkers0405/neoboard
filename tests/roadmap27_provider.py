"""Loopback Chat Completions fixture. No board client, credential or session API."""
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import re
from threading import Thread
import time


def completion(body):
    messages = body['messages']
    if not body.get('tools'):  # auxiliary title/summary request
        return {'content': 'Color selection'}
    i = max(i for i, m in enumerate(messages) if m['role'] == 'user' and 'NeoBoard ' in str(m['content']))
    prompt = messages[i]['content']
    task = int(re.search(r'task #(\d+)', prompt)[1])
    answer = 'NeoBoard answer:' in prompt
    assert answer or 'NeoBoard assignment:' in prompt
    results = [json.loads(m['content']) for m in messages[i+1:] if m['role'] == 'tool']
    if answer:
        calls = [('read', dict(thread=task)), ('tasks_complete', dict(id=task)), ('tasks_list', dict(thread=task))]
        if results:
            # Derive the result from the scoped MCP read, not the harness or wake text.
            qid, aid = map(int, re.search(r'question #(\d+) and answer #(\d+)', prompt).groups())
            thread = {m['id']:m for m in results[0]['messages']}
            lease = thread[task]['task']
            assert lease['owner']==lease['assignee']=='worker' and lease['state']=='in_progress' and lease['expires_at']>time.time()
            q, a = thread[qid], thread[aid]
            assert q['parent']==a['parent']==task and q['labels']['watcher_kind']=='pending_question'
            assert a['author']=='bridge' and a['labels']==dict(watcher_kind='answer', watcher_question=str(qid), watcher_route='repo:19')
            match = re.fullmatch(r'Use ([a-z]+)\.', a['text'])
            assert match
            calls[1][1]['text'] = 'Selected ' + match[1] + '.'
    else:
        calls = [('tasks_list', dict(thread=task)), ('tasks_claim', dict(id=task, lease_secs=300)),
                 ('tasks_start', dict(id=task)), ('reply', dict(thread=task, text='Which color?',
                                                              labels={'watcher_kind':'pending_question'}))]
        if results:
            [root] = results[0]['messages']
            assert root['id']==task and root['task']['assignee']=='worker' and root['task']['state']=='ready'
    step = len(results)
    if step == len(calls):
        if answer:
            [root] = results[-1]['messages']
            assert root['id']==task and root['task']['state']=='completed'
        return {'content': 'Completion verified.' if answer else 'Waiting for a linked answer.'}
    tool, args = calls[step]
    names = [s['function']['name'] for s in body['tools']]
    assert 'execute' in names, names
    fn = dict(name='execute', arguments=json.dumps(dict(action='call', tool='neoboard.'+tool, arguments=args)))
    return dict(tool_calls=[dict(index=0, id=f'call_{int(answer)}_{step}', type='function', function=fn)])


@contextmanager
def provider():
    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            if self.path != '/v1/chat/completions':
                return self.send_error(404)
            try:
                delta = completion(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            except Exception as exc:
                self.server.errors.append(repr(exc))
                return self.send_error(400, 'unexpected provider request')
            chunks = [dict(choices=[dict(index=0, delta=delta)]), dict(choices=[dict(index=0, delta={},
                      finish_reason='tool_calls' if 'tool_calls' in delta else 'stop')])]
            data = ''.join('data: '+json.dumps(c)+'\n\n' for c in chunks)+'data: [DONE]\n\n'
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            self.wfile.write(data.encode())

        def log_message(self, *args):
            pass

    with ThreadingHTTPServer(('127.0.0.1', 0), Handler) as server:
        server.errors = []
        thread = Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield server
        finally:
            server.shutdown()
            thread.join()