"""The login screen: a locked server, a wrong password, a right one, and signing out.

Run against a server with a password configured:

    WEB_DESKTOP_PASSWORD_HASH='<bcrypt of the passphrase>' WEB_DESKTOP_SECRET=... \
      cargo run -p server --release
    PROBE_URL=http://127.0.0.1:8099 PROBE_PASSWORD='<the passphrase>' \
      python3 crates/e2e/probes/probe_auth.py

The suite passes the passphrase in `PROBE_PASSWORD` and the matching hash to the
server, both from the `auth` scenario in `run.py`.
"""

import os
WORK = os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e"
os.makedirs(os.path.join(WORK, "shots"), exist_ok=True)

import json, os, sys, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.common.keys import Keys
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait
from selenium.webdriver.firefox.options import Options

BASE = os.environ.get("PROBE_URL", "http://127.0.0.1:8099/")
PASSWORD = os.environ.get("PROBE_PASSWORD", "correct horse")
opts = Options(); opts.add_argument("--headless"); opts.add_argument("--window-size=1400,950")
d = webdriver.Firefox(options=opts); d.set_window_size(1400, 950)
ok = []
def check(n, c, x=""):
    ok.append(bool(c)); print(("PASS  " if c else "FAIL  ") + n + (f"   [{x}]" if x else ""), flush=True)

def status_of(path):
    """Ask the API directly from the page, so the answer is the browser's own."""
    return d.execute_async_script(
        "const done = arguments[arguments.length - 1];"
        "fetch(arguments[0]).then(r => done(r.status)).catch(e => done('error: ' + e))",
        path)

def type_password(text):
    box = WebDriverWait(d, 20).until(
        EC.presence_of_element_located((By.CSS_SELECTOR, ".login-field input")))
    box.clear(); box.send_keys(text)

def wait_for_login():
    WebDriverWait(d, 20).until(
        EC.presence_of_element_located((By.CSS_SELECTOR, ".login-card")))

def wait_for_desktop():
    WebDriverWait(d, 30).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    WebDriverWait(d, 20).until(
        EC.invisibility_of_element_located((By.CSS_SELECTOR, ".login-card")))

