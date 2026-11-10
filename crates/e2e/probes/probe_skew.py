import os, sys, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.common.keys import Keys
from selenium.webdriver.firefox.options import Options
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait

URL = os.environ.get("PROBE_URL", "http://127.0.0.1:8095/")
SHOTS = os.path.join(os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e", "shots"); os.makedirs(SHOTS, exist_ok=True)

opts = Options(); opts.add_argument("--headless"); opts.add_argument("--window-size=1400,950")
driver = webdriver.Firefox(options=opts); driver.set_window_size(1400, 950)
results = []
def check(n, c, x=""):
    results.append((n, bool(c)))
    print(("PASS  " if c else "FAIL  ") + n + (f"   [{x}]" if x else ""), flush=True)


def open_menu():
    driver.find_element(By.CSS_SELECTOR, ".start-btn").click()
    WebDriverWait(driver, 10).until(EC.visibility_of_element_located((By.CSS_SELECTOR, ".start-menu")))
    time.sleep(0.4)


def menu_items():
    return driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item")


def menu_item(name):
    for it in menu_items():
        if name in it.text:
            return it
    return None


try:
    driver.get(URL)
    WebDriverWait(driver, 30).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(2.5)  # let the catalog + health round trips settle

    # ---------- the catalog decoded despite the unknown app ----------
    dot = driver.find_element(By.CSS_SELECTOR, ".tray-dot").get_attribute("class")
    status = driver.find_element(By.CSS_SELECTOR, ".tray-dot").get_attribute("title")
    check("catalog with an unknown app still decodes (backend seen as online)",
          "online" in dot, f"{dot} / {status}")
    check("status line is not a catalog error", "catalog" not in (status or ""), status)

    # ---------- the unknown app is offered, using the server's metadata ----------
    open_menu()
    items = menu_items()
    names = [i.text for i in items]
    check("menu lists all five advertised apps", len(items) == 5, str(names))
    music = menu_item("Music")
    check("the unknown app is listed with the server's name", music is not None, str(names))
    check("the unknown app is not greyed out - it opens and explains",
          music is not None and "disabled" not in (music.get_attribute("class") or ""))

    # ---------- opening it explains the mismatch instead of failing ----------
    music.click()
    time.sleep(1.2)
    panes = driver.find_elements(By.CSS_SELECTOR, ".window .unavailable")
    check("opening an uninstalled app shows the explanatory pane", len(panes) == 1)
    text = panes[0].text if panes else ""
    check("the pane names the slug and the mismatch",
          "music" in text and "out of step" in text, text[:90].replace("\n", " / "))
    check("the window is a normal managed window (titlebar + taskbar)",
          len(driver.find_elements(By.CSS_SELECTOR, ".window .titlebar .tb-title")) == 1
          and "Music" in driver.find_element(By.CSS_SELECTOR, ".task-btn").text,
          driver.find_element(By.CSS_SELECTOR, ".tb-title").text)

    # chrome comes from the registry, so the accent is the server's
    frame = driver.find_element(By.CSS_SELECTOR, ".window").get_attribute("style")
    check("window chrome uses the server's label, glyph and accent",
          "Music" in driver.find_element(By.CSS_SELECTOR, ".tb-title").text
          and "#f472b6" in frame, frame[:70])
    check("taskbar button carries the server's accent",
          "#f472b6" in (driver.find_element(By.CSS_SELECTOR, ".task-btn").get_attribute("style") or ""))
    driver.save_screenshot(f"{SHOTS}/40-skew-unknown-app.png")

    # ---------- built-ins still work in the same session ----------
    open_menu()
    term = menu_item("Terminal")
    check("a built-in is still offered alongside it", term is not None)
    term.click()
    WebDriverWait(driver, 10).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".window .terminal")))
    driver.execute_script("document.querySelector('.term-input').focus()")
    inp = driver.find_elements(By.CSS_SELECTOR, ".term-input")[0]
    inp.send_keys("uname")
    inp.send_keys(Keys.ENTER)
    time.sleep(1.0)
    check("a built-in app runs normally under a skewed catalog",
          "wasm32-unknown-unknown" in driver.find_element(By.CSS_SELECTOR, ".term-scroll").text)
    check("two windows are open", len(driver.find_elements(By.CSS_SELECTOR, ".window")) == 2)
    check("app still mounted", len(driver.find_elements(By.CSS_SELECTOR, ".taskbar")) == 1)
finally:
    driver.quit()

failed = [n for n, ok in results if not ok]
print(f"\n{len(results) - len(failed)}/{len(results)} skew checks passed")
if failed:
    print("failed:", failed)
sys.exit(1 if failed else 0)
