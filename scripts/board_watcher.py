"""Durable pull consumer for Neoboard. See board_watcher.md. Python 3.10+."""

import argparse
import contextlib
import ipaddress
import json
import os
import re
import secrets
import sys
import time
import weakref
from http.client import IncompleteRead
from pathlib import Path
from urllib import error, parse, request

import turso
from filelock import FileLock


class NoRedirect(request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        return None


@contextlib.contextmanager
def store(path, create=False):
    # Embedded Turso opens the store in one process at a time. Serialize even
    # opens/teardowns, but never hold the database across a network wait.
    with FileLock(str(path) + '.watcher.lock', timeout=10):
        if not create and not path.is_file():
            raise ValueError('watcher database does not exist')
        db = turso.connect(str(path), isolation_level=None)
        try:
            db.row_factory = turso.Row
            # In pyturso 0.8.3 close() alone retains the native DB handle. Do
            # not let the caller keep it (including via a Row) past unlock.
            yield weakref.proxy(db)
        finally:
            db.close()
            del db


@contextlib.contextmanager
def atomic(db):
    db.execute('BEGIN IMMEDIATE')
    try:
        yield
        db.commit()
    except BaseException:
        db.rollback()
        raise


def initialize(db, args):
    url = parse.urlsplit(args.endpoint)
    try:
        loopback = ipaddress.ip_address(url.hostname or '').is_loopback
    except ValueError:
        loopback = url.hostname == 'localhost'
    if (not url.hostname or url.username is not None or url.query or url.fragment
            or not (url.scheme == 'https' or url.scheme == 'http' and loopback)):
        raise ValueError('expected HTTPS or loopback HTTP endpoint without credentials/query/fragment')
    if (not args.channel or (args.match == 'label') != bool(args.label) or (args.thread and args.match != 'reply')
            or (args.match != 'label' and (args.before is not None or args.after is not None))):
        raise ValueError('specify an exact channel; --label requires label mode; --thread requires reply mode')
    with atomic(db):
        db.execute('CREATE TABLE watcher(config TEXT NOT NULL, cursor INTEGER NOT NULL, namespace TEXT NOT NULL)')
        db.execute('CREATE TABLE seen(id INTEGER PRIMARY KEY, parent INTEGER, labels TEXT NOT NULL, ready INTEGER NOT NULL)')
        db.execute('''CREATE TABLE delivery(id TEXT PRIMARY KEY, cursor INTEGER UNIQUE NOT NULL,
                    payload TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, done INTEGER DEFAULT 0,
                    receipt TEXT, available INTEGER DEFAULT 0, last_error TEXT)''')
        config = dict(endpoint=args.endpoint.rstrip('/'), channel=args.channel, match=args.match,
                      thread=args.thread, label=args.label, before=args.before, after=args.after)
        db.execute('INSERT INTO watcher VALUES(?,0,?)', (json.dumps(config), secrets.token_hex(16)))
        if args.match == 'assignment':
            db.execute('CREATE TABLE binding(actor TEXT PRIMARY KEY, endpoint TEXT NOT NULL, session TEXT NOT NULL, UNIQUE(endpoint,session))')
            db.execute('CREATE TABLE assignment(task INTEGER PRIMARY KEY, actor TEXT NOT NULL)')
            db.execute('''CREATE TABLE wake(cursor INTEGER PRIMARY KEY, task INTEGER NOT NULL, actor TEXT NOT NULL,
                       endpoint TEXT, session TEXT, status TEXT NOT NULL, checked INTEGER NOT NULL DEFAULT 0)''')


def assignment(db, event, msg):
    kind = event['type']
    if kind == 'channel_deleted':
        db.execute('DELETE FROM assignment')
        db.execute("UPDATE wake SET status='superseded' WHERE status IN ('pending','busy','unavailable')")
        return
    if kind not in ('message', 'thread_deleted'):
        return
    task = msg['id']
    if type(task) is not int or task <= 0:
        raise ValueError('invalid task id')
    info = msg.get('task') or {}
    actor = info.get('assignee') if info.get('state') in ('ready', 'waiting', 'blocked', 'claimed') else None
    old = db.execute('SELECT actor FROM assignment WHERE task=?', (task,)).fetchone()
    if (old[0] if old else None) == actor:
        return
    db.execute('DELETE FROM assignment WHERE task=?', (task,))
    db.execute("UPDATE wake SET status='superseded' WHERE task=? AND status IN ('pending','busy','unavailable')", (task,))
    if kind == 'message' and isinstance(actor, str) and actor and msg.get('parent') is None:
        db.execute('INSERT INTO assignment VALUES(?,?)', (task, actor))
        bound = db.execute('SELECT endpoint,session FROM binding WHERE actor=?', (actor,)).fetchone()
        endpoint, session = bound if bound else (None, None)
        db.execute('INSERT INTO wake(cursor,task,actor,endpoint,session,status) VALUES(?,?,?,?,?,?)',
                   (event['cursor'], task, actor, endpoint, session, 'pending' if bound else 'unbound'))
        return dict(actor=actor)


def match(db, cfg, event):
    kind = event['type']
    msg = event['message'] if kind == 'message' else event
    if msg.get('channel') != cfg['channel']:
        return None
    if cfg['match'] == 'assignment':
        return assignment(db, event, msg)
    if kind == 'channel_deleted':
        db.execute('DELETE FROM seen')
    elif kind == 'thread_deleted':
        db.execute('DELETE FROM seen WHERE id=? OR parent=?', (msg['id'], msg['id']))
    transition = None
    if kind == 'message':
        old = db.execute('SELECT labels,ready FROM seen WHERE id=?', (msg['id'],)).fetchone()
        labels = msg['labels']
        ready = (msg.get('task') or {}).get('state') == 'ready'
        db.execute('INSERT OR REPLACE INTO seen VALUES(?,?,?,?)',
                   (msg['id'], msg.get('parent'), json.dumps(labels), ready))
        if cfg['match'] == 'reply' and not old and msg.get('parent') is not None:
            if cfg['thread'] is None or cfg['thread'] == msg['parent']:
                transition = {}
        elif cfg['match'] == 'task-ready' and ready and not (old and old['ready']):
            transition = {}
        elif cfg['match'] == 'label':
            key = cfg['label']
            before = json.loads(old['labels']).get(key) if old else None
            after = labels.get(key)
            if (before != after and (cfg['before'] is None or cfg['before'] == before)
                    and (cfg['after'] is None or cfg['after'] == after)):
                transition = dict(key=key, before=before, after=after)
    return {} if cfg['match'] == 'channel' else transition


def poll(path, wait):
    with store(path) as db:
        row = dict(db.execute('SELECT * FROM watcher').fetchone())
    cfg = json.loads(row['config'])
    token = os.environ.get('NEOBOARD_WATCH_TOKEN')
    if not token:
        raise ValueError('set NEOBOARD_WATCH_TOKEN')
    url = cfg['endpoint'] + '/v1/events?' + parse.urlencode(dict(after=row['cursor'], wait_ms=wait))
    http = request.build_opener(request.ProxyHandler({}), NoRedirect())
    with http.open(request.Request(url, headers={'Authorization': 'Bearer ' + token}), timeout=35) as res:
        events = json.load(res)
    if not isinstance(events, list):
        raise ValueError('expected an event array')
    with store(path) as db, atomic(db):
        cursor, namespace = db.execute('SELECT cursor,namespace FROM watcher').fetchone()
        if namespace != row['namespace']:
            raise ValueError('subscription replaced during poll')
        previous = row['cursor']
        for event in events:
            seq = event['cursor']
            if type(seq) is not int or seq <= previous:
                raise ValueError('expected increasing event cursors')
            previous = seq
            if seq <= cursor:  # another poll committed while our HTTP request was in flight
                continue
            details = match(db, cfg, event)
            if details is not None:
                delivery_id = f"{row['namespace']}:{seq}"
                payload = dict(id=delivery_id, match=cfg['match'], details=details, event=event)
                db.execute('INSERT INTO delivery(id,cursor,payload) VALUES(?,?,?)',
                           (delivery_id, seq, json.dumps(payload)))
            cursor = seq
        db.execute('UPDATE watcher SET cursor=?', (cursor,))
    return dict(cursor=cursor)


def consume(db, args):
    with atomic(db):
        now = int(time.time())
        if args.cmd == 'take':
            row = db.execute('SELECT * FROM delivery WHERE done=0 AND available<=? ORDER BY cursor LIMIT 1',
                             (now,)).fetchone()
            if row is None:
                return None
            receipt = secrets.token_hex(16)
            db.execute('UPDATE delivery SET receipt=?, available=?, attempts=attempts+1 WHERE id=?',
                       (receipt, now + args.lease, row['id']))
            return dict(delivery=json.loads(row['payload']), receipt=receipt, attempt=row['attempts'] + 1)
        if args.cmd == 'replay':
            changed = db.execute('UPDATE delivery SET done=0,available=0,receipt=NULL WHERE id=?', (args.id,))
        else:
            done = args.cmd == 'ack'
            changed = db.execute('''UPDATE delivery SET done=?, available=?, receipt=NULL,last_error=?
                                  WHERE id=? AND receipt=? AND done=0 AND available>?''',
                                 (done, 0 if done else now + args.delay, None if done else args.reason,
                                  args.id, args.receipt, now))
        if not changed.rowcount:
            raise ValueError('unknown delivery or stale receipt')
        return dict(id=args.id)


def positive(value):
    n = int(value)
    if not 1 <= n <= 2147483647:
        raise argparse.ArgumentTypeError('expected positive 32-bit integer')
    return n


def bind(db, args):
    url = parse.urlsplit(args.endpoint)
    if (url.scheme not in ('http', 'https') or url.hostname not in ('127.0.0.1', '::1', 'localhost')
            or url.username is not None or url.query or url.fragment or url.path not in ('', '/')
            or not args.actor or not re.fullmatch(r'ses_[a-zA-Z0-9_-]+', args.session)):
        raise ValueError('expected exact actor, loopback Neoism origin and ses_ session ID')
    db.execute('INSERT INTO binding VALUES(?,?,?)', (args.actor, args.endpoint.rstrip('/'), args.session))


def wake_one(path):
    token = os.environ.get('NEOISM_WAKE_TOKEN')
    if not token:
        raise ValueError('set NEOISM_WAKE_TOKEN')
    http = request.build_opener(request.ProxyHandler({}), NoRedirect())
    headers = {'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'}
    # Serialize wake consumers, without holding the Turso handle during HTTP.
    with FileLock(str(path) + '.wake.lock', timeout=30):
        with store(path) as db:
            row = db.execute("SELECT * FROM wake WHERE status IN ('pending','busy','unavailable') ORDER BY checked,cursor LIMIT 1").fetchone()
            if row is None:
                return None
            item = dict(row)
            del row
            cfg = json.loads(db.execute('SELECT config FROM watcher').fetchone()[0])
        base = item['endpoint'] + '/v2/sessions'
        target = base + '/' + item['session']

        def get(url):
            with http.open(request.Request(url, headers=headers), timeout=10) as res:
                return json.load(res)

        try:
            # Idle sessions can be absent from the status map; absence alone
            # does not establish that the bound session exists.
            if get(target)['id'] != item['session']:
                raise ValueError('wrong session')
            status = get(base + '/status').get(item['session'], {'type': 'idle'})
            idle = status['type'] == 'idle' and not (status.get('compactionQueued') or status.get('runID'))
            outcome = 'uncertain' if idle else 'busy'
        except (OSError, IncompleteRead, ValueError, TypeError, KeyError, AttributeError):
            outcome = 'unavailable'
        with store(path) as db:
            # Commit a non-retryable reservation *before* the external effect.
            changed = db.execute("UPDATE wake SET status=?, checked=? WHERE cursor=? AND status IN ('pending','busy','unavailable')",
                                 (outcome, time.time_ns(), item['cursor']))
            if not changed.rowcount:
                return dict(cursor=item['cursor'], status='superseded')
        if outcome == 'uncertain':
            body = json.dumps({'prompt': f"NeoBoard assignment: check task #{item['task']} in channel {cfg['channel']}. Verify the current assignment before acting."}).encode()
            try:
                with http.open(request.Request(target + '/prompt-if-idle', body, headers), timeout=10) as res:
                    if res.status == 204:
                        outcome = 'accepted'
            except error.HTTPError as exc:
                if exc.code == 412:
                    # Only the conditional route's explicit no-effect response
                    # reopens a reservation. Never retry an ambiguous POST.
                    outcome = 'rejected'
                    try:
                        if json.load(exc).get('code') == 'session.not_idle':
                            outcome = 'busy'
                    except (ValueError, AttributeError, OSError, IncompleteRead):
                        pass
                elif exc.code in (404, 405, 501):
                    outcome = 'unsupported_or_missing'
                elif 400 <= exc.code < 500:
                    outcome = 'rejected'
            except (OSError, IncompleteRead):
                pass
            with store(path) as db:
                if outcome == 'busy' and not db.execute('''SELECT 1 FROM assignment WHERE task=? AND actor=?
                        AND NOT EXISTS (SELECT 1 FROM wake WHERE task=? AND cursor>?)''',
                        (item['task'], item['actor'], item['task'], item['cursor'])).fetchone():
                    outcome = 'superseded'
                db.execute('UPDATE wake SET status=? WHERE cursor=?', (outcome, item['cursor']))
        return dict(cursor=item['cursor'], status=outcome)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--db', required=True)
    sub = p.add_subparsers(dest='cmd', required=True)
    init = sub.add_parser('init')
    init.add_argument('--endpoint', required=True)
    init.add_argument('--channel', required=True)
    init.add_argument('--match', required=True, choices=['channel', 'reply', 'label', 'task-ready', 'assignment'])
    init.add_argument('--thread', type=positive)
    init.add_argument('--label')
    init.add_argument('--before')
    init.add_argument('--after')
    sub.add_parser('poll')
    sub.add_parser('follow')
    binding = sub.add_parser('bind')
    for field in ('actor', 'endpoint', 'session'):
        binding.add_argument('--' + field, required=True)
    sub.add_parser('wake')
    status = sub.add_parser('wake-status')
    status.add_argument('--after', type=int, default=0)
    status.add_argument('--limit', type=positive, default=100)
    inspect = sub.add_parser('inspect')
    inspect.add_argument('--after', type=int, default=0)
    inspect.add_argument('--limit', type=positive, default=100)
    sub.add_parser('take').add_argument('--lease', type=positive, default=60)
    for name in ('ack', 'fail', 'replay'):
        cmd = sub.add_parser(name)
        cmd.add_argument('id')
        if name != 'replay':
            cmd.add_argument('receipt')
        if name == 'fail':
            cmd.add_argument('--delay', type=positive, default=5)
            cmd.add_argument('--reason', default='consumer failed')
    args = p.parse_args()
    os.umask(0o077)
    path = Path(args.db).resolve()
    if args.cmd == 'follow':
        delay = 1
        while True:
            try:
                poll(path, 25000)
                delay = 1
            except (error.URLError, TimeoutError, ConnectionError, IncompleteRead) as exc:
                print(exc, file=sys.stderr, flush=True)
                time.sleep(delay)
                delay = min(60, delay * 2)
    if args.cmd == 'poll':
        result = poll(path, 0)
    elif args.cmd == 'wake':
        result = wake_one(path)
    else:
        with store(path, create=args.cmd == 'init') as db:
            if args.cmd == 'init':
                initialize(db, args)
                return
            if args.cmd == 'bind':
                bind(db, args)
                return
            if args.cmd == 'wake-status':
                print(json.dumps([dict(r) for r in db.execute('SELECT * FROM wake WHERE cursor>? ORDER BY cursor LIMIT ?', (args.after, args.limit))]))
                return
            if args.cmd != 'inspect':
                print(json.dumps(consume(db, args)))
                return
            with atomic(db):
                result = dict(cursor=db.execute('SELECT cursor FROM watcher').fetchone()[0], deliveries=[
                    dict(id=r['id'], delivery=json.loads(r['payload']), attempts=r['attempts'],
                         done=bool(r['done']), available_at=r['available'], last_error=r['last_error'])
                    for r in db.execute('SELECT * FROM delivery WHERE cursor>? ORDER BY cursor LIMIT ?',
                                        (args.after, args.limit))])
    print(json.dumps(result))


if __name__ == '__main__':
    try:
        main()
    except (OSError, turso.Error, ValueError, KeyError, TypeError) as exc:
        sys.exit(str(exc))