try:
    d.get(BASE)
    wait_for_login()
    time.sleep(1.0)

    # 1. locked on arrival: the login card, and no shell behind it
    card = d.find_element(By.CSS_SELECTOR, ".login-card").text
    check("the login screen is what loads first", "Web Desktop" in card and "Unlock" in card,
          card.replace("\n", " / ")[:70])
    check("no taskbar is rendered while locked", not d.find_elements(By.CSS_SELECTOR, ".taskbar"))
    check("no start menu is rendered while locked", not d.find_elements(By.CSS_SELECTOR, ".start-btn"))
    d.save_screenshot(os.path.join(WORK, "shots", "70-login.png"))

    # 2. the API is closed too, not just the UI
    check("/api/session is open (the screen needs it)", status_of("/api/session") == 200,
          str(status_of("/api/session")))
    check("/api/catalog is refused", status_of("/api/catalog") == 401, str(status_of("/api/catalog")))
    check("/api/fs/list is refused", status_of("/api/fs/list?path=") == 401,
          str(status_of("/api/fs/list?path=")))
    session = json.loads(d.execute_async_script(
        "const done = arguments[arguments.length - 1];"
        "fetch('/api/session').then(r => r.text()).then(done)"))
    check("session says a password is required and none is held",
          session == {"authenticated": False, "enabled": True}, json.dumps(session))

    # 3. a wrong password is refused, in words, and the screen stays
    type_password("not the password")
    d.find_element(By.CSS_SELECTOR, ".login-btn").click()
    WebDriverWait(d, 20).until(
        EC.presence_of_element_located((By.CSS_SELECTOR, ".login-error")))
    error = d.find_element(By.CSS_SELECTOR, ".login-error").text
    check("a wrong password says so", "not accepted" in error, error)
    check("and does not let anyone in", not d.find_elements(By.CSS_SELECTOR, ".taskbar"))
    check("and the session cookie was not set",
          not any(c["name"] == "wd_session" for c in d.get_cookies()), str(d.get_cookies()))

    # 4. the right one opens the desktop
    type_password(PASSWORD)
    d.find_element(By.CSS_SELECTOR, ".login-field input").send_keys(Keys.ENTER)
    wait_for_desktop()
    time.sleep(1.5)
    check("the right password opens the desktop", len(d.find_elements(By.CSS_SELECTOR, ".taskbar")) == 1)
    check("/api/catalog is readable once signed in", status_of("/api/catalog") == 200,
          str(status_of("/api/catalog")))
    cookies = {c["name"]: c for c in d.get_cookies()}
    check("a session cookie was set", "wd_session" in cookies, str(list(cookies)))
    check("the cookie is HTTP-only", cookies.get("wd_session", {}).get("httpOnly") is True,
          str(cookies.get("wd_session")))
    check("the cookie is not readable from JavaScript", "wd_session" not in d.execute_script("return document.cookie"),
          d.execute_script("return document.cookie"))
    d.save_screenshot(os.path.join(WORK, "shots", "71-unlocked.png"))

    # 5. the session survives a reload
    d.get(BASE)
    wait_for_desktop()
    time.sleep(1.2)
    check("a reload stays signed in", not d.find_elements(By.CSS_SELECTOR, ".login-card"))

    # 6. Settings reports the state and can sign out
    d.find_element(By.CSS_SELECTOR, ".start-btn").click(); time.sleep(0.5)
    for it in d.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item"):
        if "Settings" in it.text: it.click(); break
    WebDriverWait(d, 20).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".settings")))
    panes = {p.find_element(By.TAG_NAME, "h3").text.strip().lower(): p
             for p in d.find_elements(By.CSS_SELECTOR, ".settings .pane")}
    account = panes.get("account", None)
    check("Settings has an Account pane", account is not None, str(list(panes)))
    check("it says signed in", "signed in" in account.text, account.text.replace("\n", " / ")[:80])
    account.find_element(By.CSS_SELECTOR, ".ghost-btn").click()
    wait_for_login()
    time.sleep(1.0)
    check("signing out puts the login screen back", len(d.find_elements(By.CSS_SELECTOR, ".login-card")) == 1)
    check("and closes the windows behind it", not d.find_elements(By.CSS_SELECTOR, ".window"))
    check("and the API is refused again", status_of("/api/catalog") == 401,
          str(status_of("/api/catalog")))

    # 7. a session that disappears under an open desktop brings the login screen back:
    #    the cookie is deleted behind the shell's back, which is what expiry looks like
    #    from inside the page, and the next failure is re-checked rather than just shown
    type_password(PASSWORD)
    d.find_element(By.CSS_SELECTOR, ".login-btn").click()
    wait_for_desktop()
    time.sleep(1.2)
    d.delete_all_cookies()
    d.find_element(By.CSS_SELECTOR, ".start-btn").click(); time.sleep(0.5)
    for it in d.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item"):
        if "Settings" in it.text: it.click(); break
    WebDriverWait(d, 20).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".settings")))
    panes = {p.find_element(By.TAG_NAME, "h3").text.strip().lower(): p
             for p in d.find_elements(By.CSS_SELECTOR, ".settings .pane")}
    panes["backend"].find_element(By.CSS_SELECTOR, ".ghost-btn").click()
    wait_for_login()
    time.sleep(0.8)
    check("a session that ran out is noticed, not just reported as an error",
          len(d.find_elements(By.CSS_SELECTOR, ".login-card")) == 1)
    check("and the desktop behind it is closed", not d.find_elements(By.CSS_SELECTOR, ".window"))

    # 8. a wrong password after that is still refused
    type_password("still not the password")
    d.find_element(By.CSS_SELECTOR, ".login-btn").click()
    WebDriverWait(d, 20).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".login-error")))
    check("a wrong password is refused the second time too",
          not d.find_elements(By.CSS_SELECTOR, ".taskbar"))
    d.save_screenshot(os.path.join(WORK, "shots", "72-signed-out.png"))
finally:
    d.quit()
print(f"\n{sum(ok)}/{len(ok)} login checks passed")
sys.exit(0 if all(ok) else 1)
