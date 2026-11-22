"""Isolated CLI tests. Only loopback fixtures; optional disposable compiled board."""

import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib import request
from urllib.parse import parse_qs, urlsplit

from test_board_watcher import WatcherFixture


CLI = Path(__file__).resolve().parents[1] / 'scripts/inbox_watcher.py'


def item(id, kind='mention'):
    return dict(id=id, thread=4, message_id=12, cursor=900, read=False, kind=kind, actor='other',
                channel='project/dev', text='private-payload', author='other/private-byline')


class InboxCLI(WatcherFixture):
    def setUp(self):
        super().setUp()  # separate conditional runtime fixture
        self.board_token = 'board-fixture'
        self.env = dict(HOME=str(self.db.parent), PATH=os.defpath, PYTHONDONTWRITEBYTECODE='1', NEOISM_WAKE_TOKEN='neo-fixture')

    def command(self, *args):
        return [sys.executable, '-B', str(CLI), '--db', str(self.db), *args]

    def cli(self, *args, fails=False):
        p = subprocess.run(self.command(*args), env=dict(self.env, NEOBOARD_INBOX_TOKEN=self.board_token),
                           capture_output=True, text=True, timeout=10)
        if fails:
            self.assertNotEqual(p.returncode, 0)
            return p.stderr
        self.assertEqual(p.returncode, 0, p.stderr)
        return json.loads(p.stdout) if p.stdout else None

    def bind(self, **options):
        args = dict(board=self.board, runtime=self.url, session='ses_fixture', actor='reader')
        args.update(options)
        return [v for k, value in args.items() for v in ('--' + k, value)]

    def init(self):
        self.cli('init', '--allow-inbox-wake', *self.bind())

    def step(self):
        return self.cli('run', '--once')


class InboxTest(InboxCLI):
    def setUp(self):
        super().setUp()
        self.items = []
        self.owner = 'reader'
        self.redirect = False
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                url = urlsplit(self.path)
                q = parse_qs(url.query)
                if (url.path != '/v1/inbox' or self.headers['Authorization'] != 'Bearer board-fixture'
                        or q['read'] != ['false'] or q['limit'] != ['1']):
                    self.send_error(403)
                    return
                after = int(q['after'][0])
                fixture.cursors.append(after)
                if fixture.redirect:
                    self.send_response(302)
                    self.send_header('Location', fixture.board + '/redirect-target')
                    self.end_headers()
                    return
                page = dict(actor=fixture.owner, cursor=999, items=[i for i in fixture.items if i['id'] > after][:1])
                self.send_response(200)
                self.end_headers()
                self.wfile.write(json.dumps(page).encode())

            def log_message(self, *_):
                pass

        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        worker = threading.Thread(target=server.serve_forever)
        worker.start()
        self.addCleanup(server.server_close)
        self.addCleanup(worker.join)
        self.addCleanup(server.shutdown)
        self.board = 'http://127.0.0.1:' + str(server.server_port)

    def test_binding_and_authenticated_pages(self):
        self.cli('run', '--once', fails=True)
        self.cli('init', *self.bind(), fails=True)
        for bad in (dict(runtime='https://example.invalid'), dict(runtime=self.url+'/other'),
                    dict(runtime='http://user@localhost'), dict(session='ses_x/../other'),
                    dict(board='http://example.invalid'), dict(actor='local'), dict(actor='other'), dict(session='ses_missing')):
            self.cli('init', '--allow-inbox-wake', *self.bind(**bad), fails=True)
        self.assertFalse(self.db.exists())
        self.init()
        self.cli('init', '--allow-inbox-wake', *self.bind(), fails=True)
        self.owner = 'other'
        self.items = [item(21)]
        self.cli('run', '--once', fails=True)
        self.owner = 'reader'
        self.items[0]['channel'] = 'go to http://someone.example'
        self.cli('run', '--once', fails=True)
        self.assertEqual(self.cli('status')['watermark'], 0)
        self.assertFalse(self.posts)

    def test_redirect_does_not_forward_board_bearer(self):
        self.init()
        self.redirect = True
        self.assertIn('HTTP 302', self.cli('run', '--once', fails=True))
        self.assertEqual(self.cli('status')['reservations'], [])
        self.assertFalse(self.posts)

    def test_stable_busy_restart_and_unread_watermark(self):
        self.items = [item(31), item(55, 'completed')]
        self.init()
        self.post_status = 412
        busy = self.step()
        self.assertEqual(busy['status'], 'busy')
        self.post_status = 204
        first = self.step()
        self.assertEqual(first, dict(reservation=busy['reservation'], status='accepted'))
        self.assertEqual(self.step()['status'], 'accepted')
        self.assertEqual(self.step(), dict(watermark=55, status='empty'))
        self.assertEqual(self.cursors, [0, 0, 30, 31, 55])
        self.assertEqual(len(self.prompts), 2)
        self.assertIn(first['reservation'], self.prompts[0]['prompt'])
        for secret in ('private-payload', 'other/private-byline', self.board_token, 'neo-fixture'):
            self.assertNotIn(secret, str(self.prompts) + str(self.cli('status')))

    def test_busy_item_no_longer_visible(self):
        self.init()
        self.items = [item(31)]
        self.post_status = 412
        self.assertEqual(self.step()['status'], 'busy')
        self.items = [item(40)]
        self.assertEqual(self.step()['status'], 'withdrawn')
        self.post_status = 204
        self.assertEqual(self.step()['status'], 'accepted')
        self.assertEqual(len(self.posts), 2)

    def test_ambiguous_admission_requires_exact_reconciliation(self):
        self.init()
        self.items = [item(7), item(24)]
        self.post_status = 500
        self.cli('run', '--once', fails=True)
        row, = self.cli('status')['reservations']
        self.assertEqual(row['status'], 'uncertain')
        self.cli('resolve', 'wrong:7', 'confirmed', fails=True)
        self.cli('run', '--once', fails=True)
        self.assertEqual(len(self.posts), 1)
        self.cli('resolve', row['key'], 'confirmed')
        self.post_status = 204
        self.assertEqual(self.step()['status'], 'accepted')
        self.assertEqual(len(self.prompts), 2)

    def test_crash_after_post_and_competing_service(self):
        self.items = [item(7)]
        self.init()
        self.reply_gate.clear()
        with subprocess.Popen(self.command('run'), env=dict(self.env, NEOBOARD_INBOX_TOKEN=self.board_token),
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL) as p:
            try:
                self.assertTrue(self.received.wait(5))
                self.cli('run', '--once', fails=True)
                row, = self.cli('status')['reservations']
                self.assertEqual(row['status'], 'uncertain')
            finally:
                p.kill()
                p.wait(5)
                self.reply_gate.set()
        self.cli('run', '--once', fails=True)
        self.cli('resolve', row['key'], 'skip')
        self.assertEqual(self.step()['status'], 'empty')
        self.assertEqual(len(self.posts), 1)


