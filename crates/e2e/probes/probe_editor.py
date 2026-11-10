import os, subprocess, sys, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.common.keys import Keys
from selenium.webdriver.firefox.options import Options
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait

BASE = os.environ.get("PROBE_URL", "http://127.0.0.1:8080/")
ROOT = os.environ.get("PROBE_ROOT", os.environ.get("PROBE_ROOT") or "/tmp/web-desktop-e2e/root")
SHOTS = os.path.join(os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e", "shots")
os.makedirs(SHOTS, exist_ok=True)

# fixtures the editor works on
os.makedirs(f"{ROOT}/editor-test", exist_ok=True)
open(f"{ROOT}/editor-test/alpha.txt", "w").write("alpha line 1\nline 2\n")
open(f"{ROOT}/editor-test/beta.txt", "w").write("beta\n")
open(f"{ROOT}/editor-test/binary.bin", "wb").write(b"\x00\x01\x02binary\x00")
open(f"{ROOT}/editor-test/image.png", "wb").write(b"\x89PNG\r\n\x1a\n\x00\x00")

opts = Options(); opts.add_argument("--headless"); optsz = opts.add_argument("--window-size=1500,1000")
driver = webdriver.Firefox(options=opts); driver.set_window_size(1500, 1000)
results = []
def check(n, c, x=""):
    results.append((n, bool(c)))
    print(("PASS  " if c else "FAIL  ") + n + (f"   [{x}]" if x else ""), flush=True)

def load_page():
    driver.get(BASE)
    WebDriverWait(driver, 30).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(2.5)

def open_editor():
    driver.find_element(By.CSS_SELECTOR, ".start-btn").click()
    WebDriverWait(driver, 10).until(EC.visibility_of_element_located((By.CSS_SELECTOR, ".start-menu")))
    time.sleep(0.4)
    for it in driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item"):
        if "Text Editor" in it.text:
            it.click()
            break
    WebDriverWait(driver, 20).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".editor")))
    time.sleep(0.8)

def nodes():
    return {n.find_element(By.CSS_SELECTOR, ".editor-node-name").text: n
            for n in driver.find_elements(By.CSS_SELECTOR, ".editor-node")}

def tabs():
    return [t.find_element(By.CSS_SELECTOR, ".tab-name").text for t in driver.find_elements(By.CSS_SELECTOR, ".editor-tab")]

def tab(name):
    for t in driver.find_elements(By.CSS_SELECTOR, ".editor-tab"):
        if t.find_element(By.CSS_SELECTOR, ".tab-name").text == name:
            return t
    return None

def box():
    return driver.find_element(By.CSS_SELECTOR, ".editor-textarea")

def status():
    return driver.find_element(By.CSS_SELECTOR, ".editor-status").text

def open_file(name):
    nodes()[name].click()
    time.sleep(1.0)

def save_all():
    for b in driver.find_elements(By.CSS_SELECTOR, ".editor-bar .ghost-btn"):
        if b.text == "Save all":
            b.click()
            time.sleep(1.5)
            return
    raise AssertionError("no Save all button")

