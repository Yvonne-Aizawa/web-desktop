import os
WORK = os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e"
import json
import sys
import time

from selenium import webdriver
from selenium.webdriver.common.action_chains import ActionChains
from selenium.webdriver.common.by import By
from selenium.webdriver.common.keys import Keys
from selenium.webdriver.firefox.options import Options
from selenium.webdriver.firefox.service import Service
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait

URL = "http://127.0.0.1:8080/"
SHOTS = os.path.join(os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e", "shots")

os.makedirs(SHOTS, exist_ok=True)

opts = Options()
opts.add_argument("--headless")
opts.add_argument("--window-size=1280,800")
opts.set_preference("dom.disable_beforeunload", True)

driver = webdriver.Firefox(options=opts)
driver.set_window_size(1280, 800)
results = []


def check(name, ok, extra=""):
    results.append((name, bool(ok), extra))
    print(("PASS  " if ok else "FAIL  ") + name + (f"  [{extra}]" if extra else ""), flush=True)


def shot(name):
    driver.save_screenshot(f"{SHOTS}/{name}.png")


def open_menu():
    """The start button toggles, so only click it when the menu is closed."""
    if not driver.find_elements(By.CSS_SELECTOR, ".start-menu"):
        driver.find_element(By.CSS_SELECTOR, ".start-btn").click()
        WebDriverWait(driver, 5).until(EC.visibility_of_element_located((By.CSS_SELECTOR, ".start-menu")))


def launch(app):
    """Open the menu and start the app whose name contains `app`."""
    open_menu()
    for item in driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item"):
        if app in item.text:
            item.click()
            WebDriverWait(driver, 10).until(
                lambda d: not d.find_elements(By.CSS_SELECTOR, ".start-menu"))
            return True
    return False

try:
    driver.get(URL)
    wait = WebDriverWait(driver, 30)

    # 1. did the wasm app mount at all?
    wait.until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    check("app mounted (taskbar present)", True)

    # 2. backend catalog came back -> tray dot online, status line populated
    wait.until(lambda d: "online" in (d.find_element(By.CSS_SELECTOR, ".tray-dot").get_attribute("class") or ""))
    check("catalog fetched from axum (tray dot online)", True)

    # 3. empty-desktop hint
    check("empty hint visible", len(driver.find_elements(By.CSS_SELECTOR, ".empty-hint")) == 1)
    shot("01-desktop")

    # 4. start menu opens
    open_menu()
    items = driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item")
    names = [i.text.split("\n")[1] for i in items if "\n" in i.text]
    check("start menu lists the catalog (built-ins, plus any served app)",
          {"Terminal", "Files", "Settings", "About"} <= set(names), str(names))
    shot("02-start-menu")

    # 5. search filters
    search = driver.find_element(By.CSS_SELECTOR, ".menu-search")
    search.send_keys("term")
    time.sleep(0.4)
    filtered = driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item")
    check("search filters the catalog", len(filtered) == 1, f"items={len(filtered)}")
    search.send_keys(Keys.CONTROL, "a")
    search.send_keys(Keys.BACKSPACE)
    time.sleep(0.3)

    # 6. launch the terminal from the start menu
    term_btn = None
    for it in driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item"):
        if "Terminal" in it.text:
            term_btn = it
            break
    check("terminal entry found in start menu", term_btn is not None)
    term_btn.click()
    wait.until(EC.presence_of_element_located((By.CSS_SELECTOR, ".window .terminal")))
    check("terminal window opened", True)
    check("start menu closed after launch", not driver.find_elements(By.CSS_SELECTOR, ".start-menu"))
    check("empty hint hidden", not driver.find_elements(By.CSS_SELECTOR, ".empty-hint"))

    # 7. drag the window by its title bar
    win = driver.find_element(By.CSS_SELECTOR, ".window")
    before = win.get_attribute("style")
    box = driver.find_element(By.CSS_SELECTOR, ".titlebar").rect
    ac = ActionChains(driver)
    ac.move_to_element(driver.find_element(By.CSS_SELECTOR, ".titlebar"))
    ac.click_and_hold()
    ac.move_by_offset(140, 90)
    ac.release()
    ac.perform()
    time.sleep(0.5)
    after = win.get_attribute("style")

    def left_of(style):
        return float(style.split("left:")[1].split("px")[0])

    def top_of(style):
        return float(style.split("top:")[1].split("px")[0])

    check(
        "drag moves the window",
        abs(left_of(after) - left_of(before) - 140) < 6 and abs(top_of(after) - top_of(before) - 90) < 6,
        f"{left_of(before)},{top_of(before)} -> {left_of(after)},{top_of(after)}",
    )
    shot("03-dragged")

    # 8. resize from the south-east handle
    w0 = float(after.split("width:")[1].split("px")[0])
    h0 = float(after.split("height:")[1].split("px")[0])
    ac = ActionChains(driver)
    ac.move_to_element(driver.find_element(By.CSS_SELECTOR, ".rz-se"))
    ac.click_and_hold()
    ac.move_by_offset(120, 60)
    ac.release()
    ac.perform()
    time.sleep(0.5)
    style = win.get_attribute("style")
    w1 = float(style.split("width:")[1].split("px")[0])
    h1 = float(style.split("height:")[1].split("px")[0])
    check("resize grows the window", w1 - w0 > 100 and h1 - h0 > 45, f"{w0}x{h0} -> {w1}x{h1}")
    shot("04-resized")

    # 9. resize from the north-west handle moves the origin too
    l0, t0 = left_of(style), top_of(style)
    ac = ActionChains(driver)
    ac.move_to_element(driver.find_element(By.CSS_SELECTOR, ".rz-nw"))
    ac.click_and_hold()
    ac.move_by_offset(-60, -30)
    ac.release()
    ac.perform()
    time.sleep(0.5)
    style = win.get_attribute("style")
    check(
        "nw resize moves origin and grows size",
        left_of(style) < l0 - 40 and top_of(style) < t0 - 15 and float(style.split("width:")[1].split("px")[0]) > w1,
        f"{l0},{t0} -> {left_of(style)},{top_of(style)}",
    )

    # 10. terminal commands
    inp = driver.find_element(By.CSS_SELECTOR, ".term-input")
    inp.click()
    time.sleep(0.2)
    inp.send_keys("uname")
    inp.send_keys(Keys.ENTER)
    time.sleep(0.4)
    print("TERM TEXT AFTER uname:", repr(driver.find_element(By.CSS_SELECTOR, ".terminal .term-scroll").text[-160:]))
    inp.click()
    inp.send_keys("open files")
    inp.send_keys(Keys.ENTER)
    time.sleep(0.6)
    wait.until(lambda d: len(d.find_elements(By.CSS_SELECTOR, ".window")) == 2)
    check("terminal `open files` launched a second window", True)
    out = driver.find_element(By.CSS_SELECTOR, ".terminal .term-scroll").text
    check("terminal echoed uname output", "wasm32-unknown-unknown" in out)
    shot("05-two-windows")

    # 11. maximizing via the title bar button. The browser re-serialises the style
    # attribute (`width:100%` comes back as `width: 100%`), so assert on geometry:
    # a maximized window is exactly the window layer.
    layer = driver.find_element(By.CSS_SELECTOR, ".window-layer").rect
    maxbtn = driver.find_elements(By.CSS_SELECTOR, ".window")[1].find_element(By.CSS_SELECTOR, ".tb-max")
    maxbtn.click()
    time.sleep(0.4)
    maxed = driver.find_elements(By.CSS_SELECTOR, ".window")[1].rect
    styles = [w.get_attribute("style") for w in driver.find_elements(By.CSS_SELECTOR, ".window")]
    check("maximize fills the window layer",
          maxed["width"] == layer["width"] and maxed["height"] == layer["height"]
          and any("100%" in s for s in styles),
          f"{maxed['width']}x{maxed['height']} vs layer {layer['width']}x{layer['height']}")
    shot("06-maximized")
    maxbtn.click()
    time.sleep(0.4)

    # 12. taskbar buttons: minimize the focused window then restore it
    btns = driver.find_elements(By.CSS_SELECTOR, ".task-btn")
    check("taskbar shows one button per window", len(btns) == 2, f"btns={len(btns)}")
    btns[1].click()
    time.sleep(0.4)
    mins = [w.get_attribute("class") for w in driver.find_elements(By.CSS_SELECTOR, ".window")]
    check("taskbar click minimizes", any("minimized" in c for c in mins), str(mins))
    btns[1].click()
    time.sleep(0.4)
    mins = [w.get_attribute("class") for w in driver.find_elements(By.CSS_SELECTOR, ".window")]
    check("second click restores", not any("minimized" in c for c in mins), str(mins))
    shot("07-taskbar")

    # 13. focus/z-order: clicking a window raises it. The Files window sits on top
    # of the terminal's titlebar after the drag and resize tests, and a covered
    # titlebar cannot be clicked — the browser's rule, not the app's — so move
    # Files clear of it first (which also raises Files, so the comparison means
    # something).
    files_win = driver.find_elements(By.CSS_SELECTOR, ".window")[1]
    ac = ActionChains(driver)
    ac.move_to_element(files_win.find_element(By.CSS_SELECTOR, ".titlebar"))
    ac.click_and_hold()
    ac.move_by_offset(420, 320)
    ac.release()
    ac.perform()
    time.sleep(0.5)
    wins = driver.find_elements(By.CSS_SELECTOR, ".window")
    wins[0].find_element(By.CSS_SELECTOR, ".titlebar").click()
    time.sleep(0.3)
    zs = [int(w.get_attribute("style").split("z-index:")[1].split(";")[0]) for w in driver.find_elements(By.CSS_SELECTOR, ".window")]
    check("clicking a window raises it", zs[0] > zs[1], f"z={zs}")
    actives = [b.get_attribute("class") for b in driver.find_elements(By.CSS_SELECTOR, ".task-btn")]
    check("taskbar marks the focused window", "active" in actives[0], str(actives))
    shot("08-zorder")

    # 14. files app navigation (raise Files first: it is behind the terminal)
    driver.find_elements(By.CSS_SELECTOR, ".task-btn")[1].click()
    time.sleep(0.5)
    files_body = driver.find_elements(By.CSS_SELECTOR, ".window")[1].find_element(By.CSS_SELECTOR, ".files")
    root_crumbs = [c.text for c in driver.find_elements(By.CSS_SELECTOR, ".crumb-seg")]
    folders = files_body.find_elements(By.CSS_SELECTOR, ".files-list .row")
    folder = folders[0].find_element(By.CSS_SELECTOR, ".row-name").text
    folders[0].click()
    time.sleep(0.4)
    inside = [c.text for c in driver.find_elements(By.CSS_SELECTOR, ".crumb-seg")]
    check("files app navigates into a folder",
          len(inside) == len(root_crumbs) + 1 and inside[-1] == folder,
          f"{root_crumbs} -> {inside}")
    for item in files_body.find_elements(By.CSS_SELECTOR, ".side-item"):
        if "notes" in item.text:
            item.click()
            break
    time.sleep(0.4)
    rows = files_body.find_elements(By.CSS_SELECTOR, ".files-list .row")
    check("files app lists the files in a folder", len(rows) > 0, str([r.text for r in rows][:4]))
    target = next((r for r in rows if "todo" in r.text), rows[0])
    target.click()
    # the preview arrives asynchronously, so wait for it; read textContent, which
    # does not depend on the pane happening to be inside the viewport
    read = "return document.querySelector('.files-preview').textContent"
    WebDriverWait(driver, 10).until(lambda d: (d.execute_script(read) or "").strip())
    preview = driver.execute_script(read)
    check("files app previews the selected file", "todo" in preview, repr(preview[:60]))
    shot("09-files")

    # 15. settings: switch wallpaper, then reconnect
    check("start menu launches Settings", launch("Settings"), "no Settings item")
    wait.until(EC.presence_of_element_located((By.CSS_SELECTOR, ".window .settings")))
    swatches = driver.find_elements(By.CSS_SELECTOR, ".swatch")
    check("settings lists 6 wallpapers", len(swatches) == 6, f"n={len(swatches)}")
    bg0 = driver.find_element(By.CSS_SELECTOR, ".desktop").get_attribute("style")
    swatches[3].click()
    time.sleep(0.4)
    bg1 = driver.find_element(By.CSS_SELECTOR, ".desktop").get_attribute("style")
    check("wallpaper changes on click", bg0 != bg1, f"{bg0[:48]} -> {bg1[:48]}")
    driver.find_elements(By.CSS_SELECTOR, ".settings .ghost-btn")[0].click()
    time.sleep(1.0)
    shot("10-settings")
    check("reconnect keeps backend online", "online" in (driver.find_element(By.CSS_SELECTOR, ".tray-dot").get_attribute("class") or ""))

    # 16. close every window via Settings, then confirm the hint returns
    driver.find_elements(By.CSS_SELECTOR, ".settings .ghost-btn.danger")[0].click()
    time.sleep(0.5)
    check("close all windows", len(driver.find_elements(By.CSS_SELECTOR, ".window")) == 0)
    check("empty hint returns", len(driver.find_elements(By.CSS_SELECTOR, ".empty-hint")) == 1)
    shot("11-closed-all")

    # 17. still alive (no wasm panic blanked the tree)
    check("app still mounted at the end", len(driver.find_elements(By.CSS_SELECTOR, ".taskbar")) == 1)
finally:
    with open(os.path.join(WORK, "shots", "result.json"), "w") as fh:
        json.dump(results, fh, indent=1)
    driver.quit()

failed = [r for r in results if not r[1]]
print(f"\n{len(results) - len(failed)}/{len(results)} checks passed")
sys.exit(1 if failed else 0)
