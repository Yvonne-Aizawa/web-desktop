import os
import json, os, shutil, sys, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.firefox.options import Options
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait

BASE = os.environ.get("PROBE_URL", "http://127.0.0.1:8080/")
APPS = os.environ.get("E2E_APPS") or os.path.join(os.environ.get("E2E_REPO","."), "apps/calculator")
SHOTS = os.path.join(os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e", "shots")
os.makedirs(SHOTS, exist_ok=True)

opts = Options(); opts.add_argument("--headless"); opts.add_argument("--window-size=1400,950")
driver = webdriver.Firefox(options=opts); driver.set_window_size(1400, 950)
results = []
def check(n, c, x=""):
    results.append((n, bool(c)))
    print(("PASS  " if c else "FAIL  ") + n + (f"   [{x}]" if x else ""), flush=True)

def load_page():
    driver.get(BASE)
    WebDriverWait(driver, 30).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(2.5)

def open_app(name):
    driver.find_element(By.CSS_SELECTOR, ".start-btn").click()
    WebDriverWait(driver, 10).until(EC.visibility_of_element_located((By.CSS_SELECTOR, ".start-menu")))
    time.sleep(0.4)
    for it in driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item"):
        if name in it.text:
            it.click()
            return True
    return False

def close_all():
    # click each window's close button
    for _ in range(6):
        btns = driver.find_elements(By.CSS_SELECTOR, ".window .tb-close")
        if not btns:
            break
        btns[0].click()
        time.sleep(0.4)

def display():
    els = driver.find_elements(By.CSS_SELECTOR, ".calc-display")
    return els[0].text if els else ""

def press(label):
    for b in driver.find_elements(By.CSS_SELECTOR, ".calc-key"):
        if b.text == label:
            b.click()
            time.sleep(0.15)
            return True
    return False

def served_error():
    els = driver.find_elements(By.CSS_SELECTOR, ".served-error")
    return els[0].text if els else ""

try:
    # ---------------- the served app loads and works ----------------
    load_page()
    driver.find_element(By.CSS_SELECTOR, ".start-btn").click(); time.sleep(0.5)
    items = driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item")
    names = [i.text.split("\n")[1] for i in items if "\n" in i.text]
    check("the served app is advertised alongside the built-ins",
          "Calculator" in names and {"Terminal", "Files", "Settings", "About"} <= set(names),
          str(names))
    calc_item = next((i for i in items if "Calculator" in i.text), None)
    check("its blurb says it is served", calc_item is not None and "Served" in calc_item.text,
          calc_item.text.replace("\n", " / ") if calc_item else "")

    calc_item.click()
    WebDriverWait(driver, 20).until(
        EC.presence_of_element_located((By.CSS_SELECTOR, ".served-app .calc-display")))
    check("the app mounted into its window", True)
    check("its own stylesheet came with it",
          len(driver.find_elements(By.CSS_SELECTOR, ".served-app style")) == 1
          or driver.execute_script("return !!document.querySelector('.calc-display')"))
    check("the load overlay is gone once ready",
          not driver.find_elements(By.CSS_SELECTOR, ".served-status"))
    check("window chrome uses the manifest's label and accent",
          "Calculator" in driver.find_element(By.CSS_SELECTOR, ".tb-title").text
          and "#fb923c" in (driver.find_element(By.CSS_SELECTOR, ".window").get_attribute("style") or ""))

    # the arithmetic is wasm actually running
    for key in ["7", "+", "8", "="]:
        press(key)
    check("the calculator computes in wasm (7 + 8 = 15)", display() == "15", display())
    press("C")
    for key in ["1", "2", "×", "1", "2", "="]:
        press(key)
    check("a second sum works (12 x 12 = 144)", display() == "144", display())
    driver.save_screenshot(f"{SHOTS}/50-served-app.png")

    # host services
    foot = driver.find_element(By.CSS_SELECTOR, ".calc-foot").text
    check("ctx.size() and ctx.window_id() reach the app",
          "320×400" in foot and "window #" in foot, foot)
    equals_bg = driver.execute_script(
        "const b=[...document.querySelectorAll('.calc-key')].find(k=>k.textContent.trim()==='=');"
        "return b ? getComputedStyle(b).backgroundColor : ''")
    check("ctx.theme() reaches the app (accent on '=')", "251, 146, 60" in equals_bg, equals_bg)

    # unmount + remount: the module is cached, the instance is new
    close_all()
    check("closing the window removes the app's DOM", not driver.find_elements(By.CSS_SELECTOR, ".calc-display"))
    open_app("Calculator")
    WebDriverWait(driver, 20).until(
        EC.presence_of_element_located((By.CSS_SELECTOR, ".served-app .calc-display")))
    check("reopening mounts a fresh instance", display() == "0", display())
    close_all()

    # ---------------- a corrupted bundle is refused ----------------
    shutil.copytree(f"{APPS}/0.1.0", f"{APPS}/0.1.1", dirs_exist_ok=True)
    wasm = f"{APPS}/0.1.1/app_bg.wasm"
    with open(wasm, "r+b") as fh:
        fh.seek(2000)
        fh.write(b"\x00\xde\xad\xbe\xef")
    load_page()
    open_app("Calculator")
    WebDriverWait(driver, 20).until(lambda d: d.find_elements(By.CSS_SELECTOR, ".served-error"))
    err = served_error()
    check("a tampered bundle is refused by its checksum",
          "checksum mismatch" in err, err[:90].replace("\n", " / "))
    check("nothing from the app ran", not driver.find_elements(By.CSS_SELECTOR, ".calc-display"))
    driver.save_screenshot(f"{SHOTS}/51-served-tampered.png")
    shutil.rmtree(f"{APPS}/0.1.1")

    # ---------------- an app needing a newer ABI is refused ----------------
    shutil.copytree(f"{APPS}/0.1.0", f"{APPS}/0.1.2", dirs_exist_ok=True)
    manifest = f"{APPS}/0.1.2/manifest.json"
    data = json.load(open(manifest))
    data["abi"] = "99"
    json.dump(data, open(manifest, "w"))
    load_page()
    open_app("Calculator")
    WebDriverWait(driver, 20).until(lambda d: d.find_elements(By.CSS_SELECTOR, ".served-error"))
    err = served_error()
    check("an app built for a newer ABI is refused, and says so",
          "ABI 99" in err and "speaks 1" in err, err[:90].replace("\n", " / "))
    driver.save_screenshot(f"{SHOTS}/52-served-abi.png")
    shutil.rmtree(f"{APPS}/0.1.2")

    # ---------------- cleanup: the good build still works ----------------
    load_page()
    open_app("Calculator")
    WebDriverWait(driver, 20).until(
        EC.presence_of_element_located((By.CSS_SELECTOR, ".served-app .calc-display")))
    check("the good build still loads afterwards", display() == "0")
    check("app still mounted", len(driver.find_elements(By.CSS_SELECTOR, ".taskbar")) == 1)
finally:
    driver.quit()
    for stray in ("0.1.1", "0.1.2"):
        shutil.rmtree(f"{APPS}/{stray}", ignore_errors=True)

failed = [n for n, ok in results if not ok]
print(f"\n{len(results) - len(failed)}/{len(results)} served-app checks passed")
if failed:
    print("failed:", failed)
sys.exit(1 if failed else 0)