try:
    load_page()
    open_editor()
    check("the editor opens from the start menu", len(driver.find_elements(By.CSS_SELECTOR, ".editor-sheet, .editor-body")) >= 1)
    check("it starts with no tabs", tabs() == [], str(tabs()))

    # browse into the fixture folder
    nodes()["editor-test"].click()
    time.sleep(1.2)
    check("the sidebar lists the folder", set(nodes()) >= {"alpha.txt", "beta.txt", "binary.bin", "image.png"}, str(sorted(nodes())))

    # ---------------- open a file, edit it, save with Ctrl+S ----------------
    open_file("alpha.txt")
    check("a file opens in a tab", tabs() == ["alpha.txt"], str(tabs()))
    check("its contents are in the box", box().get_attribute("value") == "alpha line 1\nline 2\n",
          repr(box().get_attribute("value")))
    check("the gutter numbers the lines", driver.find_element(By.CSS_SELECTOR, ".editor-gutter").text.split("\n") == ["1", "2", "3"],
          repr(driver.find_element(By.CSS_SELECTOR, ".editor-gutter").text))
    check("the status bar names the file", "/editor-test/alpha.txt" in status(), status().replace("\n", " / "))

    box().click()
    box().send_keys(Keys.END)
    box().send_keys("line 3\n")
    time.sleep(0.4)
    check("editing marks the tab dirty", "●" in tab("alpha.txt").text or "dirty" in (tab("alpha.txt").get_attribute("class") or ""),
          tab("alpha.txt").get_attribute("class"))
    check("the status bar says unsaved", "unsaved" in status(), status().replace("\n", " / "))
    check("the gutter grew with the text",
          driver.find_element(By.CSS_SELECTOR, ".editor-gutter").text.split("\n") == ["1", "2", "3", "4"],
          repr(driver.find_element(By.CSS_SELECTOR, ".editor-gutter").text))
    check("the caret readout moves", "Ln 4" in status() or "Ln 3" in status(), status().replace("\n", " / "))

    box().send_keys(Keys.CONTROL, "s")
    time.sleep(1.2)
    check("Ctrl+S writes the file",
          open(f"{ROOT}/editor-test/alpha.txt").read() == "alpha line 1\nline 2\nline 3\n",
          repr(open(f"{ROOT}/editor-test/alpha.txt").read()))
    check("the tab is clean again", "dirty" not in (tab("alpha.txt").get_attribute("class") or ""))
    check("the save is reported", "Saved" in driver.find_element(By.CSS_SELECTOR, ".editor-bar").text,
          driver.find_element(By.CSS_SELECTOR, ".editor-bar").text)

    # ---------------- several buffers at once ----------------
    open_file("beta.txt")
    check("a second file opens a second tab", tabs() == ["alpha.txt", "beta.txt"], str(tabs()))
    check("the box now shows the second file", box().get_attribute("value") == "beta\n",
          repr(box().get_attribute("value")))
    box().click()
    box().send_keys(Keys.END)
    box().send_keys("more beta\n")
    time.sleep(0.4)

    tab("alpha.txt").find_element(By.CSS_SELECTOR, ".tab-name").click()
    time.sleep(0.6)
    check("switching back shows the first buffer", box().get_attribute("value") == "alpha line 1\nline 2\nline 3\n",
          repr(box().get_attribute("value")))
    check("the first tab is marked active", "active" in (tab("alpha.txt").get_attribute("class") or ""))
    check("the edit in the other tab was kept", tab("beta.txt") is not None and "dirty" in (tab("beta.txt").get_attribute("class") or ""))
    check("the status counts the open buffers", "2 open" in status() and "1 unsaved" in status(),
          status().replace("\n", " / "))
    driver.save_screenshot(f"{SHOTS}/70-editor.png")

    save_all()
    check("Save all writes every changed tab", open(f"{ROOT}/editor-test/beta.txt").read() == "beta\nmore beta\n",
          repr(open(f"{ROOT}/editor-test/beta.txt").read()))
    check("nothing is left unsaved", "unsaved" not in status(), status().replace("\n", " / "))

    # ---------------- unsupported files still open, without an editor ----------------
    open_file("binary.bin")
    check("a binary file opens a tab that explains itself",
          "binary" in driver.find_element(By.CSS_SELECTOR, ".editor-body").text,
          driver.find_element(By.CSS_SELECTOR, ".editor-body").text[:60].replace("\n", " / "))
    check("with no textarea", not driver.find_elements(By.CSS_SELECTOR, ".editor-textarea"))

    # ---------------- new file ----------------
    for b in driver.find_elements(By.CSS_SELECTOR, ".editor-bar .ghost-btn"):
        if "New" in b.text:
            b.click()
            break
    WebDriverWait(driver, 5).until(EC.visibility_of_element_located((By.CSS_SELECTOR, ".files-prompt")))
    input_box = driver.find_element(By.CSS_SELECTOR, ".prompt-input")
    input_box.click()
    input_box.send_keys("fresh.txt")
    driver.find_element(By.CSS_SELECTOR, ".files-prompt .ghost-btn.primary").click()
    time.sleep(1.5)
    check("the new file is created on disk", os.path.exists(f"{ROOT}/editor-test/fresh.txt"))
    check("and opened in a tab", "fresh.txt" in tabs(), str(tabs()))
    box().click()
    box().send_keys("brand new\n")
    time.sleep(0.3)
    box().send_keys(Keys.CONTROL, "s")
    time.sleep(1.2)
    check("saving the new file works", open(f"{ROOT}/editor-test/fresh.txt").read() == "brand new\n",
          repr(open(f"{ROOT}/editor-test/fresh.txt").read()))

    # ---------------- closing guards unsaved changes ----------------
    box().click()
    box().send_keys(Keys.END)
    box().send_keys("unsaved tail\n")
    time.sleep(0.4)
    closer = tab("fresh.txt").find_element(By.CSS_SELECTOR, ".tab-close")
    closer.click()
    time.sleep(0.4)
    check("closing a dirty tab asks first", "fresh.txt" in tabs() and "discard" in closer.text, closer.text)
    check("and says what will happen",
          "unsaved changes" in driver.find_element(By.CSS_SELECTOR, ".editor-bar").text,
          driver.find_element(By.CSS_SELECTOR, ".editor-bar").text)
    closer.click()
    time.sleep(0.5)
    check("the second click closes it", "fresh.txt" not in tabs(), str(tabs()))
    check("the file on disk is untouched by the discard",
          open(f"{ROOT}/editor-test/fresh.txt").read() == "brand new\n",
          repr(open(f"{ROOT}/editor-test/fresh.txt").read()))

    check("app still mounted", len(driver.find_elements(By.CSS_SELECTOR, ".taskbar")) == 1)
finally:
    driver.quit()
    subprocess.run(["rm", "-rf", f"{ROOT}/editor-test"], capture_output=True)

failed = [n for n, ok in results if not ok]
print(f"\n{len(results) - len(failed)}/{len(results)} editor checks passed")
if failed:
    print("failed:", failed)
sys.exit(1 if failed else 0)
