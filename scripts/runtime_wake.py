"""One operator-authorized turn in an explicitly selected runtime session."""

import argparse
import json
import re
import subprocess
import threading


UUID = r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}"
LIMIT = 4 * 1024 * 1024


class ProtocolError(ValueError):
    pass


def record(pipe):
    line = pipe.readline(LIMIT + 1)
    if not line:
        return None
    if len(line) > LIMIT:
        raise ProtocolError('oversized record')
    value = json.loads(line)
    if not isinstance(value, dict):
        raise ProtocolError('not an object')
    return value


def codex(proc, session, prompt):
    def send(message):
        proc.stdin.write(json.dumps(message).encode() + b'\n')
        proc.stdin.flush()

    def request(number, method, params):
        send({'id': number, 'method': method, 'params': params})
        while (msg := record(proc.stdout)) is not None:
            if 'id' not in msg:  # asynchronous notification
                continue
            if msg.get('id') != number or 'method' in msg or 'error' in msg:
                break
            result = msg.get('result')
            if isinstance(result, dict):
                return result
            break
        raise ProtocolError('missing response')

    request(1, 'initialize', {'clientInfo': {'name': 'neoboard', 'version': '1'}})
    send({'method': 'initialized', 'params': {}})
    resumed = request(2, 'thread/resume', {'threadId': session})
    if resumed['thread']['id'] != session:
        raise ProtocolError('resume mismatch')
    started = request(3, 'turn/start', {'threadId': session, 'input': [{'type': 'text', 'text': prompt}]})
    if not started['turn']['id']:
        raise ProtocolError('missing turn')
    # Keep the stdio server alive until the turn settles. Never approve requests.
    while (msg := record(proc.stdout)) is not None:
        if 'id' in msg:
            raise ProtocolError('interactive request')
        if msg.get('method') == 'turn/completed':
            params = msg['params']
            if (params['threadId'], params['turn']['id'], params['turn']['status']) != (
                    session, started['turn']['id'], 'completed'):
                raise ProtocolError('turn mismatch or failure')
            return
    raise ProtocolError('missing completion')


def wake(runtime, session, task, allow_start):
    pattern = r'ses_[A-Za-z0-9]{1,128}' if runtime == 'opencode' else UUID
    if not re.fullmatch(pattern, session) or task < 1:
        raise ValueError('expected a concrete session ID and positive task ID')
    if not allow_start:
        return 'consent_required'
    prompt = f'Check NeoBoard task #{task} and verify its current assignment before acting.'
    commands = {
        'claude': ['claude', '--resume', session, '--print', '--output-format', 'json', prompt],
        'opencode': ['opencode', 'run', '--session', session, '--format', 'json', prompt],
        'codex': ['codex', 'app-server'],
    }
    try:
        proc = subprocess.Popen(commands[runtime], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                stdin=subprocess.PIPE if runtime == 'codex' else subprocess.DEVNULL)
    except OSError:
        return 'not_started'
    with proc:
        timer = threading.Timer(300, proc.kill)
        timer.start()
        try:
            if runtime == 'codex':
                codex(proc, session, prompt)
                proc.stdin.close()
            elif runtime == 'claude':
                data = proc.stdout.read(LIMIT + 1)
                if len(data) > LIMIT:
                    raise ProtocolError('oversized result')
                result = json.loads(data)
                if (result['type'], result['subtype'], result['session_id']) != ('result', 'success', session) or result['is_error'] is not False:
                    raise ProtocolError('unexpected result')
            else:
                finished = False
                while (event := record(proc.stdout)) is not None:
                    if event['sessionID'] != session or event['type'] == 'error':
                        raise ProtocolError('unexpected event')
                    finished |= event['type'] == 'step_finish'
                if not finished:
                    raise ProtocolError('missing step')
            return 'delivered' if proc.wait() == 0 else 'uncertain'
        except (ValueError, TypeError, KeyError, OSError):
            return 'uncertain'
        finally:
            timer.cancel()
            if proc.poll() is None:
                proc.kill()


def main():
    parser = argparse.ArgumentParser(allow_abbrev=False, description=__doc__)
    parser.add_argument('runtime', choices=['claude', 'codex', 'opencode'])
    parser.add_argument('--session', required=True)
    parser.add_argument('--task', type=int, required=True)
    parser.add_argument('--allow-start', action='store_true')
    args = parser.parse_args()
    try:
        status = wake(args.runtime, args.session, args.task, args.allow_start)
    except ValueError as exc:
        parser.error(str(exc))
    print(json.dumps({'status': status, 'runtime': args.runtime, 'session': args.session, 'task': args.task}))
    return 0 if status == 'delivered' else 1


if __name__ == '__main__':
    raise SystemExit(main())