"""One repository/channel GitHub bridge. See github_bridge.md before deployment."""
import argparse
import hashlib
import hmac
import json
import os
import re
import sqlite3
import sys
from urllib import request, parse


def positive(value):
    if type(value) is not int or value <= 0:
        raise ValueError('expected positive integer ID')
    return value


class Bridge:
    def __init__(self, path, cfg, board, github):
        self.cfg, self.board, self.github = cfg, board, github
        self.db = db = sqlite3.connect(path, timeout=60)
        db.executescript('''
            CREATE TABLE IF NOT EXISTS config(id INTEGER PRIMARY KEY, binding TEXT, cursor INTEGER);
            CREATE TABLE IF NOT EXISTS mapping(number INTEGER PRIMARY KEY, thread INTEGER UNIQUE NOT NULL);
            CREATE TABLE IF NOT EXISTS delivery(key TEXT PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS imported(source TEXT PRIMARY KEY, message INTEGER UNIQUE NOT NULL);
            CREATE TABLE IF NOT EXISTS exported(message INTEGER PRIMARY KEY, root INTEGER NOT NULL, remote INTEGER);
        ''')
        with db:
            binding = json.dumps(cfg, sort_keys=True)
            db.execute('INSERT OR IGNORE INTO config VALUES(1,?,0)', (binding,))
            if db.execute('SELECT binding FROM config').fetchone()[0] != binding:
                raise ValueError('state belongs to a different bridge configuration')

    def action(self, key, **action):
        prefix = f"gh:{self.cfg['repo_id']}:{self.cfg['channel']}:"
        idem = hashlib.sha256((prefix + key).encode()).hexdigest()
        return positive(self.board('/v1/actions', action, {'Idempotency-Key': idem})['id'])

    def webhook(self, event, delivery, signature, raw, secret):
        if not secret or not hmac.compare_digest(
                'sha256=' + hmac.new(secret, raw, hashlib.sha256).hexdigest(), signature):
            raise ValueError('invalid webhook signature')
        if not delivery:
            raise ValueError('missing delivery ID')
        p = json.loads(raw)
        if p['repository']['id'] != self.cfg['repo_id']:
            raise ValueError('wrong repository')
        if (event, p.get('action')) not in {('issues', 'opened'), ('pull_request', 'opened'),
                ('issue_comment', 'created'), ('pull_request_review_comment', 'created')}:
            return 'ignored'
        item = p['issue' if event in ('issues', 'issue_comment') else 'pull_request']
        number = positive(item['number'])
        obj = p['comment'] if event.endswith('comment') else item
        if obj['user']['id'] == self.cfg['github_actor_id']:
            return 'echo'
        key = f"{event}:{positive(obj['id'])}"
        text = (obj.get('title', '') + '\n\n' + (obj['body'] or '')).strip() or '(empty)'
        with self.db as db:
            db.execute('BEGIN IMMEDIATE')
            if db.execute('SELECT 1 FROM delivery WHERE key=?', (delivery,)).fetchone():
                return 'duplicate'
            db.execute('INSERT INTO delivery VALUES(?)', (delivery,))
            if db.execute('SELECT 1 FROM imported WHERE source=?', (key,)).fetchone():
                return 'duplicate'
            row = db.execute('SELECT thread FROM mapping WHERE number=?', (number,)).fetchone()
            if not row:
                if db.execute('SELECT 1 FROM exported WHERE root=1 AND remote IS NULL').fetchone():
                    raise RuntimeError('issue creation in flight; redeliver webhook later')
                # issue_comment can arrive before issues.opened (also for PRs).
                task = self.cfg.get('issue_task_assignee') if event in ('issues', 'issue_comment') and 'pull_request' not in item else None
                thread = self.action(f'root:{number}', type='post', channel=self.cfg['channel'],
                                     text=f"GitHub {self.cfg['repo']}#{number}",
                                     **({'task': True, 'assignee': task} if task else {}))
                db.execute('INSERT INTO mapping VALUES(?,?)', (number, thread))
            else:
                thread = row[0]
            message = self.action(key, type='reply', thread=thread, text=text)
            db.execute('INSERT INTO imported VALUES(?,?)', (key, message))
        return 'imported'

    def cursor(self):
        return self.db.execute('SELECT cursor FROM config').fetchone()[0]

    def finish(self, message, remote):
        with self.db as db:
            db.execute('BEGIN IMMEDIATE')
            row = db.execute('SELECT root FROM exported WHERE message=?', (message,)).fetchone()
            changed = db.execute('UPDATE exported SET remote=? WHERE message=? AND remote IS NULL',
                                 (positive(remote), message)).rowcount
            if changed != 1:
                raise ValueError('no pending export')
            if row[0]:
                db.execute('INSERT INTO mapping VALUES(?,?)', (remote, message))

    def poll(self):
        after = self.cursor()
        events = self.board(f'/v1/events?after={after}&wait_ms=0')
        for e in events:
            seq = positive(e['cursor'])
            if seq <= after:
                raise ValueError('non-increasing cursor')
            m = e.get('message', {})
            target = None
            with self.db as db:
                db.execute('BEGIN IMMEDIATE')
                if self.cursor() != after:
                    return  # a concurrent poll advanced; refetch next time
                if (e['type'] == 'message' and m['channel'] == self.cfg['channel']
                        and e['actor'] != self.cfg['board_actor']):
                    mid = positive(m['id'])
                    row = db.execute('SELECT number FROM mapping WHERE thread=?', (m['parent'],)).fetchone()
                    own = db.execute('SELECT 1 FROM imported WHERE message=? UNION ALL SELECT 1 FROM mapping WHERE thread=?', (mid, mid)).fetchone()
                    root = m['parent'] is None and m['labels'].get('github') == 'issue'
                    if not own and (row or root):
                        old = db.execute('SELECT remote FROM exported WHERE message=?', (mid,)).fetchone()
                        if old == (None,):
                            raise RuntimeError(f'unresolved export of board message {mid}; reconcile before polling')
                        if old is None:
                            db.execute('INSERT INTO exported VALUES(?,?,NULL)', (mid, root))
                            target = '/issues' if root else f'/issues/{row[0]}/comments'
                if not target:
                    db.execute('UPDATE config SET cursor=?', (seq,))
            if target:
                body = {'body': m['text'] + f'\n\n<!-- neoboard:{mid} -->'}
                if root:
                    body['title'] = (m['text'].strip().split('\n')[0] or 'Board issue')[:200]
                result = self.github('/repos/' + self.cfg['repo'] + target, body)
                self.finish(mid, result['number' if root else 'id'])
                with self.db as db:
                    db.execute('UPDATE config SET cursor=max(cursor,?)', (seq,))
            after = seq


