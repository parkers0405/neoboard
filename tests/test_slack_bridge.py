import hashlib
import hmac
import json
from pathlib import Path
import tempfile
import unittest

from scripts.slack_bridge import Bridge, RateLimited


FIX = Path(__file__).with_name('test_slack_bridge_fixtures')
ROOT = '1700000000.000001'
REPLY = '1700000000.000002'


class BridgeTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.path = str(Path(tmp.name) / 'state.db')
        self.cfg = json.loads((FIX / 'binding.json').read_text())
        self.actions, self.events, self.sent = {}, [], []
        self.open()

    def open(self):
        self.b = Bridge(self.path, self.cfg, self.board, self.slack)
        self.addCleanup(self.b.db.close)

    def board(self, path, body=None, headers=None):
        if body is None:
            after = int(path.split('after=')[1].split('&')[0])
            return [e for e in self.events if e['cursor'] > after]
        self.assertEqual(path, '/v1/actions')
        key = headers['Idempotency-Key']
        self.assertEqual(self.actions.setdefault(key, body), body)
        return {'id': body.get('id', list(self.actions).index(key) + 100)}

    def slack(self, path, body):
        self.sent.append((path, body))
        return {'ok': True, 'channel': body['channel'], 'ts': body.get('ts', f'1800000000.{len(self.sent):06d}')}

    def enqueue(self, key='Ev1', fields=None, outer=None, bad=False, now=1000):
        payload = json.loads((FIX / 'message.json').read_text())
        payload.update(event_id=key, **(outer or {}))
        payload['event'].update(fields or {})
        raw = json.dumps(payload).encode()
        sig = 'v0=' + hmac.new(b's', b'v0:1000:' + raw, hashlib.sha256).hexdigest()
        return self.b.receive(raw, '1000', sig if not bad else 'v0=no', b's', now)

    def message(self, mid, text, parent=None, channel='project/dev', author='human', actor='human'):
        self.events.append(dict(type='message', cursor=len(self.events)*2+2, actor=actor,
                                message=dict(id=mid, text=text, parent=parent, channel=channel, author=author)))

    def test_auth_and_scope(self):
        for args in [dict(bad=True), dict(now=1301)]:
            with self.assertRaises(ValueError):
                self.enqueue(**args)
        for args in [dict(outer={'team_id':'OTHER'}), dict(outer={'api_app_id':'OTHER'}),
                     dict(fields={'channel':'DPRIVATE'}), dict(fields={'type':'reaction_added'})]:
            self.assertEqual('ignored', self.enqueue(**args))
        self.message(7, 'private', channel='project/dev/private')
        self.b.drain()
        self.b.poll()
        self.assertFalse(self.sent or self.actions)
        with self.assertRaisesRegex(ValueError, 'binding mismatch'):
            Bridge(self.path, dict(self.cfg, conversation='OTHER'), self.board, self.slack)

    def test_thread_order_edits_delete_and_replays(self):
        self.enqueue('reply', {'ts':REPLY, 'thread_ts':ROOT})
        self.enqueue()
        self.enqueue()
        self.b.drain()
        self.open()
        self.b.drain()
        edit = dict(subtype='message_changed', message=dict(ts=REPLY, user='U', text='new', edited={'ts':'1700000001.000003'}))
        self.enqueue('edited', edit)
        self.b.drain()
        edit['message']['edited']['ts'] = REPLY
        self.enqueue('old', edit)
        self.enqueue('deleted', dict(subtype='message_deleted', deleted_ts=REPLY))
        self.enqueue('replay', dict(ts=REPLY, thread_ts=ROOT))
        self.b.drain()
        self.assertEqual(list(self.actions.values()), [dict(type='post',channel='project/dev',text='original'),
            dict(type='reply',thread=100,text='original'),dict(type='edit',id=101,text='new'),
            dict(type='edit',id=101,text='[Deleted on Slack]')])

    def test_cross_direction_threads_and_echoes(self):
        self.enqueue()
        self.b.drain()
        self.message(100, 'import', author='slack-bridge')
        self.message(100, 'metadata event')  # mapped Slack origin even if actor differs
        self.message(7, '<!channel>', 100)
        self.message(7, '<!channel>', 100)
        self.message(7, 'edited', 100)
        self.message(8, 'board root')
        self.message(9, 'orphan', 1234)
        self.b.poll()
        self.assertEqual([p for p,_ in self.sent], ['/api/chat.postMessage','/api/chat.update','/api/chat.postMessage'])
        self.assertEqual(self.sent[0][1]['thread_ts'], ROOT)
        self.assertEqual(self.sent[0][1]['text'], '&lt;!channel&gt;')
        self.assertEqual(self.sent[1][1]['ts'], '1800000000.000001')
        self.enqueue('reply', dict(ts=REPLY, thread_ts='1800000000.000003'))
        self.enqueue('echo', dict(ts='1800000000.000001'))
        self.enqueue('bot', dict(user='UBRIDGE',ts='1700000004.000001'))
        self.b.drain()
        self.assertEqual(list(self.actions.values())[-1]['thread'], 8)
        self.assertEqual(len(self.actions), 2)
        self.events.append(dict(type='thread_deleted', actor='user', cursor=50, channel='project/dev', id=100))
        self.b.poll()
        self.assertEqual(self.sent[-1], ('/api/chat.delete', dict(channel='CTEST', ts='1800000000.000001')))
        self.assertEqual(len(self.sent), 4)  # not the human's original Slack root

    def test_retry_after_rate_limit_and_ambiguous_success(self):
        self.message(4, 'one')
        def limited(*args):
            raise RateLimited(120)
        self.b.slack = limited
        with self.assertRaises(RateLimited):
            self.b.poll()
        self.open()
        with self.assertRaises(RateLimited):
            self.b.poll()
        self.assertFalse(self.sent)
        with self.b.db:
            self.b.db.execute('UPDATE state SET resume=0')
        def ambiguous(*args):
            self.slack(*args)
            raise TimeoutError()
        self.b.slack = ambiguous
        with self.assertRaises(TimeoutError):
            self.b.poll()
        self.open()
        with self.assertRaisesRegex(RuntimeError, 'reconcile'):
            self.b.poll()
        self.b.reconcile(4, '1800000000.000001')
        self.b.poll()
        self.assertEqual(len(self.sent), 1)
        self.assertEqual(self.b.db.execute('SELECT cursor FROM state').fetchone(), (2,))

    def test_import_timeout_reuses_same_action_after_restart(self):
        self.enqueue()
        def ambiguous(*args):
            self.board(*args)
            raise TimeoutError()
        self.b.board = ambiguous
        with self.assertRaises(TimeoutError):
            self.b.drain()
        self.open()
        self.b.drain()
        self.assertEqual(len(self.actions), 1)
        self.assertEqual(self.b.db.execute('SELECT mid,ts FROM msg').fetchall(), [(100, ROOT)])

    def test_token_identity(self):
        for team, user in [('wrong','UBRIDGE'), ('TTEST','wrong')]:
            self.b.slack = lambda path, body: dict(ok=True, team_id=team, user_id=user)
            with self.assertRaisesRegex(ValueError, 'token does not match'):
                self.b.verify_bot()
        self.b.slack = lambda path, body: dict(ok=True, team_id='TTEST', user_id='UBRIDGE')
        self.b.verify_bot()

    def test_retry_partially_deleted_thread(self):
        self.message(4, 'root')
        self.message(5, 'child', 4)
        self.b.poll()
        self.events.append(dict(type='thread_deleted',actor='human',channel='project/dev',id=4,cursor=12))
        replies = [dict(ok=True, channel='CTEST', ts='1800000000.000002'), TimeoutError(),
                   dict(ok=False,error='message_not_found'), dict(ok=True, channel='CTEST', ts='1800000000.000001')]
        def delete(path, body):
            self.assertEqual(path, '/api/chat.delete')
            r = replies.pop(0)
            if isinstance(r, Exception):
                raise r
            return r
        self.b.slack = delete
        with self.assertRaises(TimeoutError):
            self.b.poll()
        self.b.poll()
        self.assertFalse(replies)
        self.assertEqual(self.b.db.execute('SELECT count(*) FROM msg').fetchone(), (0,))