"""Run the read-only NeoBoard guides on loopback. Python 3.9+, stdlib only."""

import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit
from urllib.request import urlopen

SITE = Path(__file__).resolve().parent
DOCS = SITE.parent / 'docs'
PAGES = ('start', 'boards', 'tasks', 'agents', 'operations')
VERSION = '1.77.0'
DIGEST = '4f387c930903fe6b63b3497ebf02ca65005d52016c72dcc8bfc44ac662986cb3'
URL = f'https://cdn.jsdelivr.net/npm/@scalar/api-reference@{VERSION}/dist/browser/standalone.js'


def scalar_bundle():
    cache = SITE / '.cache' / f'scalar-{VERSION}.js'
    if cache.exists():
        data = cache.read_bytes()
    else:
        with urlopen(URL, timeout=60) as response:
            data = response.read()
    if hashlib.sha256(data).hexdigest() != DIGEST:
        raise SystemExit(f'Scalar checksum mismatch. Remove {cache} if present and retry.')
    if not cache.exists():
        cache.parent.mkdir(exist_ok=True)
        cache.write_bytes(data)
    return data


class Preview(BaseHTTPRequestHandler):
    def do_GET(self):
        path = urlsplit(self.path).path
        if path == '/':
            self.send_response(302)
            self.send_header('Location', '/docs/start.md')
            self.end_headers()
            return
        routes = {'/app.js': (SITE / 'app.js', 'text/javascript')}
        for page in PAGES:
            routes[f'/docs/{page}.md'] = (SITE / 'index.html', 'text/html')
            routes[f'/source/{page}.md'] = (DOCS / f'{page}.md', 'text/plain')
        if path == '/scalar.js':
            data, mime = self.server.scalar, 'text/javascript'
        elif path in routes:
            file, mime = routes[path]
            try:
                data = file.read_bytes()
            except FileNotFoundError:
                self.send_error(404)
                return
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header('Content-Type', mime + '; charset=utf-8')
        self.send_header('Content-Length', str(len(data)))
        self.send_header('Cache-Control', 'no-store')
        self.send_header('Content-Security-Policy', "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; font-src 'self' data:; form-action 'none'; base-uri 'none'; frame-ancestors 'none'")
        self.end_headers()
        self.wfile.write(data)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=8765)
    args = parser.parse_args()
    bundle = scalar_bundle()
    with ThreadingHTTPServer(('127.0.0.1', args.port), Preview) as server:
        server.scalar = bundle
        print(f'NeoBoard docs: http://127.0.0.1:{server.server_port}/', flush=True)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass