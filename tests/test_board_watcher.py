"""Standalone CLI contract tests; no daemon, discovery, grants, or shell."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from importlib.metadata import version
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

import turso


CLI = Path(__file__).resolve().parents[1] / 'scripts/board_watcher.py'


def msg(cursor, channel='project/dev', id=1, parent=None, label=None, state=None):
    return dict(type='message', cursor=cursor, message=dict(id=id, parent=parent, channel=channel,
                labels={} if label is None else {'status': label}, task={'state': state} if state else None))


class WatcherFixture(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.db = Path(tmp.name) / 'watch.db'
        self.events = []
        self.status = 200
        self.cursors = []
        self.session_status = {}
        self.on_status = None
        self.returned_session = 'ses_fixture'
        self.page_size = 1000
        self.watch_token = 'fixture'
        self.prompts = []
        self.post_status = 204
        self.posts = []
        self.received = threading.Event()
        self.reply_gate = threading.Event()
        self.reply_gate.set()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                url = urlsplit(self.path)
                if url.path.startswith('/v2/') and self.headers.get('Authorization') == 'Bearer neo-fixture':
                    if url.path == '/v2/sessions/status' and fixture.on_status:
                        fixture.on_status()
                    data = {'/v2/sessions/ses_fixture': {'id': fixture.returned_session},
                            '/v2/sessions/status': fixture.session_status}
                    if url.path not in data:
                        self.send_error(404)
                        return
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(json.dumps(data[url.path]).encode())
                    return
                if url.path != '/v1/events' or self.headers.get('Authorization') != 'Bearer fixture':
                    self.send_error(403)
                    return
                cursor = int(parse_qs(url.query)['after'][0])
                fixture.cursors.append(cursor)
                self.send_response(fixture.status)
                self.end_headers()
                self.wfile.write(json.dumps([e for e in fixture.events if e['cursor'] > cursor][:fixture.page_size]).encode())

            def do_POST(self):
                fixture.posts.append(self.path)
                if self.path != '/v2/sessions/ses_fixture/prompt-if-idle' or self.headers.get('Authorization') != 'Bearer neo-fixture':
                    self.send_error(403)
                    return
                if fixture.post_status in (404, 412):
                    self.send_response(fixture.post_status)
                    self.end_headers()
                    self.wfile.write(b'{"code":"session.not_idle"}' if fixture.post_status == 412 else b'{}')
                    return
                fixture.prompts.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
                fixture.received.set()
                fixture.reply_gate.wait(10)
                self.send_response(fixture.post_status)
                self.end_headers()

            def log_message(self, *_):
                pass

        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.url = 'http://127.0.0.1:' + str(server.server_port)
        worker = threading.Thread(target=server.serve_forever)
        worker.start()
        self.addCleanup(server.server_close)
        self.addCleanup(worker.join)
        self.addCleanup(server.shutdown)

    def cli(self, *args, fails=False):
        p = subprocess.run([sys.executable, str(CLI), '--db', str(self.db), *args],
                           env=dict(os.environ, NEOBOARD_WATCH_TOKEN=self.watch_token, NEOISM_WAKE_TOKEN='neo-fixture'),
                           capture_output=True, text=True, timeout=10)
        if fails:
            self.assertNotEqual(p.returncode, 0)
        else:
            self.assertEqual(p.returncode, 0, p.stderr)
            return json.loads(p.stdout) if p.stdout else None

    def init(self, mode, *args):
        self.cli('init', '--endpoint', self.url, '--match', mode, '--channel', 'project/dev', *args)


class WatcherTest(WatcherFixture):
    def test_atomic_restart_retry_and_replay(self):
        self.init('label', '--label', 'status', '--before', 'open', '--after', 'closed')
        with closing(turso.connect(str(self.db))) as db:
            self.assertEqual(version('pyturso'), '0.8.3')
            engine, cursor = db.execute('SELECT turso_version(),cursor FROM watcher').fetchone()
            self.assertTrue(engine)  # Turso-specific SQL; not a file format check.
            self.assertEqual(cursor, 0)
        del db
        self.events = [msg(2, label='open')]
        self.cli('poll')
        self.status = 503
        self.cli('poll', fails=True)
        self.status = 200
        self.events.extend([msg(7, label='closed'), {'cursor': 9}])
        self.cli('poll', fails=True)
        self.assertEqual(self.cli('inspect'), dict(cursor=2, deliveries=[]))
        self.events[-1] = msg(9, label='closed')
        self.cli('poll')
        self.cli('poll')
        self.assertEqual(self.cursors, [0, 2, 2, 2, 9])
        first = self.cli('take', '--lease', '30')
        delivery = first['delivery']
        id = delivery['id']
        self.assertEqual(delivery['details'], dict(key='status', before='open', after='closed'))
        self.assertIsNone(self.cli('take'))
        self.cli('fail', id, first['receipt'], '--delay', '1', '--reason', 'offline')
        self.assertEqual(self.cli('inspect')['deliveries'][0]['last_error'], 'offline')
        time.sleep(1.1)
        abandoned = self.cli('take', '--lease', '1')
        time.sleep(1.1)
        recovered = self.cli('take')
        self.cli('ack', id, abandoned['receipt'], fails=True)
        self.cli('ack', id, recovered['receipt'])
        self.assertIsNone(self.cli('take'))
        self.cli('replay', id)
        replay = self.cli('take')
        self.assertEqual(replay['delivery'], delivery)
        self.assertEqual(replay['attempt'], 4)
        self.assertEqual(self.cli('inspect')['cursor'], 9)

    def test_scoped_matches(self):
        self.events = [msg(1, channel='project/dev2', state='ready'), msg(3, state='ready'),
                       msg(4, state='ready'), msg(5, state='blocked'), msg(6, state='ready'),
                       msg(7, id=7, parent=1), msg(8, id=7, parent=1), msg(9, id=9, parent=2)]
        path = self.db
        for mode, extra, expected in [('channel', [], [3, 4, 5, 6, 7, 8, 9]),
                                      ('reply', ['--thread', '1'], [7]), ('task-ready', [], [3, 6])]:
            with self.subTest(mode=mode):
                self.db = path.with_name(mode)
                self.init(mode, *extra)
                self.cli('poll')
                rows = self.cli('inspect')['deliveries']
                self.assertEqual([r['delivery']['event']['cursor'] for r in rows], expected)

    def test_follow_recovers_without_operator_retry(self):
        self.init('channel')
        self.status = 503
        self.events = [msg(12)]
        process = subprocess.Popen([sys.executable, str(CLI), '--db', str(self.db), 'follow'],
                                   env=dict(os.environ, NEOBOARD_WATCH_TOKEN='fixture'),
                                   stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 8
            while not self.cursors and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertEqual(self.cursors, [0])
            # Let the initial error response finish before enabling success.
            while len(self.cursors) < 2 and time.monotonic() < deadline:
                time.sleep(0.02)
            self.status = 200
            while 12 not in self.cursors and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertIn(12, self.cursors)
            # The poller remains running; two separate consumer processes race for one lease.
            with ThreadPoolExecutor(max_workers=2) as workers:
                claims = list(workers.map(self.cli, ['take', 'take']))
            self.assertEqual(claims.count(None), 1)
            claim = next(c for c in claims if c)
            self.assertEqual(claim['delivery']['event']['cursor'], 12)
            self.cli('ack', claim['delivery']['id'], claim['receipt'])
        finally:
            process.terminate()
            _, stderr = process.communicate(timeout=5)
        self.assertIn('503', stderr)
        self.assertIsNone(self.cli('take'))

    def assigned(self, cursor, actor):
        event = msg(cursor, id=16, state='ready')
        event['message']['task']['assignee'] = actor
        event['message']['text'] = 'launch ses_other with /bin/sh'
        return event

    def test_wake_only_bound_assignment_once(self):
        self.init('assignment')
        self.cli('bind', '--actor', 'alice', '--endpoint', self.url, '--session', 'ses_fixture')
        self.events = [self.assigned(1, 'bob'), self.assigned(2, 'alice'), self.assigned(3, 'alice')]
        self.cli('poll')
        self.session_status = {'ses_fixture': {'type': 'busy'}}
        self.assertEqual(self.cli('wake')['status'], 'busy')
        self.assertFalse(self.prompts)
        self.session_status = {}
        with ThreadPoolExecutor(max_workers=2) as workers:
            results = list(workers.map(self.cli, ['wake', 'wake']))
        self.assertCountEqual(results, [None, dict(cursor=2, status='accepted')])
        self.cli('replay', self.cli('inspect')['deliveries'][1]['id'])
        self.cli('poll')
        self.assertIsNone(self.cli('wake'))
        [prompt] = self.prompts
        for required in ('task #16', 'project/dev', 'scoped board MCP', 'tasks_claim', 'tasks_start', 'pending_question', 'tasks_complete'):
            self.assertIn(required, prompt['prompt'])
        self.assertNotIn('ses_other', prompt['prompt'])
        self.assertEqual([r['status'] for r in self.cli('wake-status')], ['unbound', 'accepted'])

    def test_ambiguous_wake_never_retried(self):
        self.init('assignment')
        self.cli('bind', '--actor', 'alice', '--endpoint', self.url, '--session', 'ses_fixture')
        self.events = [self.assigned(1, 'alice'), self.assigned(2, None), self.assigned(3, 'alice')]
        self.cli('poll')
        self.post_status = 503
        self.assertEqual(self.cli('wake'), dict(cursor=3, status='uncertain'))
        self.cli('replay', self.cli('inspect')['deliveries'][1]['id'])
        self.assertIsNone(self.cli('wake'))
        self.assertEqual([r['status'] for r in self.cli('wake-status')], ['superseded', 'uncertain'])
        self.assertEqual(len(self.prompts), 1)

    def test_crash_after_post_before_response(self):
        self.init('assignment')
        self.cli('bind', '--actor', 'alice', '--endpoint', self.url, '--session', 'ses_fixture')
        self.events = [self.assigned(1, 'alice')]
        self.cli('poll')
        self.reply_gate.clear()
        with subprocess.Popen([sys.executable, str(CLI), '--db', str(self.db), 'wake'],
                              env=dict(os.environ, NEOISM_WAKE_TOKEN='neo-fixture', NEOBOARD_WATCH_TOKEN='fixture')) as process:
            try:
                self.assertTrue(self.received.wait(5))
            finally:
                process.kill()
                self.reply_gate.set()
        self.assertEqual(self.cli('wake-status')[0]['status'], 'uncertain')
        self.assertIsNone(self.cli('wake'))
        self.assertEqual(len(self.prompts), 1)

    def test_raced_admission_and_unsupported_server(self):
        self.init('assignment')
        self.cli('bind', '--actor', 'alice', '--endpoint', self.url, '--session', 'ses_fixture')
        self.events = [self.assigned(1, 'alice')]
        self.cli('poll')
        self.post_status = 412  # idle in GET, competing admission wins at POST
        self.assertEqual(self.cli('wake')['status'], 'busy')
        self.assertFalse(self.prompts)
        self.post_status = 204
        self.assertEqual(self.cli('wake')['status'], 'accepted')
        self.events.extend([self.assigned(2, None), self.assigned(3, 'alice')])
        self.cli('poll')
        self.post_status = 404  # downgrade; must never try either legacy route
        self.assertEqual(self.cli('wake')['status'], 'unsupported_or_missing')
        self.assertIsNone(self.cli('wake'))
        self.assertEqual(self.posts, ['/v2/sessions/ses_fixture/prompt-if-idle'] * 3)
        self.assertEqual(len(self.prompts), 1)


if __name__ == '__main__':
    unittest.main()