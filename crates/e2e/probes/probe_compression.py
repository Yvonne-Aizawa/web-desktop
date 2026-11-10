#!/usr/bin/env python3
"""Check what the shell actually costs over the wire."""
import os, re, sys, gzip, brotli, urllib.request, urllib.error

BASE = (sys.argv[1] if len(sys.argv) > 1
        else os.environ.get("PROBE_URL", "http://127.0.0.1:8080")).rstrip("/")
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

# the dist uses hash-named assets, so read the real names out of the shell
html = get("/")[1].decode("utf-8", "replace")
def asset(pattern):
    m = re.search(r"['\"/]([A-Za-z0-9_.-]+" + pattern + r")['\"]", html)
    if not m:
        raise RuntimeError(f"index.html references no {pattern} asset")
    return "/" + m.group(1)
WASM = asset(r"\.wasm")
JS = asset(r"\.js")

identity = get(WASM)[1]
ship = get(WASM, "br")[1]
check("wasm is small enough to ship", len(ship) < 1_200_000,
      f"{len(ship)} bytes over the wire, {len(identity)} raw")

for enc in ("gzip", "br"):
    s, body, h = get(WASM, enc)
    ratio = len(identity) / max(1, len(body))
    check(f"wasm served as {enc}", s == 200 and h.get("content-encoding") == enc,
          f"{h.get('content-encoding')} {len(body)} bytes, {ratio:.1f}x smaller")
    plain = gzip.decompress(body) if enc == "gzip" else brotli.decompress(body)
    check(f"{enc} body decompresses to the same bytes", plain == identity,
          f"{len(plain)} bytes back")

s, body, h = get("/", "gzip")
check("the html shell is compressed", h.get("content-encoding") == "gzip", h.get("content-encoding", "none"))
s, body, h = get("/api/catalog", "gzip")
check("json is compressed", h.get("content-encoding") == "gzip", h.get("content-encoding", "none"))

# The compression predicate skips images, so an unchanged PNG does not pay for gzip.
s, body, h = get("/api/fs/preview?path=notes%2Ftodo.md", "gzip")
check("text previews are compressed", h.get("content-encoding") == "gzip", h.get("content-encoding", "none"))

total = len(ship) + len(get(JS, "br")[1])
print(f"\nfirst load of the shell: ~{total // 1024} KB (brotli, wasm + js)")
print(f"{sum(ok)}/{len(ok)} compression checks passed")
sys.exit(0 if all(ok) else 1)
