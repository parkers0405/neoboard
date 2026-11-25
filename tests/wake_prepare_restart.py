"""Compare a released CLI and a candidate after the selected local daemon changes ports."""
import argparse
from contextlib import ExitStack
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory

from paired_wake_drill import Board, environment


def check(old, new, daemon):
    with TemporaryDirectory() as tmp:
        root = Path(tmp)
        env = environment(root)

        def prepare(cli, url):
            return subprocess.run([cli, 'wake', 'prepare', '--board-id', 'disposable', '--harness', 'neoism',
                                   '--project', 'drill', '--local-data-dir', root/'board', '--local-endpoint', url+'/',
                                   '--create-agent'], env=env, capture_output=True, timeout=40)

        with ExitStack() as stack:
            first = Board(root, daemon, env, stack)
            assert prepare(old, first.url).returncode == 0
        # Boot the same data directory again. Board's helper waits for fresh discovery.
        (root/'board/endpoint.json').unlink(missing_ok=True)
        (root/'reviewer.neochat').unlink()
        with ExitStack() as stack:
            from paired_wake_drill import start, eventually, json
            start(stack, root, [daemon, '--bind', '127.0.0.1:0', '--data-dir', root/'board'], env)
            current = eventually(lambda: json.loads((root/'board/endpoint.json').read_text()))['url']
            assert current != first.url
            failed = prepare(old, current)
            assert failed.returncode != 0 and b'Board unreachable' in failed.stderr
            for _ in range(2):
                result = prepare(new, current)
                assert result.returncode == 0 and b'wake pair --code' in result.stdout
            state = json.loads((root/'wake/bindings.json').read_bytes())
            assert not state['bindings'] and len(state['pending']) == 1
            print('Previous CLI: stale inbox unreachable. Candidate: prepare and repeat succeeded; one pending request, no binding.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('old', 'new', 'daemon'):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    check(*(p.resolve() for p in (args.old, args.new, args.daemon)))