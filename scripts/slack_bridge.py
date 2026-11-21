"""Slack Events API / NeoBoard adapter. See slack_bridge.md for the receiver contract."""
import argparse
import hashlib
import hmac
import json
import os
import re
import sqlite3
import sys
import time
from urllib import error, parse, request


def timestamp(value):
    if not isinstance(value, str) or not re.fullmatch(r'[0-9]{1,12}\.[0-9]{6}', value):
        raise ValueError('invalid Slack timestamp')
    return value


def version(ts):
    return int(timestamp(ts).replace('.', ''))


def digest(text):
    return hashlib.sha256(text.encode()).hexdigest()


class Deferred(Exception):
    """Thread root has not arrived yet."""


class RateLimited(Exception):
    def __init__(self, delay):
        self.delay = max(1, int(delay))
        super().__init__(f'retry after {self.delay} seconds')


class Bridge:
    def __init__(self, path, cfg, board, slack):
        for k in ('team', 'app', 'bot_user', 'conversation', 'channel', 'board_actor', 'board_url'):
            if not isinstance(cfg[k], str) or not cfg[k].strip():
                raise ValueError(f'empty {k}')
        url = parse.urlsplit(cfg['board_url'])
        if not url.hostname or url.username or url.password or url.query or url.fragment or not (
                url.scheme == 'https' or url.scheme == 'http' and url.hostname in ('127.0.0.1', 'localhost', '::1')):
            raise ValueError('board requires HTTPS or loopback HTTP')
        self.cfg, self.board, self.slack = cfg, board, slack
        self.db = db = sqlite3.connect(path, timeout=2)
        self.binding = json.dumps(cfg, sort_keys=True)
        db.executescript('''
            CREATE TABLE IF NOT EXISTS state(id INTEGER PRIMARY KEY,binding TEXT,cursor INTEGER,resume REAL);
            CREATE TABLE IF NOT EXISTS inbox(event TEXT PRIMARY KEY,body TEXT);
            CREATE TABLE IF NOT EXISTS msg(mid INTEGER PRIMARY KEY,root INTEGER NOT NULL,ts TEXT UNIQUE,
                origin TEXT NOT NULL,revision INTEGER NOT NULL,hash TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS removed(ts TEXT PRIMARY KEY);
        ''')
        with db:
            db.execute('INSERT OR IGNORE INTO state VALUES(1,?,0,0)', (self.binding,))
        if db.execute('SELECT binding FROM state').fetchone()[0] != self.binding:
            db.close()
            raise ValueError('state binding mismatch')

    def receive(self, raw, stamp, signature, secret, now=None):
        now = time.time() if now is None else now
        if not secret or not re.fullmatch('[0-9]{1,12}', stamp) or abs(now - int(stamp)) > 300:
            raise ValueError('invalid signing secret or stale delivery')
        expected = 'v0=' + hmac.new(secret, b'v0:' + stamp.encode() + b':' + raw, hashlib.sha256).hexdigest()
        if not hmac.compare_digest(expected, signature):
            raise ValueError('invalid signature')
        p = json.loads(raw)
        if p['type'] == 'url_verification':
            return p['challenge']
        if p['type'] != 'event_callback' or (p['team_id'], p['api_app_id']) != (self.cfg['team'], self.cfg['app']):
            return 'ignored'
        e = p['event']
        if e['type'] != 'message' or e.get('channel') != self.cfg['conversation'] or e.get('subtype') not in (
                None, 'message_changed', 'message_deleted'):
            return 'ignored'
        if not isinstance(p['event_id'], str) or not p['event_id']:
            raise ValueError('missing event_id')
        with self.db:
            self.db.execute('INSERT OR IGNORE INTO inbox VALUES(?,?)', (p['event_id'], json.dumps(e)))
        return 'accepted'

    def action(self, key, **body):
        result = self.board('/v1/actions', body, {'Idempotency-Key': digest(self.binding + ':' + key)})['id']
        if type(result) != int or result <= 0:
            raise ValueError('invalid board action outcome')
        return result

    def import_event(self, e):
        db = self.db
        deleted = e.get('subtype') == 'message_deleted'
        m = e.get('message', e)
        ts = timestamp(e['deleted_ts'] if deleted else m['ts'])
        row = db.execute('SELECT mid,origin,revision FROM msg WHERE ts=?', (ts,)).fetchone()
        if row and row[1] == 'board' or m.get('bot_id') or m.get('user') == self.cfg['bot_user']:
            return
        if db.execute('SELECT 1 FROM removed WHERE ts=?', (ts,)).fetchone():
            return
        if deleted:
            if row:
                self.action('delete:' + ts, type='edit', id=row[0], text='[Deleted on Slack]')
            db.execute('INSERT INTO removed VALUES(?)', (ts,))
            return
        if m.get('subtype') or not m.get('user') or not m.get('text', '').strip():
            return
        rev = version(m.get('edited', {}).get('ts', e.get('event_ts', ts) if e.get('subtype') else ts))
        if row and rev <= row[2]:
            return
        if row:
            self.action(f'edit:{ts}:{rev}', type='edit', id=row[0], text=m['text'])
            db.execute('UPDATE msg SET revision=? WHERE mid=?', (rev, row[0]))
            return
        if m.get('thread_ts') and m['thread_ts'] != ts:
            parent = timestamp(m['thread_ts'])
            root = db.execute('SELECT mid FROM msg WHERE ts=? AND mid=root', (parent,)).fetchone()
            if not root:
                if db.execute('SELECT 1 FROM removed WHERE ts=?', (parent,)).fetchone():
                    return
                raise Deferred()
            mid = self.action('create:' + ts, type='reply', thread=root[0], text=m['text'])
            root_id = root[0]
        else:
            root_id = mid = self.action('create:' + ts, type='post', channel=self.cfg['channel'], text=m['text'])
        db.execute('INSERT INTO msg VALUES(?,?,?,?,?,?)', (mid, root_id, ts, 'slack', rev, ''))

    def drain(self):
        for key, body in self.db.execute('SELECT event,body FROM inbox WHERE body IS NOT NULL ORDER BY rowid').fetchall():
            try:
                with self.db:
                    self.import_event(json.loads(body))
                    self.db.execute('UPDATE inbox SET body=NULL WHERE event=?', (key,))
            except Deferred:
                continue
        return self.db.execute('SELECT count(*) FROM inbox WHERE body IS NOT NULL').fetchone()[0]

    def verify_bot(self):
        identity = self.slack('/api/auth.test', {})
        if not identity.get('ok') or (identity.get('team_id'), identity.get('user_id')) != (
                self.cfg['team'], self.cfg['bot_user']):
            raise ValueError('Slack token does not match bound workspace/bot')

    def send(self, method, **body):
        result = self.slack('/api/chat.' + method, dict(channel=self.cfg['conversation'], **body))
        if not result.get('ok'):
            if method == 'delete' and result.get('error') == 'message_not_found':
                return
            raise RuntimeError('Slack rejected ' + method + ': ' + str(result.get('error')))
        if result.get('channel') != self.cfg['conversation']:
            raise ValueError('Slack response channel mismatch')
        ts = timestamp(result['ts'])
        if 'ts' in body and ts != body['ts']:
            raise ValueError('Slack response timestamp mismatch')
        return ts

    def export_event(self, e):
        db = self.db
        if e.get('channel') == self.cfg['channel'] and e['type'] == 'channel_deleted':
            raise RuntimeError('bound channel deleted; retire binding before reuse')
        if e.get('channel') == self.cfg['channel'] and e['type'] == 'thread_deleted':
            for mid, ts, origin in db.execute('SELECT mid,ts,origin FROM msg WHERE root=? ORDER BY mid DESC', (e['id'],)).fetchall():
                if origin == 'board':
                    self.send('delete', ts=ts)
                db.execute('INSERT OR IGNORE INTO removed VALUES(?)', (ts,))
                db.execute('DELETE FROM msg WHERE mid=?', (mid,))
            return
        if e['type'] != 'message':
            return
        m = e['message']
        if m['channel'] != self.cfg['channel'] or self.cfg['board_actor'] in (e['actor'], m['author']):
            return
        mid = m['id']
        row = db.execute('SELECT ts,origin,hash FROM msg WHERE mid=?', (mid,)).fetchone()
        fingerprint = digest(m['text'])
        text = m['text'].replace('&', '&amp;').replace('<', '&lt;').replace('>', '&gt;')
        if len(text) > 40000:
            raise ValueError('text exceeds Slack limit; refusing a truncated mirror')
        if row:
            if row[1] == 'board' and row[2] != fingerprint:
                self.send('update', ts=row[0], text=text, parse='none')
                db.execute('UPDATE msg SET hash=? WHERE mid=?', (fingerprint, mid))
            return
        body = dict(text=text, mrkdwn=False, unfurl_links=False, unfurl_media=False)
        if m['parent'] is not None:
            root = db.execute('SELECT ts FROM msg WHERE mid=? AND root=mid', (m['parent'],)).fetchone()
            if not root:
                return  # unmapped parent, never fall back to a top-level post
            body['thread_ts'] = root[0]
        # A timeout or crash after chat.postMessage is ambiguous. Persist the intent first.
        db.execute('INSERT INTO msg VALUES(?,?,NULL,?,?,?)', (mid, m['parent'] or mid, 'board', 0, fingerprint))
        db.commit()
        try:
            ts = self.send('postMessage', **body)
        except RateLimited:
            db.execute('DELETE FROM msg WHERE mid=?', (mid,))
            db.commit()
            raise
        db.execute('UPDATE msg SET ts=? WHERE mid=?', (ts, mid))

    def reconcile(self, mid, ts):
        with self.db as db:
            if ts == 'absent':
                changed = db.execute('DELETE FROM msg WHERE mid=? AND ts IS NULL', (mid,))
            else:
                changed = db.execute('UPDATE msg SET ts=? WHERE mid=? AND ts IS NULL', (timestamp(ts), mid))
            if changed.rowcount != 1:
                raise ValueError('not a pending Slack post')

    def poll(self):
        pending = self.db.execute('SELECT mid FROM msg WHERE ts IS NULL').fetchone()
        if pending:
            raise RuntimeError(f'reconcile pending Slack post for board message {pending[0]}')
        after, resume = self.db.execute('SELECT cursor,resume FROM state').fetchone()
        if resume > time.time():
            raise RateLimited(int(resume - time.time()) + 1)
        for e in self.board(f'/v1/events?after={after}&wait_ms=0'):
            if type(e['cursor']) != int or e['cursor'] <= after:
                raise ValueError('non-increasing board cursor')
            try:
                with self.db:
                    self.export_event(e)
                    after = e['cursor']
                    self.db.execute('UPDATE state SET cursor=?', (after,))
            except RateLimited as exc:
                with self.db:
                    self.db.execute('UPDATE state SET resume=?', (time.time() + exc.delay,))
                raise


