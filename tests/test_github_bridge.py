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
        if path == '/v1/snapshot':
            latest = {e['message']['id']: e['message'] for e in self.events if e['type'] == 'message'}
            return {'messages': list(latest.values())}
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

    def hook(self, event='issue_comment', delivery='abc', *, actor=1, repo=12, bad=False,
             body='hello', number=8, oid=30, action=None, sender=None):
        obj = dict(user={'id': actor}, body=body, id=oid)
        item = dict(obj, number=number, id=20 if event.endswith('comment') else oid, title='title')
        payload = dict(repository={'id': repo}, sender={'id': actor if sender is None else sender},
                       action=action or ('created' if event.endswith('comment') else 'opened'))
        payload['issue' if event in ('issues', 'issue_comment') else 'pull_request'] = item
        if event.endswith('comment'):
            payload['comment'] = obj
        raw = json.dumps(payload).encode()
        sig = 'sha256=' + hmac.new(b'secret', raw, hashlib.sha256).hexdigest()
        return self.b.webhook(event, delivery, sig, raw + (b' ' if bad else b''), b'secret')

    def message(self, seq, mid, parent, actor='alice', *, task=None, text='Board note', **labels):
        self.events.append(dict(type='message', cursor=seq, actor=actor, message=dict(
            id=mid, parent=parent, channel=self.cfg['channel'], task=task, text=text, labels=labels)))

    def question_thread(self, enabled=True, task=True, poll=True):
        if enabled:
            self.path = self.path.with_name('opt-in')
            self.cfg.update(answer_actor_ids=[1, 9])  # even an allowlisted bridge echo is ignored
            self.restart()
        self.message(1, 50, None, task={'state': 'in_progress'} if task else None, github='issue')
        self.message(2, 51, 50, watcher_kind='pending_question')
        self.message(3, 52, 50, text='watcher_kind=pending_question <!-- neoboard:51 -->')
        if poll:
            self.b.poll()

    def test_signed_answer_contract_completion_and_replay(self):
        self.question_thread()
        self.assertIn('Board question reply ID: `51`', self.writes[1][1]['body'])
        self.restart()
        body = '/neoboard-answer 51\r\nUse blue.\nSecond line.'
        self.assertEqual('imported', self.hook(body=body))
        self.assertEqual([dict(type='reply_with_labels', thread=50, text='Use blue.\nSecond line.', labels={
            'watcher_kind': 'answer', 'watcher_question': '51', 'watcher_route': 'repo:12'})], list(self.actions.values()))
        self.restart()
        self.assertEqual('duplicate', self.hook(body=body))
        self.assertEqual('duplicate', self.hook(delivery='again', body=body))
        self.assertEqual('echo', self.hook(delivery='bot', actor=9, body=body))
        self.message(4, 100, 50, 'bridge', watcher_kind='answer')
        # tasks_complete atomically upserts the task root, then inserts a plain result reply.
        self.message(5, 50, None, task={'state': 'completed', 'result': 'Shipped blue.'}, github='issue')
        self.message(6, 53, 50, text='Shipped blue.')
        self.message(7, 53, 50, text='Shipped blue.')
        self.b.poll()
        self.restart()
        self.b.poll()
        self.assertEqual([('/repos/org/project/issues/8/comments', {'body': 'Shipped blue.\n\n<!-- neoboard:53 -->'})], self.writes[3:])

    def test_no_answer_by_default_or_for_non_task(self):
        for enabled, task in [(False, True), (True, False)]:
            with self.subTest(enabled=enabled, task=task):
                self.question_thread(enabled=enabled, task=task)
                self.hook(body='/neoboard-answer 51\nYes')
                self.assertEqual('reply', list(self.actions.values())[-1]['type'])
                self.actions.clear()
                self.events.clear()

    def test_signed_comments_without_proof_are_not_answers(self):
        self.question_thread()
        self.hook('issues', 'other-issue-opened', number=7, oid=19)
        valid = '/neoboard-answer 51\nYes'
        cases = [dict(body=valid, actor=2), dict(body=valid, number=7), dict(body=valid, sender=2),
                 dict(body=valid, number=6),  # unmapped issue
                 dict(body=valid, event='pull_request_review_comment'), dict(body=valid, event='issues'),
                 dict(body='<!-- neoboard:51 -->\n' + valid), dict(body=' ' + valid),
                 dict(body='/neoboard-answer 51\n \t'), dict(body='/neoboard-answer 52\nYes'),
                 dict(body='/neoboard-answer 50\nYes'), dict(body='/neoboard-answer 404\nYes'),
                 dict(body='/neoboard-answer 051\nYes'), dict(body='/neoboard-answer ５１\nYes'),
                 dict(body='/neoboard-answer 9223372036854775808\nYes')]
        for n, case in enumerate(cases):
            with self.subTest(case=case):
                self.assertEqual('imported', self.hook(delivery=str(n), oid=200+n, **case))
                self.assertEqual('reply', list(self.actions.values())[-1]['type'])
        self.assertEqual('ignored', self.hook(action='edited', body=valid))
        with self.assertRaisesRegex(ValueError, 'signature'):
            self.hook(bad=True, body=valid)
        with self.assertRaisesRegex(ValueError, 'repository'):
            self.hook(repo=13, body=valid)

    def test_export_and_current_question_metadata_both_required(self):
        self.question_thread()
        self.message(4, 51, 50)  # mark withdrawn after export, before next poll
        self.message(5, 52, 50, watcher_kind='pending_question')  # relabel of ordinary export
        self.message(6, 53, 50, watcher_kind='pending_question')  # not yet exported
        for mid in (51, 52, 53):
            self.hook(delivery=str(mid), oid=mid, body=f'/neoboard-answer {mid}\nOK')
            self.assertEqual('reply', list(self.actions.values())[-1]['type'])

    def test_lost_answer_response_preserves_original_action(self):
        self.question_thread()
        def lost(*args):
            response = self.board(*args)
            if args[0] == '/v1/actions':
                raise TimeoutError
            return response
        self.b.board = lost
        body = '/neoboard-answer 51\nYes'
        with self.assertRaises(TimeoutError):
            self.hook(body=body)
        self.message(4, 51, 50)  # reclassifying now would conflict at the board
        self.restart()
        self.assertEqual('imported', self.hook(body=body))
        self.assertEqual(['reply_with_labels'], [a['type'] for a in self.actions.values()])

    def test_ambiguous_question_export_and_lost_ordinary_import(self):
        self.question_thread(poll=False)
        def lost_comment(path, body):
            result = self.github(path, body)
            if path.endswith('/comments'):
                raise TimeoutError
            return result
        self.b.github = lost_comment
        with self.assertRaises(TimeoutError):
            self.b.poll()

        body = '/neoboard-answer 51\nYes'
        def lost_board(*args):
            result = self.board(*args)
            if args[0] == '/v1/actions':
                raise TimeoutError
            return result
        self.b.board = lost_board
        with self.assertRaises(TimeoutError):
            self.hook(body=body)
        self.restart()
        self.b.finish(51, 501)  # operator independently confirmed the GitHub write
        self.b.poll()
        self.assertEqual('imported', self.hook(body=body))
        self.assertEqual('imported', self.hook(delivery='fresh', oid=31, body=body))
        self.assertEqual(['reply', 'reply_with_labels'], [a['type'] for a in self.actions.values()])
        self.assertEqual(1, sum('Board question reply ID:' in w[1]['body'] for w in self.writes))

    def test_answer_allowlist_rejects_non_integer_ids(self):
        for bad in ('1', None, [0], [True], ['1'], [1.0]):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                Bridge(':memory:', dict(self.cfg, answer_actor_ids=bad), self.board, self.github)

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