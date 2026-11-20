"""Offline-copy migration drill for two explicitly selected neochatd executables."""
import argparse
from contextlib import contextmanager
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


@contextmanager
def daemon(binary, directory):
    endpoint = directory / 'endpoint.json'
    check(not endpoint.exists(), 'stale endpoint')
    child = subprocess.Popen([binary, '--data-dir', directory, '--bind', '127.0.0.1:0'],
                             cwd=directory, env={'HOME': str(directory), 'PATH': os.defpath},
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL)
    try:
        deadline = time.monotonic() + 20
        while not endpoint.exists():
            check(child.poll() is None, f'{binary} exited during startup')
            check(time.monotonic() < deadline, 'startup timed out')
            time.sleep(.05)
        record = json.loads(endpoint.read_bytes())
        match = re.fullmatch(r'http://127\.0\.0\.1:(\d+)', record['url'])
        check(match and 0 < int(match[1]) < 65536, 'unexpected private endpoint')

        def request(path, token, data=None, status=200):
            check(child.poll() is None, 'daemon exited before request')
            conn = http.client.HTTPConnection('127.0.0.1', int(match[1]), timeout=10)
            headers = {'Authorization': 'Bearer ' + token}
            if isinstance(data, dict):
                headers['Content-Type'] = 'application/json'
                data = json.dumps(data).encode()
            try:
                conn.request('GET' if data is None else 'POST', '/v1/' + path, data, headers)
                resp = conn.getresponse()
                body = resp.read()
                check(resp.status == status, f'{path}: expected {status}, got {resp.status}')
                if status == 200:
                    return body if path.startswith('attachments/') else json.loads(body)
            finally:
                conn.close()

        yield request
    finally:
        child.terminate()
        try:
            check(child.wait(timeout=10) == 0, 'daemon shutdown failed')
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
            raise


PAYLOAD = bytes(range(256)) + b'\x00\xff'


def seed(api, directory):
    host = (directory / 'board-token').read_text().strip()
    admin = (directory / 'admin-token').read_text().strip()
    member = api('admin/invites', admin, {'actor': 'reader', 'channels': ['general']})['token']

    def action(**fields):
        return api('actions', host, fields)['id']

    root = action(type='post', channel='general', text='migration root')
    reply = action(type='reply', thread=root, text='migration reply')
    task = action(type='post', channel='private', text='migration task', task=True,
                  labels={'priority': 'high'})
    attachment = api(f'messages/{task}/attachments?name=fixture.bin', host, PAYLOAD)
    return host, member, root, reply, task, attachment


def verify(api, host, member, root, reply, task, attachment):
    rows = {m['id']: m for m in api('snapshot', host)['messages']}
    check(set(rows) == {root, reply, task}, 'host message set changed')
    for key, text, channel, parent in [(root, 'migration root', 'general', None),
                                      (reply, 'migration reply', 'general', root),
                                      (task, 'migration task', 'private', None)]:
        m = rows[key]
        check((m['text'], m['channel'], m['parent']) == (text, channel, parent),
              f'message {key}: content or thread mismatch')
    check(rows[task]['task']['state'] == 'ready' and rows[task]['labels'] == {'priority': 'high'},
          'task mismatch')
    check((attachment['name'], attachment['message_id'], attachment['size']) ==
          ('fixture.bin', task, len(PAYLOAD)), 'incorrect upload metadata')
    if 'attachments' in rows[task]:
        check(rows[task]['attachments'] == [attachment], 'snapshot attachment mismatch')
    listing = f'messages/{task}/attachments'
    download = f"attachments/{attachment['id']}"
    check(api(listing, host) == [attachment], 'attachment metadata mismatch')
    check(api(download, host) == PAYLOAD, 'attachment bytes mismatch')
    check({m['id'] for m in api('snapshot', member)['messages']} == {root, reply},
          'legacy member scope mismatch')
    api(listing, member, status=404)
    api(download, member, status=404)
    api('snapshot', 'invalid', status=401)
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ('--old', '--new'):
        parser.add_argument(flag, type=Path, required=True)
    args = parser.parse_args()
    binaries = [p.resolve(strict=True) for p in (args.old, args.new)]
    for stage, binary in zip(('old', 'new'), binaries):
        check(binary.is_file() and os.access(binary, os.X_OK), f'not executable: {binary}')
        digest = hashlib.sha256()
        with binary.open('rb') as f:
            for chunk in iter(lambda: f.read(1024 * 1024), b''):
                digest.update(chunk)
        print(f'{stage}: {binary} sha256={digest.hexdigest()}', flush=True)

    with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
        old, new = Path(a), Path(b)
        with daemon(binaries[0], old) as api:
            fixture = seed(api, old)
            baseline = verify(api, *fixture)
        for name in ('board.sqlite3', 'board-token', 'admin-token'):
            shutil.copy2(old / name, new / name)
        for suffix in ('-wal', '-journal'):
            sidecar = old / ('board.sqlite3' + suffix)
            if sidecar.exists():
                shutil.copy2(sidecar, new / sidecar.name)
        with daemon(binaries[1], new) as api:
            migrated = verify(api, *fixture)
            for key, row in baseline.items():
                # Later APIs can enrich snapshots (e.g. inline attachment metadata).
                for field, value in row.items():
                    check(field in migrated[key] and migrated[key][field] == value,
                          f'legacy message {key} field {field} changed')
    print('PASS: copied fixture and authorization verified (engine provenance not tested)')


if __name__ == '__main__':
    main()