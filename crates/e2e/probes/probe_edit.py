import sys
import time
import os
import subprocess
WORK = os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e"
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

opts = Options(); opts.add_argument("--headless"); opts.add_argument("--window-size=1500,1000")
driver = webdriver.Firefox(options=opts); driver.set_window_size(1500, 1000)
results = []
def check(n, c, x=""):
    results.append((n, bool(c)))
    print(("PASS  " if c else "FAIL  ") + n + (f"   [{x}]" if x else ""), flush=True)

def load_page():
    driver.get(BASE)
    WebDriverWait(driver, 30).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(2)

def open_files():
    driver.find_element(By.CSS_SELECTOR, ".start-btn").click()
    WebDriverWait(driver, 10).until(EC.visibility_of_element_located((By.CSS_SELECTOR, ".start-menu")))
    time.sleep(0.4)
    for it in driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item"):
        if "Files" in it.text:
            it.click()
            break
    WebDriverWait(driver, 20).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".files-list .row")))
    time.sleep(0.6)

def row(name):
    for r in driver.find_elements(By.CSS_SELECTOR, ".files-list .row"):
        if r.find_element(By.CSS_SELECTOR, ".row-name").text == name:
            return r
    return None

def rows():
    return [r.find_element(By.CSS_SELECTOR, ".row-name").text
            for r in driver.find_elements(By.CSS_SELECTOR, ".files-list .row")]

def btn(label, scope=".files-main"):
    for b in driver.find_elements(By.CSS_SELECTOR, f"{scope} .ghost-btn"):
        if label in b.text:
            return b
    return None

def notice():
    els = driver.find_elements(By.CSS_SELECTOR, ".files-notice")
    return els[0].text if els else ""

def editor():
    return driver.find_element(By.CSS_SELECTOR, ".editor-box")

def type_name(text):
    box = driver.find_element(By.CSS_SELECTOR, ".prompt-input")
    box.click()
    box.send_keys(Keys.CONTROL, "a")
    box.send_keys(text)
    driver.find_element(By.CSS_SELECTOR, ".files-prompt .ghost-btn.primary").click()
    time.sleep(1.2)

