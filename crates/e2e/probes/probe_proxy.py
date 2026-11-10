
"""Fetch through the proxy from the page, the way an app's client would."""
import os, sys, time, urllib.parse
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait
from selenium.webdriver.firefox.options import Options

BASE = os.environ.get("PROBE_URL", "http://127.0.0.1:8096/")
opts = Options(); opts.add_argument("--headless"); opts.add_argument("--window-size=1200,900")
d = webdriver.Firefox(options=opts)
ok = []
def check(n, c, x=""):
    ok.append(bool(c)); print(("PASS  " if c else "FAIL  ") + n + (f"   [{x}]" if x else ""), flush=True)

def proxied(url):
    """Exactly the URL app_sdk::fetch builds."""
    endpoint = "/api/proxy?url=" + urllib.parse.quote(url, safe="")
    return d.execute_async_script("""
        const [endpoint, done] = arguments;
        fetch(endpoint).then(async r => done({status: r.status, body: await r.text()}))
                       .catch(e => done({error: String(e)}));
    """, endpoint)

try:
    d.get(BASE)
    WebDriverWait(d, 30).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(2.0)

    r = proxied("http://127.0.0.1:8099/cors-test")
    check("the page can read a body with no CORS headers via the proxy",
          r.get("status") == 200 and '"ok": true' in r.get("body", ""), str(r)[:90])
    r = proxied("http://127.0.0.1:8099/redirect")
    check("a redirect is followed for the caller too", r.get("status") == 200, str(r)[:60])
    r = proxied("https://example.com/")
    check("a host outside the list is refused, with the reason",
          r.get("status") == 403 and "WEB_DESKTOP_PROXY_HOSTS" in r.get("body", ""), str(r)[:90])

    # the catalog advertises the policy, which is how Settings and an app can tell
    info = d.execute_script("""
        const cb = arguments[arguments.length - 1];
        fetch('/api/catalog').then(r => r.json()).then(j => cb(j.proxy)).catch(e => cb({error: String(e)}));
    """)
    check("the catalog advertises the proxy policy",
          info.get("hosts") == ["127.0.0.1", "localhost"] and info.get("allow_private") is True, str(info)[:80])
finally:
    d.quit()
print(f"\n{sum(ok)}/{len(ok)} proxy-in-the-page checks passed")
sys.exit(0 if all(ok) else 1)