@unittest.skipUnless(os.environ.get('NEOBOARD_TEST_BIN_DIR'), 'set NEOBOARD_TEST_BIN_DIR for disposable daemon test')
class InboxDaemonTest(InboxCLI):
    def test_only_recipient_or_subscriber_wakes(self):
        base = self.db.parent
        data = base / 'board'
        binary = Path(os.environ['NEOBOARD_TEST_BIN_DIR']).resolve() / 'neochatd'
        with subprocess.Popen([binary, '--bind', '127.0.0.1:0', '--data-dir', data],
                              cwd=base, env=dict(HOME=str(base), PATH=os.defpath, XDG_CONFIG_HOME=str(base/'config'),
                                                 XDG_DATA_HOME=str(base/'data')), stdout=subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL) as proc:
            try:
                deadline = time.monotonic() + 8
                while not (data/'endpoint.json').exists():
                    self.assertIsNone(proc.poll())
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(.02)
                self.board = json.loads((data/'endpoint.json').read_text())['url']
                admin = (data/'admin-token').read_text().strip()

                def api(route, credential, **body):
                    req = request.Request(self.board + '/v1/' + route, json.dumps(body).encode() if body else None,
                                          {'Authorization': 'Bearer ' + credential, 'Content-Type': 'application/json'})
                    with request.build_opener(request.ProxyHandler({})).open(req, timeout=5) as r:
                        return json.load(r)

                def issue(actor, grants):
                    return api('admin/restricted-invites', admin, invite=dict(actor=actor, channels=['general']), grants=grants)['token']

                self.board_token = issue('reader', ['read', 'notifications'])
                writer = issue('writer', ['read', 'post', 'reply'])
                self.init()
                root = api('actions', writer, type='post', channel='general', text='not a notification')['id']
                api('subscriptions', self.board_token, thread=root, kind='reply', author='writer')
                api('actions', writer, type='reply', thread=root, text='sensitive answer')
                api('actions', writer, type='post', channel='general', text='@reader private mention')
                page = api('inbox', self.board_token)
                self.assertEqual([i['kind'] for i in page['items']], ['reply', 'mention'])
                reader, self.board_token = self.board_token, writer
                self.cli('run', '--once', fails=True)  # another valid scoped credential
                self.assertFalse(self.posts)
                self.board_token = reader
                for i in page['items']:
                    r = self.step()
                    self.assertEqual(r['status'], 'accepted')
                    self.assertTrue(r['reservation'].endswith(':' + str(i['id'])))
                self.assertEqual(self.step()['status'], 'empty')
                self.assertEqual(api('inbox', reader), page)  # never consumed the desktop's read state
                self.assertEqual(len(self.posts), 2)
            finally:
                proc.terminate()
                proc.wait(timeout=5)


if __name__ == '__main__':
    unittest.main()