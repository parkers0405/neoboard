import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


HERE = Path(__file__).resolve().parent
SCRIPT = HERE.parent / 'scripts' / 'runtime_wake.py'
IDS = {name: ('00000000-0000-4000-8000-000000000001', '00000000-0000-4000-8000-000000000002')
       for name in ('claude', 'codex')}
IDS['opencode'] = ('ses_abc', 'ses_def')


class WakeTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name)
        for runtime, sessions in IDS.items():
            cli = self.home / runtime
            cli.write_text('#!' + sys.executable + '\n' + (HERE / 'fixtures/wake_runtime.py').read_text())
            cli.chmod(0o700)
            for session in sessions:
                (self.home / session).touch()

    def run_cli(self, runtime, session, *args, wrong=None, completed_turn=None):
        env = {'HOME': str(self.home), 'PATH': str(self.home)}
        if wrong:
            env['WRONG_ID'] = wrong
        if completed_turn:
            env['COMPLETED_TURN'] = completed_turn
        return subprocess.run([sys.executable, '-B', SCRIPT, runtime, '--session', session, '--task', '17', *args],
                              cwd=self.home, env=env, capture_output=True, text=True, timeout=10)

    def test_explicit_target(self):
        for runtime, (other, target) in IDS.items():
            with self.subTest(runtime=runtime):
                path = self.home / target
                path.write_text('')
                result = self.run_cli(runtime, target, '--allow-start')
                self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
                self.assertEqual(json.loads(result.stdout)['status'], 'delivered')
                self.assertEqual(len(path.read_text().splitlines()), 1)
                self.assertEqual((self.home / other).read_text(), '')

    def test_refused_before_launch(self):
        for runtime, (_, target) in IDS.items():
            result = self.run_cli(runtime, target)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(json.loads(result.stdout)['status'], 'consent_required')
            self.assertNotEqual(self.run_cli(runtime, 'latest', '--allow-start').returncode, 0)
        self.assertFalse((self.home / 'launches').exists())

    def test_no_success_on_identity_mismatch(self):
        for runtime, (other, target) in IDS.items():
            (self.home / target).write_text('')
            result = self.run_cli(runtime, target, '--allow-start', wrong=other)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(json.loads(result.stdout)['status'], 'uncertain')
            if runtime == 'codex':
                self.assertEqual((self.home / target).read_text(), '')

    def test_codex_completion_must_match_started_turn(self):
        result = self.run_cli('codex', IDS['codex'][1], '--allow-start', completed_turn='another-turn')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(json.loads(result.stdout)['status'], 'uncertain')

    def test_missing_session_no_fallback(self):
        for runtime, (other, target) in IDS.items():
            (self.home / target).unlink(missing_ok=True)
            result = self.run_cli(runtime, target, '--allow-start')
            self.assertEqual(json.loads(result.stdout)['status'], 'uncertain')
            self.assertFalse((self.home / target).exists())
            self.assertEqual((self.home / other).read_text(), '')


if __name__ == '__main__':
    unittest.main()