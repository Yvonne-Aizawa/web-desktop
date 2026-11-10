
import os, subprocess, sys, time
from selenium import webdriver
from selenium.webdriver.common.action_chains import ActionChains
from selenium.webdriver.common.by import By
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait
from selenium.webdriver.firefox.options import Options

BASE = os.environ.get("PROBE_URL", "http://127.0.0.1:8080/")
ROOT = os.environ.get("PROBE_ROOT", os.environ.get("PROBE_ROOT") or "/tmp/web-desktop-e2e/root")
SHOTS = os.path.join(os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e", "shots"); os.makedirs(SHOTS, exist_ok=True)

folder = f"{ROOT}/openwith"
subprocess.run(["rm", "-rf", folder], capture_output=True)
os.makedirs(folder, exist_ok=True)
open(f"{folder}/prose.txt", "w").write("a text file\n")
subprocess.run(["magick", "-size", "160x120", "xc:gold", f"{folder}/photo.png"], check=True)
subprocess.run(["magick", "-size", "160x120", "xc:teal", f"{folder}/scan.tiff"], check=True)

opts = Options(); opts.add_argument("--headless"); opts.add_argument("--window-size=1500,1000")
driver = webdriver.Firefox(options=opts); driver.set_window_size(1500, 1000)
results = []
def check(n, c, x=""):
    results.append((n, bool(c)))
    print(("PASS  " if c else "FAIL  ") + n + (f"   [{x}]" if x else ""), flush=True)

def open_files():
    driver.find_element(By.CSS_SELECTOR, ".start-btn").click()
    WebDriverWait(driver, 10).until(EC.visibility_of_element_located((By.CSS_SELECTOR, ".start-menu")))
    time.sleep(0.4)
    for it in driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item"):
        if it.text.startswith("Files") or "\nFiles\n" in it.text:
            it.click(); break
    WebDriverWait(driver, 20).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".files-list .row")))
    time.sleep(0.8)

def row(name):
    for r in driver.find_elements(By.CSS_SELECTOR, ".files-list .row"):
        if r.find_element(By.CSS_SELECTOR, ".row-name").text == name:
            return r

def open_button():
    for b in driver.find_elements(By.CSS_SELECTOR, ".preview-head .ghost-btn"):
        if b.text.startswith("Open in"):
            return b
    return None

def titles():
    return [t.text for t in driver.find_elements(By.CSS_SELECTOR, ".tb-title")]

def raise_window(name):
    for b in driver.find_elements(By.CSS_SELECTOR, ".task-btn"):
        if name in b.text and "active" not in (b.get_attribute("class") or ""):
            b.click(); time.sleep(0.8); return

try:
    driver.get(BASE)
    WebDriverWait(driver, 30).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(2.5)
    open_files()
    row("openwith").click(); time.sleep(1.2)

    # text -> Text Editor
    row("prose.txt").click(); time.sleep(1.0)
    b = open_button()
    check("a text file offers the Text Editor", b is not None and b.text == "Open in Text Editor",
          b.text if b else "none")
    b.click(); time.sleep(2.0)
    check("it opens the Text Editor with the file",
          "Text Editor" in titles()
          and "prose.txt" in [t.find_element(By.CSS_SELECTOR, ".tab-name").text
                              for t in driver.find_elements(By.CSS_SELECTOR, ".editor-tab")],
          str(titles()))

    # image -> Gallery
    raise_window("Files")
    row("photo.png").click(); time.sleep(1.0)
    b = open_button()
    check("an image offers the Gallery", b is not None and b.text == "Open in Gallery",
          b.text if b else "none")
    b.click(); time.sleep(2.5)
    check("it opens the Gallery on that photo",
          "Gallery" in titles() and "photo.png" in driver.find_element(By.CSS_SELECTOR, ".viewer-name").text,
          str(titles()) + " / " + driver.find_element(By.CSS_SELECTOR, ".viewer-name").text)
    check("the viewer decoded it",
          driver.execute_script("const i=document.querySelector('.viewer-image');"
                               "return i && i.complete && i.naturalWidth===160 && i.naturalHeight===120"))
    driver.save_screenshot(f"{SHOTS}/82-open-with.png")

    # a picture no browser decodes goes to the Gallery, which explains itself
    raise_window("Files")
    row("scan.tiff").click(); time.sleep(1.0)
    b = open_button()
    check("an undecodable image still offers the Gallery", b is not None and b.text == "Open in Gallery",
          b.text if b else "none")
    b.click(); time.sleep(2.5)
    check("the Gallery says it cannot show that format",
          "cannot show" in driver.find_element(By.CSS_SELECTOR, ".files-notice").text,
          driver.find_element(By.CSS_SELECTOR, ".files-notice").text)
    check("and no viewer is opened over it", not driver.find_elements(By.CSS_SELECTOR, ".gallery-viewer"))

    # double-clicking a row opens it in the same app
    raise_window("Files")
    ActionChains(driver).double_click(row("prose.txt")).perform()
    time.sleep(2.0)
    check("double-clicking a text row opens the editor",
          len([t for t in driver.find_elements(By.CSS_SELECTOR, ".editor-tab")]) >= 1,
          str(titles()))
    check("app still mounted", len(driver.find_elements(By.CSS_SELECTOR, ".taskbar")) == 1)
finally:
    driver.quit()
    subprocess.run(["rm", "-rf", folder], capture_output=True)

failed = [n for n, ok in results if not ok]
print(f"\n{len(results) - len(failed)}/{len(results)} open-with checks passed")
if failed:
    print("failed:", failed)
sys.exit(1 if failed else 0)