try:
    load_page()
    open_files()
    driver.save_screenshot(f"{SHOTS}/60-editing.png")

    check("a writable root offers the write actions",
          all(btn(l) is not None for l in ("Folder", "File", "Upload")),
          str([b.text for b in driver.find_elements(By.CSS_SELECTOR, ".files-bar .ghost-btn")]))

    # ---------------- new folder ----------------
    btn("Folder").click()
    WebDriverWait(driver, 5).until(EC.visibility_of_element_located((By.CSS_SELECTOR, ".files-prompt")))
    type_name("edited")
    check("new folder appears", "edited" in rows(), str(rows()))
    check("the notice says so", "Created" in notice(), notice())
    check("it exists on disk", os.path.isdir(f"{ROOT}/edited"))

    # ---------------- new file, then edit and save it ----------------
    row("edited").click()
    time.sleep(1.0)
    btn("File").click()
    WebDriverWait(driver, 5).until(EC.visibility_of_element_located((By.CSS_SELECTOR, ".files-prompt")))
    type_name("draft.txt")
    check("new file appears in the folder", "draft.txt" in rows(), str(rows()))

    row("draft.txt").click()
    time.sleep(1.0)
    check("an empty text file opens an editor", len(driver.find_elements(By.CSS_SELECTOR, ".editor-box")) == 1)
    box = editor()
    box.click()
    box.send_keys("hello from the editor\nsecond line\n")
    time.sleep(0.4)
    check("typing marks the buffer unsaved",
          "unsaved" in driver.find_element(By.CSS_SELECTOR, ".editor-bar").text,
          driver.find_element(By.CSS_SELECTOR, ".editor-bar").text.replace("\n", " / "))

    # Save with Ctrl+S, the way the toolbar advertises it
    box.send_keys(Keys.CONTROL, "s")
    time.sleep(1.2)
    check("Ctrl+S saves", open(f"{ROOT}/edited/draft.txt").read() == "hello from the editor\nsecond line\n",
          repr(open(f"{ROOT}/edited/draft.txt").read()[:40]))
    check("the unsaved marker is gone",
          "unsaved" not in driver.find_element(By.CSS_SELECTOR, ".editor-bar").text)
    check("the save is reported", "Saved" in notice(), notice())

    # edit again and save with the button
    box.click()
    box.send_keys(Keys.END)
    box.send_keys("third\n")
    time.sleep(0.3)
    btn("Save", ".editor-bar").click()
    time.sleep(1.2)
    check("the Save button works too",
          open(f"{ROOT}/edited/draft.txt").read().endswith("third\n"),
          repr(open(f"{ROOT}/edited/draft.txt").read()[-20:]))

    # revert
    box.click()
    box.send_keys(Keys.END)
    box.send_keys("scratch")
    time.sleep(0.3)
    btn("Revert", ".editor-bar").click()
    time.sleep(0.3)
    check("revert restores what is on disk", "scratch" not in editor().get_attribute("value"),
          repr(editor().get_attribute("value")[-20:]))
    check("and it is no longer dirty",
          "unsaved" not in driver.find_element(By.CSS_SELECTOR, ".editor-bar").text)
    driver.save_screenshot(f"{SHOTS}/61-editor.png")

    # ---------------- rename ----------------
    check("a selected file offers rename", btn("Rename") is not None or btn("✎") is not None,
          str([b.text for b in driver.find_elements(By.CSS_SELECTOR, ".preview-actions .ghost-btn")]))
    rename = btn("Rename") or btn("✎")
    rename.click()
    WebDriverWait(driver, 5).until(EC.visibility_of_element_located((By.CSS_SELECTOR, ".files-prompt")))
    type_name("renamed.txt")
    check("the file is renamed", "renamed.txt" in rows() and "draft.txt" not in rows(), str(rows()))
    check("the content survived the rename", "third" in open(f"{ROOT}/edited/renamed.txt").read())

    # ---------------- upload (binary safe) ----------------
    payload = bytes(range(256)) * 8
    local = os.path.join(WORK, "upload-me.bin")
    open(local, "wb").write(payload)
    driver.find_element(By.CSS_SELECTOR, ".files-bar input[type=file]").send_keys(local)
    time.sleep(2.0)
    check("an uploaded file is listed", "upload-me.bin" in rows(), str(rows()))
    check("the bytes arrive unchanged", open(f"{ROOT}/edited/upload-me.bin", "rb").read() == payload)
    check("the upload is reported", "Uploaded" in notice(), notice())

    # ---------------- delete, which now goes to the trash ----------------
    row("upload-me.bin").click()
    time.sleep(0.8)
    delete = btn("Delete")
    check("a selected file offers delete", delete is not None)
    delete.click()
    time.sleep(1.5)
    # One click, no dialog: the trash makes it reversible, and the undo bar says so.
    check("one click moves it to the trash",
          "upload-me.bin" not in rows() and not os.path.exists(f"{ROOT}/edited/upload-me.bin"))
    check("the deletion is reported as a move, not a destroy",
          "trash" in notice(), notice())
    check("and the undo bar offers it back",
          "Undo" in driver.find_element(By.CSS_SELECTOR, ".undo-bar").text,
          driver.find_element(By.CSS_SELECTOR, ".undo-bar").text.replace("\n", " "))
    check("the trash really holds it",
          os.path.isdir(f"{ROOT}/.trash") and len(os.listdir(f"{ROOT}/.trash")) == 1)

    # ---------------- read-only notice explains itself ----------------
    check("the sidebar says the root is read-write",
          "Read-write" in driver.find_element(By.CSS_SELECTOR, ".files-side").text,
          driver.find_element(By.CSS_SELECTOR, ".side-fine").text[:60])

    # ---------------- hand a file to the Text Editor ----------------
    subprocess.run(["mkdir", "-p", f"{ROOT}/edited"], capture_output=True)
    open(f"{ROOT}/edited/handoff.txt", "w").write("handed over\n")
    for _ in range(4):
        btn("⟳").click()
        time.sleep(0.8)
    driver.find_element(By.CSS_SELECTOR, ".files-bar .ghost-btn[title='Reload this folder']").click()
    time.sleep(1.2)
    row("handoff.txt").click()
    time.sleep(1.0)
    edit = None
    for b in driver.find_elements(By.CSS_SELECTOR, ".preview-head .ghost-btn"):
        if b.text == "✎ Edit":
            edit = b
    check("Files offers to open a file in the Text Editor", edit is not None and edit.is_displayed(),
          str([b.text for b in driver.find_elements(By.CSS_SELECTOR, ".preview-head .ghost-btn")]))
    edit.click()
    time.sleep(2.0)
    check("the editor opens with that file in a tab",
          "handoff.txt" in [t.find_element(By.CSS_SELECTOR, ".tab-name").text
                            for t in driver.find_elements(By.CSS_SELECTOR, ".editor-tab")],
          str([t.text for t in driver.find_elements(By.CSS_SELECTOR, ".editor-tab")]))
    check("its contents came with it",
          driver.find_element(By.CSS_SELECTOR, ".editor-textarea").get_attribute("value") == "handed over\n",
          repr(driver.find_element(By.CSS_SELECTOR, ".editor-textarea").get_attribute("value")))
    # the editor is already open now: a second handoff must still land
    driver.find_elements(By.CSS_SELECTOR, ".task-btn")[1].click()   # raise Files
    time.sleep(0.8)
    for b in driver.find_elements(By.CSS_SELECTOR, ".preview-head .ghost-btn"):
        if b.text == "✎ Edit":
            b.click()
    time.sleep(1.5)
    check("a second handoff raises the same window",
          len(driver.find_elements(By.CSS_SELECTOR, ".window .editor")) == 1,
          str(len(driver.find_elements(By.CSS_SELECTOR, ".window .editor"))))

    # ---------------- hand a file to the Text Editor ----------------
    subprocess.run(["mkdir", "-p", f"{ROOT}/edited"], capture_output=True)
    open(f"{ROOT}/edited/handoff.txt", "w").write("handed over\n")
    for _ in range(4):
        btn("⟳").click()
        time.sleep(0.8)
    driver.find_element(By.CSS_SELECTOR, ".files-bar .ghost-btn[title='Reload this folder']").click()
    time.sleep(1.2)
    row("handoff.txt").click()
    time.sleep(1.0)
    edit = None
    for b in driver.find_elements(By.CSS_SELECTOR, ".preview-head .ghost-btn"):
        if b.text == "✎ Edit":
            edit = b
    check("Files offers to open a file in the Text Editor", edit is not None and edit.is_displayed(),
          str([b.text for b in driver.find_elements(By.CSS_SELECTOR, ".preview-head .ghost-btn")]))
    edit.click()
    time.sleep(2.0)
    check("the editor opens with that file in a tab",
          "handoff.txt" in [t.find_element(By.CSS_SELECTOR, ".tab-name").text
                            for t in driver.find_elements(By.CSS_SELECTOR, ".editor-tab")],
          str([t.text for t in driver.find_elements(By.CSS_SELECTOR, ".editor-tab")]))
    check("its contents came with it",
          driver.find_element(By.CSS_SELECTOR, ".editor-textarea").get_attribute("value") == "handed over\n",
          repr(driver.find_element(By.CSS_SELECTOR, ".editor-textarea").get_attribute("value")))
    # the editor is already open now: a second handoff must still land
    driver.find_elements(By.CSS_SELECTOR, ".task-btn")[1].click()   # raise Files
    time.sleep(0.8)
    for b in driver.find_elements(By.CSS_SELECTOR, ".preview-head .ghost-btn"):
        if b.text == "✎ Edit":
            b.click()
    time.sleep(1.5)
    check("a second handoff raises the same window",
          len(driver.find_elements(By.CSS_SELECTOR, ".window .editor")) == 1,
          str(len(driver.find_elements(By.CSS_SELECTOR, ".window .editor"))))

    # ---------------- hand a file to the Text Editor ----------------
    subprocess.run(["mkdir", "-p", f"{ROOT}/edited"], capture_output=True)
    open(f"{ROOT}/edited/handoff.txt", "w").write("handed over\n")
    for _ in range(4):
        btn("⟳").click()
        time.sleep(0.8)
    driver.find_element(By.CSS_SELECTOR, ".files-bar .ghost-btn[title='Reload this folder']").click()
    time.sleep(1.2)
    row("handoff.txt").click()
    time.sleep(1.0)
    edit = None
    for b in driver.find_elements(By.CSS_SELECTOR, ".preview-head .ghost-btn"):
        if b.text == "✎ Edit":
            edit = b
    check("Files offers to open a file in the Text Editor", edit is not None and edit.is_displayed(),
          str([b.text for b in driver.find_elements(By.CSS_SELECTOR, ".preview-head .ghost-btn")]))
    edit.click()
    time.sleep(2.0)
    check("the editor opens with that file in a tab",
          "handoff.txt" in [t.find_element(By.CSS_SELECTOR, ".tab-name").text
                            for t in driver.find_elements(By.CSS_SELECTOR, ".editor-tab")],
          str([t.text for t in driver.find_elements(By.CSS_SELECTOR, ".editor-tab")]))
    check("its contents came with it",
          driver.find_element(By.CSS_SELECTOR, ".editor-textarea").get_attribute("value") == "handed over\n",
          repr(driver.find_element(By.CSS_SELECTOR, ".editor-textarea").get_attribute("value")))
    # the editor is already open now: a second handoff must still land
    driver.find_elements(By.CSS_SELECTOR, ".task-btn")[1].click()   # raise Files
    time.sleep(0.8)
    for b in driver.find_elements(By.CSS_SELECTOR, ".preview-head .ghost-btn"):
        if b.text == "✎ Edit":
            b.click()
    time.sleep(1.5)
    check("a second handoff raises the same window",
          len(driver.find_elements(By.CSS_SELECTOR, ".window .editor")) == 1,
          str(len(driver.find_elements(By.CSS_SELECTOR, ".window .editor"))))

    check("app still mounted", len(driver.find_elements(By.CSS_SELECTOR, ".taskbar")) == 1)
finally:
    driver.quit()
    subprocess.run(["rm", "-rf", f"{ROOT}/edited"], capture_output=True)

failed = [n for n, ok in results if not ok]
print(f"\n{len(results) - len(failed)}/{len(results)} editing checks passed")
if failed:
    print("failed:", failed)
sys.exit(1 if failed else 0)
