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


def check_windows_subsystems(bundle):
    # Check the linker output, not the Rust source. The PE32/PE32+ optional
    # headers both put Subsystem at offset 68, after the signature + COFF header.
    for name in ('neochat-desktop', 'neoboard', 'neochat', 'neochatd', 'neochat-mcp', 'neochat-share'):
        with bundle.open(name + '.exe') as exe:
            if exe.read(2) != b'MZ':
                raise ValueError(f'{name}: missing DOS header')
            exe.seek(0x3c)
            pe = int.from_bytes(exe.read(4), 'little')
            exe.seek(pe)
            if exe.read(4) != b'PE\0\0':
                raise ValueError(f'{name}: missing PE header')
            exe.seek(pe + 20)
            size = int.from_bytes(exe.read(2), 'little')  # SizeOfOptionalHeader
            exe.seek(pe + 24)
            if size < 70 or exe.read(2) not in (b'\x0b\x01', b'\x0b\x02'):
                raise ValueError(f'{name}: invalid PE optional header')
            exe.seek(pe + 24 + 68)
            field = exe.read(2)
            if len(field) != 2:
                raise ValueError(f'{name}: truncated PE subsystem')
            actual = int.from_bytes(field, 'little')
            expected = 2 if name == 'neochat-desktop' else 3  # WINDOWS_GUI / WINDOWS_CUI
            if actual != expected:
                raise ValueError(f'{name}: expected PE subsystem {expected}, got {actual}')


def smoke_zip(archive):
    with tempfile.TemporaryDirectory() as tmp, zipfile.ZipFile(archive) as bundle:
        root = Path(tmp).resolve()
        suffix = '.exe' if os.name == 'nt' else ''
        if os.name == 'nt':
            check_windows_subsystems(bundle)
            desktop = bundle.extract('neochat-desktop.exe', root)
            help_text = subprocess.run([desktop, '--help'], check=True, capture_output=True, timeout=30).stdout
            if b'--data-dir' not in help_text:
                raise RuntimeError('GUI desktop lost redirected --help output')
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
    parser.add_argument('mode', choices=('zip', 'image', 'windows-pe'),
                        help='windows-pe checks the Windows ZIP on any host without running a binary')
    parser.add_argument('artifact')
    args = parser.parse_args()
    if args.mode == 'windows-pe':
        with zipfile.ZipFile(args.artifact) as bundle:
            check_windows_subsystems(bundle)
        print('Verified Windows PE subsystems: desktop GUI (2); all five console tools CUI (3)')
        raise SystemExit(0)
    try:
        (smoke_zip if args.mode == 'zip' else smoke_image)(args.artifact)
    except subprocess.CalledProcessError as exc:
        raise SystemExit(f'{exc}: {exc.stderr}') from exc
    print('Packaged write/restart/read passed:', args.artifact)