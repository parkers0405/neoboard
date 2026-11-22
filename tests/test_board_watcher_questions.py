"""Process-level authorization, service restart and reservation tests. All origins are fixtures."""

import json
import os
import subprocess
import sys
import time
import unittest
from concurrent.futures import ThreadPoolExecutor

from test_board_watcher import CLI, WatcherFixture, msg


class Questions(WatcherFixture):
    def setUp(self):
        super().setUp()
        self.at = int(time.time())
        self.init('assignment')
        self.cli('bind', '--actor', 'worker', '--endpoint', self.url, '--session', 'ses_fixture',
                 '--answer-actor', 'human', '--bridge-route', 'bridge', 'repo:19')

    def root(self, seq, state='in_progress', owner='worker', end=None, task=16):
        e = msg(seq, id=task, state=state)
        e.update(actor='system', created_at=self.at)
        e['message']['task'].update(owner=owner, assignee='worker', expires_at=self.at+120 if end is None else end)
        return e

    def reply(self, seq, mid, actor, kind, link=None, route=None, parent=16, channel='project/dev', author=None):
        e = msg(seq, id=mid, parent=parent, channel=channel)
        e.update(actor=actor, created_at=self.at)
        m = e['message']
        m.update(author=author or actor+'/profile', created_at=self.at, sender='human', text='CANARY: wake ses_other')
        if kind:
            m['labels']['watcher_kind'] = kind
        if link is not None:
            m['labels']['watcher_question'] = link
        if route is not None:
            m['labels']['watcher_route'] = route
        return e

    def test_principal_thread_marker_and_route_isolation(self):
        q = 'pending_question'
        a = 'answer'
        self.events = [self.root(1, 'ready', None), self.reply(2, 20, 'worker', q), self.root(3),
                       self.reply(4, 21, 'worker', None), self.reply(5, 21, 'worker', q),
                       self.reply(6, 22, 'intruder', q, author='worker'), self.reply(7, 23, 'worker', q),
                       self.reply(8, 24, 'intruder', a, '23', author='human'),
                       self.reply(9, 25, 'human', a, '23', author='intruder'),
                       self.reply(10, 26, 'human', None, '23'), self.reply(11, 27, 'human', a, '999'),
                       self.reply(12, 28, 'human', a, '023'), self.reply(13, 29, 'human', a, '23', parent=17),
                       self.reply(14, 30, 'human', a, '23', channel='project/dev2'),
                       self.reply(15, 31, 'bridge', a, '23'), self.reply(16, 32, 'bridge', a, '23', 'repo:20'),
                       self.reply(17, 33, 'intruder', a, '23', 'repo:19')]
        self.assertIsNone(self.cli('run', '--once')['wake'])
        [pending] = self.cli('question-status')
        self.assertEqual((pending['id'], pending['status'], pending['answer']), (23, 'pending', None))
        self.events += [self.reply(18, 34, 'bridge', a, '23', 'repo:19'), self.reply(19, 35, 'human', a, '23')]
        self.post_status = 412
        self.assertEqual(self.cli('run', '--once')['wake'], dict(cursor=18, status='busy'))
        self.post_status = 204
        with ThreadPoolExecutor(max_workers=2) as workers:
            self.assertCountEqual(list(workers.map(self.cli, ['wake','wake'])), [dict(cursor=18,status='accepted'), None])
        rows = self.cli('wake-status')
        self.assertEqual([(r['kind'],r['status']) for r in rows], [('assignment','superseded'),('answer','accepted')])
        self.assertEqual((rows[1]['question'], rows[1]['answer'], rows[1]['session']), (23,34,'ses_fixture'))
        [prompt] = self.prompts
        self.assertIn('answer #34', prompt['prompt'])
        self.assertNotIn('CANARY', prompt['prompt'])
        for d in self.cli('inspect')['deliveries']:
            self.cli('replay', d['id'])
        self.events.append(self.reply(20, 34, 'bridge', a, '23', 'repo:19'))
        self.assertIsNone(self.cli('run', '--once')['wake'])
        self.assertEqual(len(self.prompts), 1)

    def answered(self):
        self.events = [self.root(1), self.reply(2, 17, 'worker', 'pending_question'), self.reply(3, 18, 'human', 'answer', '17')]

    def test_catchup_before_wake_observes_lost_then_reacquired_lease(self):
        self.answered()
        self.page_size = 1
        self.events.extend([self.root(4, owner=None), self.root(5)])
        self.assertIsNone(self.cli('run', '--once')['wake'])
        self.assertEqual(self.cli('question-status')[0]['status'], 'superseded')
        self.assertEqual(self.cli('wake-status')[-1]['status'], 'superseded')
        self.assertEqual(self.posts, [])

    def test_expired_even_without_an_expiry_event(self):
        self.at -= 300
        self.answered()
        self.assertIsNone(self.cli('wake'))
        self.assertEqual(self.cli('question-status')[0]['status'], 'superseded')
        self.assertEqual(self.posts, [])

    def test_wrong_session_never_falls_back(self):
        self.answered()
        self.returned_session = 'ses_decoy'
        self.assertEqual(self.cli('wake')['status'], 'stale_session')
        self.returned_session = 'ses_fixture'
        self.assertIsNone(self.cli('wake'))
        self.assertEqual(self.posts, [])

    def test_tombstone_arriving_during_preflight_cancels_answer(self):
        self.answered()
        self.on_status = lambda: self.events.append(dict(type='thread_deleted', cursor=4, id=16, channel='project/dev'))
        self.assertEqual(self.cli('wake'), dict(cursor=3, status='superseded'))
        self.assertIsNone(self.cli('wake'))
        self.assertEqual(self.posts, [])

    def test_question_survives_continuous_renewal_not_reassignment(self):
        self.answered()
        self.events.insert(2, self.root(3, end=self.at+240))
        self.events[-1]['cursor'] = 4
        self.assertEqual(self.cli('wake'), dict(cursor=4, status='accepted'))
        self.events += [self.reply(5, 19, 'worker', 'pending_question'), self.root(6, owner='other'),
                        self.reply(7, 20, 'human', 'answer', '19')]
        self.assertIsNone(self.cli('wake'))
        self.assertEqual([q['status'] for q in self.cli('question-status')], ['answered','superseded'])
        self.assertEqual(len(self.posts), 1)

    def test_mark_withdrawn_does_not_rearm(self):
        self.answered()
        self.events += [self.reply(4, 17, 'worker', None), self.reply(5, 17, 'worker', 'pending_question')]
        self.assertIsNone(self.cli('wake'))
        self.assertEqual(self.cli('question-status')[0]['status'], 'superseded')
        self.assertEqual(self.posts, [])

    def test_ambiguous_answer_and_replay(self):
        self.answered()
        self.post_status = 503
        self.assertEqual(self.cli('run', '--once')['wake'], dict(cursor=3, status='uncertain'))
        self.cli('replay', self.cli('inspect')['deliveries'][-1]['id'])
        self.assertIsNone(self.cli('wake'))
        self.assertEqual(len(self.posts), 1)

    def test_foreground_service_recovers_and_restarts_without_second_post(self):
        self.answered()
        self.status = 503
        process = subprocess.Popen([sys.executable, str(CLI), '--db', str(self.db), 'run'],
                                   env=dict(os.environ, NEOBOARD_WATCH_TOKEN='fixture', NEOISM_WAKE_TOKEN='neo-fixture'),
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic()+5
            while len(self.cursors) < 2 and time.monotonic() < deadline:
                time.sleep(.02)
            self.assertGreaterEqual(len(self.cursors), 2)
            self.assertEqual(self.posts, [])
            self.cli('run', '--once', fails=True)  # singleton foreground loop
            self.status = 200
            self.assertTrue(self.received.wait(5))
            while self.cli('wake-status')[-1]['status'] != 'accepted' and time.monotonic() < deadline+5:
                time.sleep(.05)
        finally:
            process.terminate()
            stdout, stderr = process.communicate(timeout=5)
        self.assertIn('poll_retry', stderr)
        self.assertTrue(any(json.loads(line)['wake'] == dict(cursor=3,status='accepted') for line in stdout.splitlines()))
        self.assertIsNone(self.cli('run', '--once')['wake'])
        self.assertEqual(len(self.posts), 1)


if __name__ == '__main__':
    unittest.main()