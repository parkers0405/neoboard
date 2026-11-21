import hashlib
import hmac
import json
import tempfile
import unittest
from pathlib import Path

from scripts.linear_bridge import Bridge


ISSUE = '11111111-1111-4111-8111-111111111111'
COMMENT = '22222222-2222-4222-8222-222222222222'
STATE = '33333333-3333-4333-8333-333333333333'


class Fixture(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.path = Path(tmp.name) / 'bridge.sqlite'
        self.cfg = dict(organization_id='fixture', channel='test/dev', board_actor='bridge')
        self.actions, self.events, self.writes = {}, [], []
        self.lost_board = self.lost_linear = False
        self.restart()

    def restart(self):
        if hasattr(self, 'b'):
            self.b.db.close()
        self.b = Bridge(self.path, self.cfg, self.board, self.linear)
        self.addCleanup(self.b.db.close)

    def board(self, path, body=None, headers=None):
        if body is None:
            cursor = int(path.split('after=')[1].split('&')[0])
            return [e for e in self.events if e['cursor'] > cursor]
        self.assertEqual(path, '/v1/actions')
        key = headers['Idempotency-Key']
        self.assertEqual(self.actions.setdefault(key, body), body)
        if self.lost_board and body['type'] == 'reply_with_labels':
            self.lost_board = False
            raise TimeoutError()
        return {'id': list(self.actions).index(key) + 1}

    def linear(self, path, body):
        self.assertEqual(path, '/graphql')
        self.writes.append(body)
        if self.lost_linear:
            raise TimeoutError()
        field = 'commentCreate' if 'input' in body['variables'] else 'issueUpdate'
        return {'data': {field: {'success': True}}}

    def send(self, delivery, kind, data, action='create', org='fixture', bad=False):
        raw = json.dumps(dict(organizationId=org, type=kind, action=action, data=data)).encode()
        sig = hmac.new(b'secret', raw, hashlib.sha256).hexdigest()
        return self.b.webhook(delivery, sig, raw + (b' ' if bad else b''), b'secret')

    def comment(self, delivery='c', cid=COMMENT):
        return self.send(delivery, 'Comment', dict(id=cid, issueId=ISSUE, body='Linear reply'))

    def issue(self, delivery='i', day=1, **extra):
        return self.send(delivery, 'Issue', dict(id=ISSUE, team={'key': 'EX'}, number=7,
                         stateId=STATE, state={'name': 'In Progress'}, title='Fix',
                         updatedAt=f'2026-01-{day:02}T12:30:00Z', **extra), action='update')

    def event(self, mid, text, parent=1, actor='alice', channel='test/dev'):
        self.events.append(dict(cursor=5 + len(self.events)*3, type='message', actor=actor,
                                message=dict(id=mid, parent=parent, channel=channel, text=text)))

    def test_comment_first_and_ambiguous_board_response(self):
        self.lost_board = True
        with self.assertRaises(TimeoutError):
            self.comment()
        self.restart()
        self.assertEqual(self.comment(), 'imported')
        self.assertEqual(self.comment(), 'duplicate')
        self.assertEqual(self.comment('redelivery'), 'duplicate')
        self.issue(day=2)
        self.assertEqual(self.issue('old'), 'stale')
        self.restart()
        self.issue('move', 3, identifier='NEW-99')
        root, reply, edit, moved = self.actions.values()
        self.assertEqual(root['labels'], {'linear_id': ISSUE})
        self.assertEqual(reply['labels'], {'linear_comment_id': COMMENT})
        self.assertEqual((reply['thread'], edit['id'], moved['id']), (1, 1, 1))
        self.assertIn('EX-7: Fix\nState: In Progress (' + STATE + ')', edit['text'])
        self.assertIn('NEW-99', moved['text'])

    def test_security_and_collision(self):
        for options in ({'org': 'other'}, {'bad': True}):
            with self.assertRaises(ValueError):
                self.send('c', 'Issue', {}, **options)
        self.assertFalse(self.actions)
        self.comment()
        with self.assertRaisesRegex(ValueError, 'reused'):
            self.issue('c')
        with self.assertRaisesRegex(ValueError, 'binding'):
            Bridge(self.path, dict(self.cfg, channel='other'), self.board, self.linear)

    def test_state_transition_overwrites_snapshot_not_identity(self):
        self.issue()
        done = dict(id=ISSUE, identifier='EX-7', title='Fix', state={'name': 'Done', 'id': COMMENT},
                    updatedAt='2026-01-01T13:00:00+00:00')
        self.send('s', 'Issue', done, action='update')
        self.assertEqual(self.issue('late'), 'stale')
        self.assertEqual(self.send('s2', 'Issue', done, action='update'), 'stale')
        root, before, after = self.actions.values()
        self.assertEqual(root['labels']['linear_id'], ISSUE)
        self.assertEqual(before['id'], after['id'])
        self.assertIn('State: Done (' + COMMENT + ')', after['text'])

    def test_bidirectional_no_echo_and_gapped_cursor(self):
        self.comment()
        self.event(2, 'imported', actor='bridge')
        self.event(2, 'updated imported')
        self.event(3, 'wrong channel', channel='other')
        self.event(4, 'wrong thread', parent=99)
        self.event(5, 'board comment')
        self.event(5, 'edit')
        self.event(6, '!linear-state ' + STATE)
        self.b.poll()
        self.restart()
        self.b.poll()
        comment, state = self.writes
        args = comment['variables']['input']
        self.assertEqual((args['body'], args['issueId']), ('board comment', ISSUE))
        self.assertEqual(state['variables'], {'issue': ISSUE, 'id': STATE})
        self.assertEqual(self.comment('echo', args['id']), 'duplicate')

    def test_uncertain_exports_stop_until_reconciled(self):
        self.issue()
        self.event(10, 'first')
        self.event(11, 'next')
        self.lost_linear = True
        with self.assertRaises(TimeoutError):
            self.b.poll()
        self.restart()
        with self.assertRaisesRegex(RuntimeError, 'pending export 10'):
            self.b.poll()
        self.assertEqual(len(self.writes), 1)
        self.b.reconcile(10, True)
        with self.assertRaises(TimeoutError):
            self.b.poll()
        self.b.reconcile(11, False)
        self.lost_linear = False
        self.b.poll()
        self.assertEqual(self.writes[1:], [self.writes[1]]*2)