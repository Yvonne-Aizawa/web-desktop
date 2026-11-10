import os
WORK = os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e"
REPO = os.environ.get("E2E_REPO") or os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
os.makedirs(os.path.join(WORK, "shots"), exist_ok=True)

import os, sys, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.firefox.options import Options
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait

opts = Options(); opts.add_argument("--headless"); opts.add_argument("--window-size=1400,950")
d = webdriver.Firefox(options=opts); d.set_window_size(1400, 950)
ok = []
def check(n, c, x=""):
    ok.append(c); print(("PASS  " if c else "FAIL  ") + n + (f"   [{x}]" if x else ""), flush=True)
try:
    d.get(os.environ.get("PROBE_URL", "http://127.0.0.1:8096/"))
    WebDriverWait(d, 30).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(2.5)
    status = d.find_element(By.CSS_SELECTOR, ".tray-dot").get_attribute("title") or ""
    check("an undecodable catalog is reported, not swallowed by the health check",
          "catalog" in status, status[:80])
    d.find_element(By.CSS_SELECTOR, ".start-btn").click(); time.sleep(0.6)
    items = d.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item")
    names = [i.text.split("\n")[1] for i in items if "\n" in i.text]
    check("the desktop still falls back to its own catalog",
          {"Terminal", "Files", "Text Editor", "Settings", "About"} <= set(names), str(names))
    for it in items:
        if "Terminal" in it.text:
            it.click(); break
    WebDriverWait(d, 10).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".window .terminal")))
    check("apps still work with a broken catalog", True)
    d.save_screenshot(os.path.join(WORK, "shots", "41-skew-broken-catalog.png"))
finally:
    d.quit()
print(f"\n{sum(ok)}/{len(ok)} broken-catalog checks passed")