class NoRedirect(request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        return None


def client(base, token):
    opener = request.build_opener(request.ProxyHandler({}), NoRedirect())

    def call(path, body=None, headers=None):
        req = request.Request(base + path, None if body is None else json.dumps(body).encode(),
                              {'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json', **(headers or {})})
        try:
            with opener.open(req, timeout=30) as response:
                return json.load(response)
        except error.HTTPError as exc:
            if exc.code == 429:
                raise RateLimited(exc.headers.get('Retry-After', 60)) from exc
            raise
    return call


def main():
    import fcntl
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('database')
    p.add_argument('binding')
    cmd = p.add_subparsers(dest='command', required=True)
    hook = cmd.add_parser('receive')
    hook.add_argument('timestamp')
    hook.add_argument('signature')
    cmd.add_parser('work')
    repair = cmd.add_parser('reconcile')
    repair.add_argument('mid', type=int)
    repair.add_argument('ts', help='verified Slack timestamp or absent')
    args = p.parse_args()
    os.umask(0o077)
    with open(args.binding) as f:
        cfg = json.load(f)
    bridge = Bridge(args.database, cfg, client(cfg['board_url'].rstrip('/'), os.getenv('NEOCHAT_TOKEN', '')),
                    client('https://slack.com', os.getenv('SLACK_BOT_TOKEN', '')))
    if args.command == 'receive':
        raw = sys.stdin.buffer.read(1_048_577)
        if len(raw) > 1_048_576:
            p.error('delivery too large')
        print(bridge.receive(raw, args.timestamp, args.signature, os.environ['SLACK_SIGNING_SECRET'].encode()))
        return
    with open(os.path.realpath(args.database) + '.worker.lock', 'a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if args.command == 'reconcile':
            bridge.reconcile(args.mid, args.ts)
        else:
            if not os.getenv('SLACK_BOT_TOKEN') or not os.getenv('NEOCHAT_TOKEN'):
                p.error('worker needs board and Slack tokens')
            bridge.verify_bot()
            deferred = bridge.drain()
            if deferred:
                print(f'{deferred} Slack deliveries waiting for thread roots', file=sys.stderr)
            bridge.poll()


if __name__ == '__main__':
    main()