class NoRedirect(request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        return None


def client(base, token):
    opener = request.build_opener(request.ProxyHandler({}), NoRedirect())

    def call(path, body=None, headers=None):
        req = request.Request(base + path, None if body is None else json.dumps(body).encode(),
                              {'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json', **(headers or {})})
        with opener.open(req, timeout=30) as response:
            return json.load(response)
    return call


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('database')
    parser.add_argument('config', help='JSON binding file')
    commands = parser.add_subparsers(dest='cmd', required=True)
    hook = commands.add_parser('webhook', help='read exact webhook bytes from stdin')
    for arg in ('event', 'delivery', 'signature'):
        hook.add_argument(arg)
    commands.add_parser('poll')
    reconcile = commands.add_parser('confirm', help='record an independently verified remote write')
    reconcile.add_argument('message', type=int)
    reconcile.add_argument('remote', type=int)
    retry = commands.add_parser('retry-absent', help='clear a pending attempt ONLY after verifying no remote write')
    retry.add_argument('message', type=int)
    args = parser.parse_args()
    with open(args.config) as f:
        cfg = json.load(f)
    if not re.fullmatch(r'[\w-]+/[\w.-]+', cfg['repo'], re.ASCII) or cfg['repo'].split('/')[1] in ('.', '..'):
        parser.error('invalid repository name')
    url = parse.urlsplit(cfg['board_url'])
    if not (url.scheme == 'https' or url.scheme == 'http' and url.hostname in ('127.0.0.1', '::1', 'localhost')):
        parser.error('board requires HTTPS or loopback HTTP')
    positive(cfg['repo_id'])
    positive(cfg['github_actor_id'])
    os.umask(0o077)
    bridge = Bridge(args.database, cfg, client(cfg['board_url'].rstrip('/'), os.environ['NEOCHAT_TOKEN']),
                    client('https://api.github.com', os.environ['GITHUB_TOKEN']))
    if args.cmd == 'poll':
        if bridge.github('/repos/' + cfg['repo'])['id'] != cfg['repo_id']:
            raise ValueError('repository name now points to another repository')
        bridge.poll()
    elif args.cmd == 'confirm':
        bridge.finish(args.message, args.remote)
    elif args.cmd == 'retry-absent':
        with bridge.db as db:
            if not db.execute('DELETE FROM exported WHERE message=? AND remote IS NULL', (args.message,)).rowcount:
                raise ValueError('no pending export')
    else:
        raw = sys.stdin.buffer.read(25 * 1024 * 1024 + 1)
        if len(raw) > 25 * 1024 * 1024:
            parser.error('webhook too large')
        print(bridge.webhook(args.event, args.delivery, args.signature, raw,
                             os.environ['GITHUB_WEBHOOK_SECRET'].encode()))


if __name__ == '__main__':
    main()