"""One Linear organization to one board channel. See docs/linear-bridge.md."""
import argparse
import datetime as dt
import hashlib
import hmac
import json
import os
import re
import sqlite3
import sys
import uuid
from urllib import request, parse


def uid(value):
    return str(uuid.UUID(value))


class Bridge:
    def __init__(self, path, cfg, board, linear):
        self.cfg, self.board, self.linear = cfg, board, linear
        self.binding = json.dumps(cfg, sort_keys=True)
        self.db = db = sqlite3.connect(path, timeout=60)
        db.executescript('''
            CREATE TABLE IF NOT EXISTS config(binding TEXT NOT NULL, cursor INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS issues(issue TEXT PRIMARY KEY, thread INTEGER UNIQUE, version TEXT);
            CREATE TABLE IF NOT EXISTS deliveries(delivery TEXT PRIMARY KEY, digest TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS comments(comment TEXT PRIMARY KEY, message INTEGER UNIQUE);
            CREATE TABLE IF NOT EXISTS exports(message INTEGER PRIMARY KEY, target TEXT, done INTEGER);
        ''')
        with db:
            db.execute('BEGIN IMMEDIATE')
            row = db.execute('SELECT binding FROM config').fetchone()
            if row is None:
                db.execute('INSERT INTO config VALUES(?,0)', (self.binding,))
            elif row != (self.binding,):
                raise ValueError('database belongs to a different binding')

    def action(self, key, **body):
        idem = hashlib.sha256((self.binding + ':' + key).encode()).hexdigest()
        return self.board('/v1/actions', body, {'Idempotency-Key': idem})

    def root(self, issue):
        row = self.db.execute('SELECT thread,version FROM issues WHERE issue=?', (issue,)).fetchone()
        if row:
            return row
        thread = self.action('issue:' + issue, type='post', channel=self.cfg['channel'],
                             text='Linear issue ' + issue, labels={'linear_id': issue})['id']
        if type(thread) is not int or thread < 1:
            raise ValueError('board did not return a thread ID')
        self.db.execute('INSERT INTO issues VALUES(?,?,NULL)', (issue, thread))
        return thread, None

    def webhook(self, delivery, signature, raw, secret):
        if not secret or not hmac.compare_digest(signature, hmac.new(secret, raw, hashlib.sha256).hexdigest()):
            raise ValueError('bad Linear-Signature')
        p = json.loads(raw)
        if p['organizationId'] != self.cfg['organization_id'] or not delivery:
            raise ValueError('wrong organization or missing Linear-Delivery')
        kind, action = p['type'], p['action']
        if (kind, action) not in {('Issue', 'create'), ('Issue', 'update'), ('Comment', 'create')}:
            return 'ignored'
        d = p['data']
        issue = uid(d['id'] if kind == 'Issue' else d['issueId'])
        with self.db as db:
            db.execute('BEGIN IMMEDIATE')
            digest = hashlib.sha256(raw).hexdigest()
            old = db.execute('SELECT digest FROM deliveries WHERE delivery=?', (delivery,)).fetchone()
            if old:
                if old != (digest,):
                    raise ValueError('delivery ID reused for different content')
                return 'duplicate'
            db.execute('INSERT INTO deliveries VALUES(?,?)', (delivery, digest))
            if kind == 'Comment':
                comment = uid(d['id'])
                if db.execute('SELECT 1 FROM comments WHERE comment=?', (comment,)).fetchone():
                    return 'duplicate'
                thread, _ = self.root(issue)
                msg = self.action('comment:' + comment, type='reply_with_labels', thread=thread,
                                  text=d['body'], labels={'linear_comment_id': comment})['id']
                if type(msg) is not int or msg < 1:
                    raise ValueError('board did not return a reply ID')
                db.execute('INSERT INTO comments VALUES(?,?)', (comment, msg))
            else:
                stamp = dt.datetime.fromisoformat(d['updatedAt'].replace('Z', '+00:00'))
                if stamp.tzinfo is None:
                    raise ValueError('updatedAt missing timezone')
                version = stamp.astimezone(dt.timezone.utc).isoformat(timespec='microseconds')
                thread, previous = self.root(issue)
                if previous and previous >= version:
                    return 'stale'
                identifier = d.get('identifier') or f"{d['team']['key']}-{d['number']}"
                state = d['state']
                state_id = uid(d.get('stateId') or state['id'])
                text = f"{identifier}: {d['title']}\nState: {state['name']} ({state_id})"
                if d.get('description'):
                    text += '\n\n' + d['description']
                self.action('snapshot:' + issue + ':' + version, type='edit', id=thread, text=text)
                db.execute('UPDATE issues SET version=? WHERE issue=?', (version, issue))
        return 'imported'

    def poll(self):
        db = self.db
        after, = db.execute('SELECT cursor FROM config').fetchone()
        for e in self.board(f'/v1/events?after={after}&wait_ms=0'):
            seq = e['cursor']
            if type(seq) is not int or seq <= after:
                raise ValueError('invalid board cursor')
            target = None
            with db:
                db.execute('BEGIN IMMEDIATE')
                if db.execute('SELECT cursor FROM config').fetchone() != (after,):
                    return
                m = e.get('message', {})
                if (e['type'] == 'message' and m['channel'] == self.cfg['channel']
                        and m['parent'] and e['actor'] != self.cfg['board_actor']):
                    row = db.execute('SELECT issue FROM issues WHERE thread=?', (m['parent'],)).fetchone()
                    mid = m['id']
                    exp = db.execute('SELECT done FROM exports WHERE message=?', (mid,)).fetchone()
                    if exp == (0,):
                        raise RuntimeError(f'reconcile pending export {mid} before polling')
                    if row and not exp and not db.execute('SELECT 1 FROM comments WHERE message=?', (mid,)).fetchone():
                        issue, = row
                        text = m['text']
                        command = re.fullmatch(r'!linear-state ([0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12})', text.strip())
                        if command:
                            target = uid(command[1])
                            mutation = 'mutation($issue:String!,$id:String!){issueUpdate(id:$issue,input:{stateId:$id}){success}}'
                            variables = {'issue': issue, 'id': target}
                            field = 'issueUpdate'
                        else:
                            target = str(uuid.uuid5(uuid.NAMESPACE_URL, f'{self.binding}:comment:{mid}'))
                            db.execute('INSERT INTO comments VALUES(?,?)', (target, mid))
                            mutation = 'mutation($input:CommentCreateInput!){commentCreate(input:$input){success}}'
                            variables = {'input': {'id': target, 'issueId': issue, 'body': text}}
                            field = 'commentCreate'
                        db.execute('INSERT INTO exports VALUES(?,?,0)', (mid, target))
                if target is None:
                    db.execute('UPDATE config SET cursor=?', (seq,))
            if target:
                res = self.linear('/graphql', {'query': mutation, 'variables': variables})
                if res.get('errors') or res['data'][field]['success'] is not True:
                    raise RuntimeError(f'Linear did not confirm export {mid}')
                with db:
                    db.execute('UPDATE exports SET done=1 WHERE message=?', (mid,))
                    db.execute('UPDATE config SET cursor=max(cursor,?)', (seq,))
            after = seq

    def reconcile(self, mid, landed):
        with self.db as db:
            db.execute('BEGIN IMMEDIATE')
            if not db.execute('SELECT 1 FROM exports WHERE message=? AND done=0', (mid,)).fetchone():
                raise ValueError('not a pending export')
            if landed:
                db.execute('UPDATE exports SET done=1 WHERE message=?', (mid,))
            else:
                db.execute('DELETE FROM comments WHERE message=?', (mid,))
                db.execute('DELETE FROM exports WHERE message=?', (mid,))


