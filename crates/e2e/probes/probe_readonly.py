import os
WORK = os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e"
REPO = os.environ.get("E2E_REPO") or os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
os.makedirs(os.path.join(WORK, "shots"), exist_ok=True)

import os, sys, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait
from selenium.webdriver.firefox.options import Options

BASE = os.environ.get("PROBE_URL", "http://127.0.0.1:8098/")
opts = Options(); opts.add_argument("--headless"); opts.add_argument("--window-size=1400,950")
d = webdriver.Firefox(options=opts); d.set_window_size(1400, 950)
ok = []
def check(n, c, x=""):
    ok.append(bool(c)); print(("PASS  " if c else "FAIL  ") + n + (f"   [{x}]" if x else ""), flush=True)
try:
    d.get(BASE)
    WebDriverWait(d, 30).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(2)
    d.find_element(By.CSS_SELECTOR, ".start-btn").click(); time.sleep(0.5)
    for it in d.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item"):
        if "Files" in it.text: it.click(); break
    WebDriverWait(d, 20).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".files-list .row")))
    time.sleep(0.6)

    labels = [b.text for b in d.find_elements(By.CSS_SELECTOR, ".files-bar .ghost-btn")]
    check("no write actions are offered", not any(x in " ".join(labels) for x in ("Folder", "File", "Upload")), str(labels))
    side = d.find_element(By.CSS_SELECTOR, ".files-side").text
    check("the sidebar explains why", "Read-only" in side and "WEB_DESKTOP_FS_WRITE=on" in side, side[-90:].replace("\n", " / "))

    # open a text file: the box is there but not editable, and there is no save
    for r in d.find_elements(By.CSS_SELECTOR, ".files-list .row"):
        if r.find_element(By.CSS_SELECTOR, ".row-name").text == "notes.txt":
            r.click(); break
    time.sleep(1.2)
    shown = d.find_element(By.CSS_SELECTOR, ".files-preview pre").text
    check("the text is shown read-only", "read-only file" in shown, repr(shown)[:40])
    check("there is no editor anywhere in Files",
          not d.find_elements(By.CSS_SELECTOR, ".files-preview textarea"))
    # Handing a file to the editor is a *read*, so it stays available here; the
    # editor is what turns read-only on for itself.
    handoff = [b for b in d.find_elements(By.CSS_SELECTOR, ".preview-head .ghost-btn")
               if b.is_displayed() and "Open in" in b.text]
    check("opening in the editor is still offered", len(handoff) == 1, str([b.text for b in handoff]))
    handoff[0].click()
    time.sleep(2.0)
    box = d.find_elements(By.CSS_SELECTOR, ".editor-textarea")
    check("the editor opens it read-only", len(box) == 1 and box[0].get_attribute("readonly") is not None,
          str(len(box)))
    bar = d.find_element(By.CSS_SELECTOR, ".editor-bar").text
    check("and offers no save", "Save" not in bar, bar.replace("\n", " / "))
    head = d.find_element(By.CSS_SELECTOR, ".preview-head").text
    check("the head says read-only", "read-only" in head, head.replace("\n", " / "))
    # the actions are rendered but hidden, so assert on what is *displayed*
    visible = [b.text for b in d.find_elements(By.CSS_SELECTOR, ".preview-actions .ghost-btn")
               if b.is_displayed()]
    check("no rename or delete is offered on the selected file", visible == [], str(visible))
    d.save_screenshot(os.path.join(WORK, "shots", "62-readonly.png"))
finally:
    d.quit()
print(f"\n{sum(ok)}/{len(ok)} read-only checks passed")
sys.exit(0 if all(ok) else 1)
