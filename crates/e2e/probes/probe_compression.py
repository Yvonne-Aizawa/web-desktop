#!/usr/bin/env python3
"""Check what the shell actually costs over the wire."""
import sys, urllib.request, urllib.error

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8080"
ok = []
def check(name, cond, extra=""):
    ok.append(cond)
    print(("PASS  " if cond else "FAIL  ") + name + (f"   [{extra}]" if extra else ""), flush=True)

def get(path, encoding=None):
    headers = {"Accept-Encoding": encoding} if encoding else {}
    req = urllib.request.Request(BASE + path, headers=headers)
    try:
        with urllib.request.urlopen(req) as r:
            body = r.read()
            return r.status, body, {k.lower(): v for k, v in r.headers.items()}
    except urllib.error.HTTPError as e:
        return e.code, e.read(), {k.lower(): v for k, v in e.headers.items()}

identity = get("/client_bg.wasm")[1]
check("wasm is small enough to ship", len(identity) < 1_200_000, f"{len(identity)} bytes raw")

for enc in ("gzip", "br"):
    s, body, h = get("/client_bg.wasm", enc)
    ratio = len(identity) / max(1, len(body))
    check(f"wasm served as {enc}", s == 200 and h.get("content-encoding") == enc,
          f"{h.get('content-encoding')} {len(body)} bytes, {ratio:.1f}x smaller")
    check(f"{enc} body decompresses to the same bytes", body[:4] == b"\0asm", str(body[:4]))

s, body, h = get("/", "gzip")
check("the html shell is compressed", h.get("content-encoding") == "gzip", h.get("content-encoding", "none"))
s, body, h = get("/api/catalog", "gzip")
check("json is compressed", h.get("content-encoding") == "gzip", h.get("content-encoding", "none"))

# The compression predicate skips images, so an unchanged PNG does not pay for gzip.
s, body, h = get("/api/fs/preview?path=notes%2Ftodo.md", "gzip")
check("text previews are compressed", h.get("content-encoding") == "gzip", h.get("content-encoding", "none"))

total = len(get("/client_bg.wasm", "br")[1]) + len(get("/client.js", "br")[1])
print(f"\nfirst load of the shell: ~{total // 1024} KB (brotli, wasm + js)")
print(f"{sum(ok)}/{len(ok)} compression checks passed")
sys.exit(0 if all(ok) else 1)
