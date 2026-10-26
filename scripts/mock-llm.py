import json, time
from http.server import BaseHTTPRequestHandler, HTTPServer

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        n = int(self.headers.get('Content-Length', 0))
        body = json.loads(self.rfile.read(n) or b'{}')
        msgs = body.get('messages', [])
        last = msgs[-1].get('content', '') if msgs else ''
        text = f"嗯，我听到了。你刚才说的是「{last[:18]}」。"
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        for ch in text:
            data = json.dumps({"choices": [{"delta": {"content": ch}}]}, ensure_ascii=False)
            self.wfile.write(f"data: {data}\n\n".encode())
            self.wfile.flush()
            time.sleep(0.03)
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

HTTPServer(('127.0.0.1', 8787), H).serve_forever()
