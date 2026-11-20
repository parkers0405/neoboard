"""Exercise the shipped daemon and CLI without using a host board or publishing ports."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import uuid
import zipfile


def exercise(cli, restart):
    def ready():
        deadline = time.monotonic() + 30
        while True:
            try:
                cli('channels')
                return
            except (subprocess.CalledProcessError, FileNotFoundError) as error:
                if time.monotonic() >= deadline:
                    raise RuntimeError('daemon did not become ready') from error
                time.sleep(0.2)

    ready()
    marker = f'smoke-{uuid.uuid4().hex}'
    cli('post', 'smoke', marker)
    restart()
    ready()
    if marker not in cli('read', 'smoke'):
        raise RuntimeError('packaged CLI did not read the persisted message after restart')


def run(*cmd, **kwargs):
    return subprocess.run(cmd, check=True, timeout=30, capture_output=True, text=True, **kwargs).stdout


def smoke_zip(archive):
    with tempfile.TemporaryDirectory() as tmp, zipfile.ZipFile(archive) as bundle:
        root = Path(tmp).resolve()
        suffix = '.exe' if os.name == 'nt' else ''
        daemon, client = [root / (name + suffix) for name in ('neochatd', 'neochat')]
        for path in (daemon, client):
            info = bundle.getinfo(path.name)
            bundle.extract(info, root)
            if os.name != 'nt':
                path.chmod((info.external_attr >> 16) & 0o777)
        env = {k: v for k, v in os.environ.items() if not k.upper().startswith('NEOCHAT_')}
        env.update(NEOCHAT_DATA_DIR=str(root / 'data'), NEOCHAT_BIND='127.0.0.1:0',
                   HOME=tmp, USERPROFILE=tmp, APPDATA=tmp, LOCALAPPDATA=tmp,
                   XDG_DATA_HOME=tmp, XDG_CONFIG_HOME=tmp, NO_PROXY='*', no_proxy='*')

        def start():
            return subprocess.Popen([daemon], env=env, cwd=root)

        def stop():
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
                raise

        def restart():
            nonlocal proc
            stop()
            proc = start()

        def cli(*args):
            if proc.poll() is not None:
                raise RuntimeError(f'packaged daemon exited: {proc.returncode}')
            # Explicit endpoint disables the CLI's automatic daemon startup.
            endpoint = json.loads((root / 'data' / 'endpoint.json').read_text())
            return run(client, *args, cwd=root, env=dict(env, NEOCHAT_URL=endpoint['url'],
                                                        NEOCHAT_TOKEN=endpoint['token']))

        proc = start()
        try:
            exercise(cli, restart)
        finally:
            if proc.poll() is None:
                stop()


def smoke_image(image):
    # Docker populates the anonymous volume from the image's non-root data dir.
    cid = run('docker', 'create', '--network', 'none', '-v', '/var/lib/neochat', image).strip()
    try:
        run('docker', 'start', cid)
        def cli(*args):
            endpoint = json.loads(run('docker', 'exec', cid, 'cat', '/var/lib/neochat/endpoint.json'))
            return run('docker', 'exec', '-e', 'NEOCHAT_URL', '-e', 'NEOCHAT_TOKEN', cid,
                       'neochat', *args, env=dict(os.environ, NEOCHAT_URL=endpoint['url'],
                                                 NEOCHAT_TOKEN=endpoint['token']))

        exercise(cli, lambda: run('docker', 'restart', cid))
    finally:
        run('docker', 'rm', '-fv', cid)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('zip', 'image'))
    parser.add_argument('artifact')
    args = parser.parse_args()
    try:
        (smoke_zip if args.mode == 'zip' else smoke_image)(args.artifact)
    except subprocess.CalledProcessError as exc:
        raise SystemExit(f'{exc}: {exc.stderr}') from exc
    print('Packaged write/restart/read passed:', args.artifact)