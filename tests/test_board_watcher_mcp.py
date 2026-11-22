"""End-to-end reply marker contract with the shipped stdio MCP and a disposable board."""

import json
import os
from pathlib import Path
import subprocess
import time
import unittest
from urllib import request

from test_board_watcher import WatcherFixture


@unittest.skipUnless(os.environ.get('NEOBOARD_TEST_BIN_DIR'), 'set NEOBOARD_TEST_BIN_DIR to the offline build directory')
class ShippedMcp(WatcherFixture):
    def test_two_turns_and_result(self):
        bins = Path(os.environ['NEOBOARD_TEST_BIN_DIR']).resolve()
        base = self.db.parent
        env = dict(HOME=str(base), PATH=os.defpath, XDG_CONFIG_HOME=str(base/'config'), XDG_DATA_HOME=str(base/'data'))
        board = base/'board'
        daemon = subprocess.Popen([bins/'neochatd', '--bind', '127.0.0.1:0', '--data-dir', board], env=env, cwd=base,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        def stop():
            daemon.terminate()
            daemon.wait(timeout=5)
        self.addCleanup(stop)
        deadline = time.monotonic()+8
        while not (board/'endpoint.json').exists():
            self.assertIsNone(daemon.poll())
            self.assertLess(time.monotonic(), deadline)
            time.sleep(.02)
        endpoint = json.loads((board/'endpoint.json').read_text())
        admin = (board/'admin-token').read_text().strip()
        def issue(actor, grants):
            body = dict(invite=dict(actor=actor,channels=['general']), grants=grants)
            req = request.Request(endpoint['url']+'/v1/admin/restricted-invites',json.dumps(body).encode(),
                                  {'Authorization':'Bearer '+admin, 'Content-Type':'application/json'})
            with request.build_opener(request.ProxyHandler({})).open(req, timeout=5) as response:
                return json.load(response)['token']

        worker = issue('worker', ['read','claim','start','renew','complete','reply'])
        human = issue('human', ['read','reply','post_task'])
        self.watch_token = issue('watch', ['read'])

        def tool(token, name, **arguments):
            messages = [dict(id=1,method='initialize',params=dict(protocolVersion='2025-06-18',capabilities={},
                             clientInfo=dict(name='watcher-fixture',version='1'))),dict(method='notifications/initialized'),
                        dict(id=2,method='tools/call',params=dict(name=name,arguments=arguments))]
            output = subprocess.check_output([bins/'neochat-mcp'], env=dict(env, NEOCHAT_URL=endpoint['url'], NEOCHAT_TOKEN=token),
                                             cwd=base, timeout=8, text=True,
                                             input=''.join(json.dumps(dict(jsonrpc='2.0',**m))+'\n' for m in messages))
            result = json.loads(output.splitlines()[-1])['result']
            self.assertFalse(result['isError'], result)
            return result['structuredContent']

        self.cli('init', '--endpoint', endpoint['url'], '--match', 'assignment', '--channel', 'general')
        self.cli('bind', '--actor', 'worker', '--answer-actor', 'human', '--endpoint', self.url, '--session', 'ses_fixture')
        task = tool(human, 'tasks_create', channel='general', title='Choose a color', assignee='worker')['id']
        self.assertEqual(self.cli('run', '--once')['wake']['status'], 'accepted')
        tool(worker, 'tasks_claim', id=task, lease_secs=300)
        tool(worker, 'tasks_start', id=task)
        qid = tool(worker, 'reply', thread=task, text='Which color?', labels={'watcher_kind':'pending_question'})['id']
        self.assertIsNone(self.cli('run', '--once')['wake'])
        self.assertEqual(self.cli('question-status')[0]['status'], 'pending')
        aid = tool(human, 'reply', thread=task, text='Cobalt.', labels={'watcher_kind':'answer','watcher_question':str(qid)})['id']
        self.assertEqual(self.cli('run', '--once')['wake']['status'], 'accepted')
        first, second = self.prompts
        self.assertIn(f'task #{task}', first['prompt'])
        self.assertIn(f'question #{qid} and answer #{aid}', second['prompt'])
        self.assertNotIn('Cobalt', second['prompt'])
        tool(worker, 'tasks_complete', id=task, text='Selected cobalt.')
        thread = tool(worker, 'read', channel='general', thread=task)['messages']
        self.assertEqual(thread[0]['task']['state'], 'completed')
        self.assertEqual([m['text'] for m in thread[1:]], ['Which color?', 'Cobalt.', 'Selected cobalt.'])
        self.assertIsNone(self.cli('run', '--once')['wake'])
        self.assertEqual([r['status'] for r in self.cli('wake-status')], ['accepted','accepted'])


if __name__ == '__main__':
    unittest.main()