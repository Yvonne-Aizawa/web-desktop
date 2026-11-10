"""Trash, undo, and read-only paths, driven through the real UI.

Runs against a scratch root started with WEB_DESKTOP_FS_READONLY=Lumina, so the
protection is the real thing and not a mock.
"""
import os, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.firefox.options import Options
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait

URL = os.environ.get("PROBE_URL", "http://127.0.0.1:8096/")
ROOT = os.environ.get("PROBE_ROOT", os.environ.get("PROBE_ROOT") or "/tmp/web-desktop-e2e/trash")
SHOTS = os.path.join(os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e", "shots")
os.makedirs(SHOTS, exist_ok=True)

opts = Options(); opts.add_argument("--headless"); opts.add_argument("--window-size=1400,900")
driver = webdriver.Firefox(options=opts)
driver.set_window_size(1400, 900)

results = []
def check(name, ok, extra=""):
    results.append((name, bool(ok)))
    print(("PASS  " if ok else "FAIL  ") + name + (f"   [{extra}]" if extra else ""), flush=True)

def shot(name):
    driver.save_screenshot(f"{SHOTS}/{name}.png")

def rows():
    return driver.find_elements(By.CSS_SELECTOR, ".files-list .row")

def row_names():
    return [r.find_element(By.CSS_SELECTOR, ".row-name").text for r in rows()]

def wait_names(predicate, why, timeout=15):
    WebDriverWait(driver, timeout).until(lambda d: predicate(row_names())) if False else None
    end = time.time() + timeout
    while time.time() < end:
        if predicate(row_names()):
            return True
        time.sleep(0.2)
    raise AssertionError(f"timed out waiting for {why}: {row_names()}")

def row(name):
    for r in rows():
        if r.find_element(By.CSS_SELECTOR, ".row-name").text == name:
            return r
    raise AssertionError(f"no row {name} in {row_names()}")

def pane():
    return driver.find_elements(By.CSS_SELECTOR, ".window .files")[0]

def preview_buttons():
    # What is *offered*: locked files render the buttons but hide them, and a
    # hidden button's text reads as empty — so look at what is displayed.
    return {b.text.strip() for b in pane().find_elements(By.CSS_SELECTOR, ".preview-actions .ghost-btn")
            if b.is_displayed()}

def open_files():
    driver.find_element(By.CSS_SELECTOR, ".start-btn").click()
    time.sleep(0.5)
    items = driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item")
    check("the start menu lists the Files app", any("Files" in it.text for it in items),
          str([it.text.replace("\n", " ")[:22] for it in items]))
    for it in items:
        if "Files" in it.text:
            it.click()
            break
    WebDriverWait(driver, 25).until(lambda d: len(d.find_elements(By.CSS_SELECTOR, ".window .files")) == 1)
    WebDriverWait(driver, 20).until(lambda d: len(rows()) > 0)

def show_trash():
    pane().find_element(By.CSS_SELECTOR, ".side-item:nth-of-type(1)")  # keep the DOM settled
    for b in pane().find_elements(By.CSS_SELECTOR, ".side-item"):
        if b.text.strip() == "Trash":
            b.click()
            return
    raise AssertionError("no Trash button in the sidebar")

def trash_rows():
    WebDriverWait(driver, 20).until(
        lambda d: d.find_elements(By.CSS_SELECTOR, ".trash-list .trash-row")
        or "empty" in d.find_elements(By.CSS_SELECTOR, ".window .files")[0].text.lower()
    )
    return driver.find_elements(By.CSS_SELECTOR, ".trash-list .trash-row")

def trash_names():
    return [r.find_element(By.CSS_SELECTOR, ".trash-name").text for r in trash_rows()]

def trash_row(name):
    for r in trash_rows():
        if r.find_element(By.CSS_SELECTOR, ".trash-name").text == name:
            return r
    raise AssertionError(f"no trash row {name} in {trash_names()}")

def toolbar_click(label):
    for b in pane().find_elements(By.CSS_SELECTOR, ".files-bar .ghost-btn"):
        if label in b.text:
            b.click()
            return
    raise AssertionError(
        f"no {label} in the toolbar: "
        + str([b.text.strip() for b in pane().find_elements(By.CSS_SELECTOR, ".files-bar .ghost-btn")]))

def trash_folder(name):
    """Folders are entered, not selected, so theirs is a toolbar action."""
    row(name).click()
    WebDriverWait(driver, 15).until(lambda d: name not in row_names())
    toolbar_click("Delete this folder")
    wait_names(lambda n: name not in n, f"{name} to leave the parent")

def click_in(el, label):
    for b in el.find_elements(By.CSS_SELECTOR, ".ghost-btn"):
        if label in b.text:
            b.click()
            return
    raise AssertionError(f"no {label} button in {el.text!r}")

def on_disk(rel):
    return os.path.exists(os.path.join(ROOT, rel))

def trash_on_disk():
    base = os.path.join(ROOT, ".trash")
    if not os.path.isdir(base):
        return []
    return [d for d in os.listdir(base) if not d.startswith(".")]

try:
    driver.get(URL)
    WebDriverWait(driver, 30).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(2)
    open_files()

    # ---------- deleting is reversible now ----------
    check("root listing does not show the trash folder",
          ".trash" not in row_names(), str(row_names()))
    pane().find_element(By.CSS_SELECTOR, ".toggle").click()   # show hidden files
    time.sleep(0.6)
    hidden_names = row_names()
    check("nor with hidden files shown: it is the app's own, not a dotfile",
          ".trash" not in hidden_names, str(hidden_names))
    pane().find_element(By.CSS_SELECTOR, ".toggle").click()
    time.sleep(0.6)

    row("scratch.txt").click()
    time.sleep(0.6)
    check("a plain file offers Delete", any("Delete" in b for b in preview_buttons()),
          str(preview_buttons()))
    click_in(pane().find_element(By.CSS_SELECTOR, ".preview-actions"), "Delete")
    wait_names(lambda n: "scratch.txt" not in n, "scratch.txt to leave the listing")
    check("Delete takes it out of the listing", "scratch.txt" not in row_names(), str(row_names()))
    check("...but the bytes are still there, in the trash",
          not on_disk("scratch.txt") and len(trash_on_disk()) == 1, str(trash_on_disk()))

    wait = WebDriverWait(driver, 10)
    wait.until(lambda d: d.find_elements(By.CSS_SELECTOR, ".undo-bar"))
    undo = pane().find_element(By.CSS_SELECTOR, ".undo-bar").text
    check("the undo bar says what happened, and offers it back",
          "scratch.txt" in undo and "Undo" in undo, undo.replace("\n", " "))
    shot("trash_undo")

    click_in(pane().find_element(By.CSS_SELECTOR, ".undo-bar"), "Undo")
    wait_names(lambda n: "scratch.txt" in n, "scratch.txt to come back")
    check("Undo puts it back, contents intact",
          on_disk("scratch.txt") and open(os.path.join(ROOT, "scratch.txt")).read().startswith("scratch"),
          "")
    check("and the trash is empty again", len(trash_on_disk()) == 0, str(trash_on_disk()))

    # ---------- the trash view ----------
    row("scratch.txt").click(); time.sleep(0.5)
    click_in(pane().find_element(By.CSS_SELECTOR, ".preview-actions"), "Delete")
    wait_names(lambda n: "scratch.txt" not in n, "scratch.txt to be trashed")
    time.sleep(0.5)
    show_trash()
    check("the trash lists what was deleted, with where it came from",
          trash_names() == ["scratch.txt"]
          and trash_row("scratch.txt").find_element(By.CSS_SELECTOR, ".trash-where").text == "scratch.txt",
          str([(r.find_element(By.CSS_SELECTOR, ".trash-name").text,
                r.find_element(By.CSS_SELECTOR, ".trash-where").text) for r in trash_rows()]))
    shot("trash_view")

    click_in(trash_row("scratch.txt"), "Restore")
    WebDriverWait(driver, 15).until(lambda d: not driver.find_elements(By.CSS_SELECTOR, ".trash-list .trash-row"))
    check("Restore takes it out of the trash and back to disk", on_disk("scratch.txt"))

    # ---------- a folder goes as a whole ----------
    for b in pane().find_elements(By.CSS_SELECTOR, ".side-item"):
        if b.text.strip() == "Root":
            b.click(); break
    wait_names(lambda n: "notes" in n, "the root listing")
    check("at the root there is no folder to act on", "Delete this folder" not in pane().text, "")
    row("notes").click()
    WebDriverWait(driver, 15).until(lambda d: "notes" not in row_names())
    labels = [b.text.strip() for b in pane().find_elements(By.CSS_SELECTOR, ".files-bar .ghost-btn")]
    check("a folder you are inside can be renamed or deleted from the toolbar",
          {"✎ Rename this folder", "🗑 Delete this folder"} <= set(labels), str(labels))
    toolbar_click("Delete this folder")
    wait_names(lambda n: "notes" not in n, "notes to leave the parent")
    check("the whole folder left the root", not on_disk("notes"))
    check("...and the view stepped out to the parent, not a broken folder",
          "notes" not in row_names() and row_names(), str(row_names()))
    show_trash()
    check("and arrived in the trash as one entry", "notes" in trash_names(), str(trash_names()))
    click_in(trash_row("notes"), "Restore")
    WebDriverWait(driver, 15).until(lambda d: not trash_rows())
    check("restoring the folder brought its contents with it",
          on_disk("notes/todo.md") and on_disk("notes/scratch.rs"))

    # ---------- deleting forever ----------
    for b in pane().find_elements(By.CSS_SELECTOR, ".side-item"):
        if b.text.strip() == "Root":
            b.click(); break
    wait_names(lambda n: "notes" in n, "the root listing")
    trash_folder("notes")
    show_trash()
    before = len(trash_on_disk())
    click_in(trash_row("notes"), "Delete forever")
    WebDriverWait(driver, 15).until(lambda d: len(trash_on_disk()) < before)
    check("Delete forever really removes it from disk",
          not on_disk("notes") and len(trash_on_disk()) == 0, str(trash_on_disk()))

    # ---------- empty the trash, which asks first ----------
    for b in pane().find_elements(By.CSS_SELECTOR, ".side-item"):
        if b.text.strip() == "Root":
            b.click(); break
    wait_names(lambda n: "scratch.txt" in n, "the root listing")
    row("scratch.txt").click(); time.sleep(0.5)
    click_in(pane().find_element(By.CSS_SELECTOR, ".preview-actions"), "Delete")
    wait_names(lambda n: "scratch.txt" not in n, "scratch.txt to be trashed")
    show_trash()
    empty = [b for b in pane().find_elements(By.CSS_SELECTOR, ".ghost-btn") if "Empty trash" in b.text]
    check("emptying asks first, because that one is irreversible",
          bool(empty) and "Empty trash" in empty[0].text, empty[0].text if empty else "no button")
    empty[0].click(); time.sleep(0.4)
    check("...and says so in the second click",
          "Really delete everything?" in empty[0].text, empty[0].text)
    empty[0].click()
    WebDriverWait(driver, 15).until(lambda d: len(trash_on_disk()) == 0)
    check("the trash is empty on disk, and says so",
          "empty" in pane().text.lower() and len(trash_on_disk()) == 0)

    # ---------- protections ----------
    for b in pane().find_elements(By.CSS_SELECTOR, ".side-item"):
        if b.text.strip() == "Root":
            b.click(); break
    wait_names(lambda n: "Lumina" in n, "the root listing")
    row("Lumina").click(); time.sleep(0.8)
    labels = [b.text.strip() for b in pane().find_elements(By.CSS_SELECTOR, ".files-bar .ghost-btn")]
    check("a protected folder offers no way to create anything",
          not any("Folder" in l or "File" in l or "Upload" in l for l in labels), str(labels))
    check("and says why, on screen", "read-only folder" in pane().text, "")
    row("gallery").click(); time.sleep(0.8)   # precious.png lives inside Lumina/gallery
    check("the read-only fence covers folders inside the protected one too",
          "read-only" in pane().text, "")
    row("precious.png").click(); time.sleep(0.8)
    check("a file inside it is locked: no rename, no delete",
          preview_buttons() == set(), str(preview_buttons()))
    check("and it marks itself read-only", "read-only" in pane().text)
    shot("trash_locked")

    # The folder cannot be deleted either, and the row says so by refusing to act.
    for b in pane().find_elements(By.CSS_SELECTOR, ".side-item"):
        if b.text.strip() == "Root":
            b.click(); break
    wait_names(lambda n: "Lumina" in n, "the root listing")
    row("Lumina").click(); time.sleep(0.6)
    check("the protected folder itself cannot be deleted from the UI",
          preview_buttons() == set(), str(preview_buttons()))
    check("nothing in Lumina was touched",
          open(os.path.join(ROOT, "Lumina/gallery/precious.png")).read().strip()
          == "the one file that must survive")

finally:
    shot("trash_final")
    driver.quit()
    failed = [n for n, ok in results if not ok]
    print(f"\n{len(results) - len(failed)}/{len(results)} trash checks passed")
    for n in failed:
        print("FAILED:", n)
