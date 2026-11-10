
"""A stand-in third-party API: no CORS headers, a redirect, and a big body."""
import http.server, socketserver, json
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        if self.path == "/cors-test":
            body = json.dumps({"ok": True, "note": "no CORS headers here"}).encode()
            self.send_response(200); self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
        elif self.path == "/redirect":
            self.send_response(302); self.send_header("location", "/cors-test"); self.end_headers()
        elif self.path == "/redirect-away":
            self.send_response(302); self.send_header("location", "http://169.254.169.254/latest/meta-data/"); self.end_headers()
        elif self.path == "/big":
            body = b"x" * (3 * 1024 * 1024)
            self.send_response(200); self.send_header("content-type", "application/octet-stream")
            self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
        else:
            self.send_response(404); self.end_headers()
class S(socketserver.ThreadingTCPServer):
    allow_reuse_address = True; daemon_threads = True
S(("127.0.0.1", 8099), H).serve_forever()
