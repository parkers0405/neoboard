import hashlib
import hmac
import json
import tempfile
import unittest
from pathlib import Path

from scripts.github_bridge import Bridge


class BridgeTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.path = Path(tmp.name) / 'state'
        self.cfg = dict(repo_id=12, repo='org/project', channel='project/dev',
                        board_actor='bridge', github_actor_id=9)
        self.actions, self.events, self.writes = {}, [], []
        self.restart()

    def restart(self):
        if hasattr(self, 'b'):
            self.b.db.close()
        self.b = Bridge(self.path, self.cfg, self.board, self.github)
        self.addCleanup(self.b.db.close)

    def board(self, path, action=None, headers=None):
        if action is None:
            after = int(path.split('after=')[1].split('&')[0])
            return [e for e in self.events if e['cursor'] > after]
        self.assertEqual(path, '/v1/actions')
        key = headers['Idempotency-Key']
        self.assertEqual(action, self.actions.setdefault(key, action))
        return {'id': list(self.actions).index(key) + 100}

    def github(self, path, body):
        self.writes.append((path, body))
        return {'id': 501, 'number': 8}

    def hook(self, event='issue_comment', delivery='abc', *, actor=1, repo=12, bad=False):
        obj = dict(user={'id': actor}, body='hello', id=30)
        item = dict(obj, number=8, id=20, title='title')
        raw = json.dumps(dict(repository={'id': repo}, comment=obj, issue=item, pull_request=item,
                              action='created' if event.endswith('comment') else 'opened')).encode()
        sig = 'sha256=' + hmac.new(b'secret', raw, hashlib.sha256).hexdigest()
        return self.b.webhook(event, delivery, sig, raw + (b' ' if bad else b''), b'secret')

    def message(self, seq, mid, parent, actor='alice', **labels):
        self.events.append(dict(type='message', cursor=seq, actor=actor, message=dict(
            id=mid, parent=parent, channel=self.cfg['channel'], text='Board note', labels=labels)))

    def test_verify_before_writing(self):
        with self.assertRaisesRegex(ValueError, 'signature'):
            self.hook(bad=True)
        with self.assertRaisesRegex(ValueError, 'repository'):
            self.hook(repo=13)
        self.assertFalse(self.actions)
        self.assertEqual('imported', self.hook())

    def test_pr_out_of_order_and_replays(self):
        self.hook('pull_request_review_comment')
        self.restart()
        self.assertEqual('duplicate', self.hook('pull_request_review_comment'))
        self.assertEqual('duplicate', self.hook('pull_request_review_comment', 'new'))
        self.hook('pull_request', 'opened')
        self.hook(delivery='discussion')
        self.assertEqual(['post', 'reply', 'reply', 'reply'], [a['type'] for a in self.actions.values()])
        self.assertEqual({100}, {a['thread'] for a in self.actions.values() if 'thread' in a})

    def test_board_idempotency_after_lost_response(self):
        def lost(*args):
            response = self.board(*args)
            if response['id'] == 101:
                raise TimeoutError
            return response
        self.b.board = lost
        with self.assertRaises(TimeoutError):
            self.hook('issues')
        self.restart()
        self.hook('issues')
        self.assertEqual(2, len(self.actions))

    def test_echoes_and_repeat_upserts(self):
        self.hook('issues')
        self.message(4, 101, 100, 'bridge')
        self.message(7, 101, 100)  # later update by another actor
        self.message(8, 202, 100)
        self.message(11, 202, 100)
        self.message(12, 203, 99)  # unrelated thread
        self.b.poll()
        self.restart()
        self.b.poll()
        self.assertEqual(12, self.b.cursor())
        self.assertEqual(['/repos/org/project/issues/8/comments'], [w[0] for w in self.writes])
        self.assertEqual('echo', self.hook(delivery='returned', actor=9))

    def test_ambiguous_issue_then_reconcile(self):
        self.message(2, 50, None, github='issue')
        def lost(*args):
            self.github(*args)
            raise TimeoutError
        self.b.github = lost
        with self.assertRaises(TimeoutError):
            self.b.poll()
        self.restart()
        with self.assertRaisesRegex(RuntimeError, 'unresolved'):
            self.b.poll()
        with self.assertRaisesRegex(RuntimeError, 'in flight'):
            self.hook()
        self.assertEqual(0, self.b.cursor())
        self.b.finish(50, 8)
        self.b.poll()
        self.hook()
        self.assertEqual([{'type': 'reply', 'thread': 50, 'text': 'hello'}], list(self.actions.values()))
        self.assertEqual(['/repos/org/project/issues'], [w[0] for w in self.writes])

    def test_normal_board_issue_then_comment(self):
        self.message(1, 40, None)
        self.message(2, 50, None, github='issue')
        self.message(4, 51, 50)
        self.b.poll()
        self.assertEqual(['/repos/org/project/issues', '/repos/org/project/issues/8/comments'],
                         [w[0] for w in self.writes])
        self.assertEqual('Board note', self.writes[0][1]['title'])