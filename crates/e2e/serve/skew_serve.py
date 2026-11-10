import os
WORK = os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e"
REPO = os.environ.get("E2E_REPO") or os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
os.makedirs(os.path.join(WORK, "shots"), exist_ok=True)
#!/usr/bin/env python3
"""Serve the built shell with a *skewed* catalog, to exercise version skew.

  serve/skew_serve.py <port> unknown-app   -> catalog with an app the shell cannot run
  serve/skew_serve.py <port> broken        -> catalog the shell cannot decode at all
"""
import functools, http.server, json, mimetypes, socketserver, sys

DIST = os.environ.get("E2E_DIST") or os.path.join(os.environ.get("E2E_REPO","."), "crates/client/dist")
PORT = int(sys.argv[1])
MODE = sys.argv[2]

mimetypes.add_type("application/wasm", ".wasm")
mimetypes.add_type("text/javascript", ".js")

BASE_APPS = [
    {"id": "terminal", "name": "Terminal", "glyph": ">_", "accent": "#4ade80",
     "blurb": "Shell with a handful of built-ins", "default_width": 680, "default_height": 420},
    {"id": "files", "name": "Files", "glyph": "\U0001f5c2", "accent": "#60a5fa",
     "blurb": "Browse the virtual filesystem", "default_width": 760, "default_height": 480},
    {"id": "settings", "name": "Settings", "glyph": "\u2699", "accent": "#c084fc",
     "blurb": "Wallpaper, connectivity, layout", "default_width": 620, "default_height": 460},
    {"id": "about", "name": "About", "glyph": "\u25cd", "accent": "#fbbf24",
     "blurb": "Build info and credits", "default_width": 460, "default_height": 400},
]

# An app only a newer server would know about.
EXTRA = {
    "id": "music", "name": "Music", "glyph": "\u266b", "accent": "#f472b6",
    "blurb": "Plays things, served from a newer build", "default_width": 520, "default_height": 360,
}


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=DIST, **kwargs)

    def log_message(self, *args):
        pass

    def _json(self, payload, status=200):
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/api/health":
            return self._json({"name": "web-desktop", "version": "0.1.0", "uptime_secs": 12})
        if path == "/api/catalog":
            if MODE == "broken":
                # Decodable JSON, wrong shape: `default_width` is a string and an
                # app has no glyph, so the client's serde must reject it.
                apps = [dict(BASE_APPS[0], default_width="680")]
                return self._json({"hostname": "web-desktop", "apps": apps, "wallpapers": [],
                                   "fs": {"enabled": False, "root": "", "writable": False,
                                          "max_preview_bytes": 0}})
            apps = BASE_APPS + [EXTRA]
            return self._json({"hostname": "web-desktop", "apps": apps, "wallpapers": [],
                               "fs": {"enabled": False, "root": "", "writable": False,
                                      "max_preview_bytes": 0}})
        return super().do_GET()


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


print(f"skew server ({MODE}) on http://127.0.0.1:{PORT}", flush=True)
Server(("127.0.0.1", PORT), functools.partial(Handler)).serve_forever()
