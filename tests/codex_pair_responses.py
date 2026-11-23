"""No model or credentials. Only offer the single precomputed command for the exact user turn."""
from http.server import BaseHTTPRequestHandler
import json
from threading import Event
from uuid import uuid4


def responses(prompt, command):
    class Handler(BaseHTTPRequestHandler):
        calls = receipts = 0
        ready, proceed = Event(), Event()

        def do_POST(self):
            if self.path != '/v1/responses':
                return self.send_error(404)
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            items = body['input']
            texts = [part.get('text') for item in items if item.get('role') == 'user'
                     for part in item['content'] if isinstance(part, dict)]
            rid = str(uuid4())
            output = dict(type='message', role='assistant', id='msg_'+rid,
                          content=[dict(type='output_text', text='Done.')])
            if any(i.get('type') == 'function_call_output' and i.get('call_id') == 'pair' for i in items):
                Handler.receipts += 1
            elif prompt in texts:
                if Handler.calls:
                    return self.send_error(409, 'unexpected repeated call')
                output = dict(type='function_call', name='exec_command', call_id='pair',
                              arguments=json.dumps(dict(cmd=command, login=False)))
                for tool in body['tools']:
                    if tool.get('name') == 'exec_command':
                        break
                    if any(t.get('name') == 'exec_command' for t in tool.get('tools', [])):
                        output['namespace'] = tool['name']
                        break
                else:
                    return self.send_error(400, 'expected an exec_command tool')
                Handler.calls += 1
                Handler.ready.set()
                if not Handler.proceed.wait(100):
                    return self.send_error(408, 'decoy did not complete')
            events = [dict(type='response.created', response=dict(id=rid)),
                      dict(type='response.output_item.done', item=output),
                      dict(type='response.completed', response=dict(id=rid, usage=dict(input_tokens=0, output_tokens=0, total_tokens=0)))]
            data = ''.join('data: '+json.dumps(e)+'\n\n' for e in events).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, *args):
            pass

    return Handler