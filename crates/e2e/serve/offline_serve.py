import os, sys

import http.server, socketserver, functools, mimetypes
mimetypes.add_type("application/wasm", ".wasm")
mimetypes.add_type("text/javascript", ".js")
H = functools.partial(http.server.SimpleHTTPRequestHandler, directory=os.environ.get("E2E_DIST") or os.path.join(os.environ.get("E2E_REPO","."), "crates/client/dist"))
class S(socketserver.TCPServer):
    allow_reuse_address = True
S(("127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 8090), H).serve_forever()