class NoRedirect(request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        return None


def client(base, authorization):
    if parse.urlsplit(base).scheme != 'https':
        raise ValueError('HTTPS endpoint required')
    opener = request.build_opener(NoRedirect(), request.ProxyHandler({}))

    def send(path, body=None, headers=None):
        req = request.Request(base.rstrip('/') + path, None if body is None else json.dumps(body).encode(),
                              {'Authorization': authorization, 'Content-Type': 'application/json', **(headers or {})})
        with opener.open(req, timeout=30) as response:
            return json.load(response)
    return send


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('database')
    parser.add_argument('config')
    cmds = parser.add_subparsers(dest='cmd', required=True)
    hook = cmds.add_parser('webhook')
    hook.add_argument('delivery')
    hook.add_argument('signature')
    cmds.add_parser('poll')
    for cmd in ('confirm', 'retry-absent'):
        cmds.add_parser(cmd).add_argument('message', type=int)
    args = parser.parse_args()
    with open(args.config) as f:
        cfg = json.load(f)
    board = client(cfg['board_url'], 'Bearer ' + os.environ['NEOCHAT_TOKEN'])
    linear = client('https://api.linear.app', os.environ['LINEAR_API_KEY']) if args.cmd == 'poll' else None
    bridge = Bridge(args.database, cfg, board, linear)
    try:
        if args.cmd == 'webhook':
            print(bridge.webhook(args.delivery, args.signature, sys.stdin.buffer.read(),
                                 os.environ['LINEAR_WEBHOOK_SECRET'].encode()))
        elif args.cmd == 'poll':
            bridge.poll()
        else:
            bridge.reconcile(args.message, args.cmd == 'confirm')
    finally:
        bridge.db.close()


if __name__ == '__main__':
    main()