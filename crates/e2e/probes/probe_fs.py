import os, sys, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.common.keys import Keys
from selenium.webdriver.firefox.options import Options
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait

URL = os.environ.get("PROBE_URL", "http://127.0.0.1:8080/")
SHOTS = os.path.join(os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e", "shots")
os.makedirs(SHOTS, exist_ok=True)

opts = Options()
opts.add_argument("--headless")
opts.add_argument("--window-size=1400,900")
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

def row(name):
    for r in rows():
        if r.find_element(By.CSS_SELECTOR, ".row-name").text == name:
            return r
    return None

def wait_rows():
    WebDriverWait(driver, 20).until(lambda d: len(d.find_elements(By.CSS_SELECTOR, ".files-list .row")) > 0)
    time.sleep(0.3)

def crumbs():
    return [c.text for c in driver.find_elements(By.CSS_SELECTOR, ".crumb-seg")]

def preview_text():
    el = driver.find_elements(By.CSS_SELECTOR, ".files-preview")
    return el[0].text if el else ""

def open_app(name):
    driver.find_element(By.CSS_SELECTOR, ".start-btn").click()
    time.sleep(0.5)
    for it in driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item"):
        if name in it.text:
            it.click()
            break
    time.sleep(1.2)

def focus_window(i):
    btns = driver.find_elements(By.CSS_SELECTOR, ".task-btn")
    if "active" not in (btns[i].get_attribute("class") or ""):
        btns[i].click()
        time.sleep(0.5)

def files_pane():
    return driver.find_elements(By.CSS_SELECTOR, ".window .files")[0]

def term(kind, text):
    inp = driver.find_elements(By.CSS_SELECTOR, ".term-input")[0]
    inp.click()
    time.sleep(0.15)
    inp.send_keys(text)
    inp.send_keys(Keys.ENTER)
    time.sleep(0.7)

def term_text():
    return driver.find_elements(By.CSS_SELECTOR, ".terminal .term-scroll")[0].text

try:
    driver.get(URL)
    wait = WebDriverWait(driver, 30)
    wait.until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(2)

    # ---------- open the Files app ----------
    open_app("Files")
    wait_rows()
    check("Files window opens on the real sandbox",
          len(driver.find_elements(By.CSS_SELECTOR, ".window .files")) == 1)

    names = row_names()
    check("root listing comes from disk",
          {"README.txt", "notes", "testdir"} <= set(names), str(names))
    check("dotfile hidden by default", ".hidden-example" not in names)
    summary = files_pane().find_element(By.CSS_SELECTOR, ".count").text
    check("summary counts hidden entries", "hidden" in summary, summary)
    check("sidebar shows the served root",
          os.environ.get("PROBE_ROOT") or "/tmp/web-desktop-e2e/root" in files_pane().find_element(By.CSS_SELECTOR, ".side-note").text)
    shot("20-files-root")

    # ---------- hidden toggle ----------
    files_pane().find_element(By.CSS_SELECTOR, ".toggle").click()
    time.sleep(0.8)
    check("show hidden reveals dotfiles", ".hidden-example" in row_names(), str(row_names()))
    files_pane().find_element(By.CSS_SELECTOR, ".toggle").click()
    time.sleep(0.8)
    check("hiding again removes them", ".hidden-example" not in row_names())

    # ---------- navigate in, preview a file ----------
    row("notes").click()
    wait_rows()
    check("clicking a folder navigates", row_names() == ["scratch.rs", "todo.md"], str(row_names()))
    check("breadcrumb follows", crumbs() == ["Sandbox", "notes"], str(crumbs()))
    check("up button enabled away from root",
          files_pane().find_elements(By.CSS_SELECTOR, ".files-bar .ghost-btn")[0].is_enabled())

    row("todo.md").click()
    time.sleep(0.8)
    pv = preview_text()
    check("text file previews its content", "# todo" in pv and "write support" in pv, pv[:60].replace("\n", " / "))
    link = driver.find_elements(By.CSS_SELECTOR, ".files-preview a")
    href = link[0].get_attribute("href") if link else ""
    check("download link points at the api", "/api/fs/download?path=notes%2Ftodo.md" in href, href)
    check("download link has the file name", link and link[0].get_attribute("download") == "todo.md")
    shot("21-files-preview")

    # ---------- breadcrumb navigation ----------
    files_pane().find_elements(By.CSS_SELECTOR, ".crumb-seg")[0].click()
    wait_rows()
    check("breadcrumb returns to the root", crumbs() == ["Sandbox"] and "README.txt" in row_names())

    # ---------- a folder with a blocked symlink and binaries ----------
    row("testdir").click()
    wait_rows()
    names = row_names()
    check("subfolder listing", {"escape", "notes-link", "big.txt", "blob.bin", "greeting.txt"} <= set(names), str(names))
    esc = row("escape")
    check("escaping symlink is marked blocked", "blocked" in (esc.get_attribute("class") or ""), esc.get_attribute("class"))
    check("in-root symlink shows as a folder", "dir" in (row("notes-link").get_attribute("class") or ""))

    esc.click()
    time.sleep(0.8)
    pv = preview_text()
    check("opening a blocked link explains itself instead of failing",
          "outside the file browser root" in pv, pv[:70])
    check("blocked click does not navigate", crumbs() == ["Sandbox", "testdir"], str(crumbs()))

    row("big.txt").click()
    time.sleep(1.0)
    pv = preview_text()
    check("truncated preview says so", "first 256 KiB" in pv, pv[:60].replace("\n", " / "))
    pre_len = driver.execute_script(
        "const p=document.querySelector('.files-preview pre'); return p ? p.textContent.length : -1")
    check("preview body is capped at 256 KiB", pre_len == 262144, str(pre_len))

    row("blob.bin").click()
    time.sleep(0.8)
    pv = preview_text()
    check("binary file reports mime and size", "application/octet-stream" in pv and "18 B" in pv, pv[:70].replace("\n", " / "))
    check("binary file renders no pre",
          not driver.find_elements(By.CSS_SELECTOR, ".files-preview pre"))
    shot("22-files-binary")

    # ---------- navigate through a symlink inside the root, then Up ----------
    row("notes-link").click()
    wait_rows()
    check("symlinked folder is browsable", crumbs() == ["Sandbox", "testdir", "notes-link"], str(crumbs()))
    files_pane().find_elements(By.CSS_SELECTOR, ".files-bar .ghost-btn")[0].click()
    wait_rows()
    check("up button works", crumbs() == ["Sandbox", "testdir"], str(crumbs()))

    # ---------- reload ----------
    before = row_names()
    files_pane().find_elements(By.CSS_SELECTOR, ".files-bar .ghost-btn")[1].click()
    time.sleep(1.0)
    check("reload keeps the same folder", row_names() == before and crumbs() == ["Sandbox", "testdir"])

    # ---------- the terminal reads the same folder ----------
    open_app("Terminal")
    time.sleep(0.8)
    focus_window(0)
    term("ls", "")
    t = term_text()
    check("terminal ls lists the sandbox", "notes/" in t and "README.txt" in t, t.splitlines()[-1][:60])
    check("terminal ls hides dotfiles", ".hidden-example" not in t)
    term("cd notes", "")
    input_prompt = driver.find_elements(By.CSS_SELECTOR, ".term-ps1")[0].text
    check("prompt follows cd", input_prompt == "guest@web-desktop:/notes $", input_prompt)
    term("pwd", "")
    check("pwd reflects the directory", "/notes" in term_text().splitlines()[-1], term_text().splitlines()[-1])
    term("cat todo.md", "")
    check("terminal cat reads the file", "write support" in term_text())
    term("ls ..", "")
    check("ls with an argument", "README.txt" in term_text().splitlines()[-1], term_text().splitlines()[-1][:60])
    term("cd ..", "")
    check("cd .. returns to the root",
          driver.find_elements(By.CSS_SELECTOR, ".term-ps1")[0].text == "guest@web-desktop:/ $")
    term("cat blob.bin", "")
    check("terminal cat refuses binaries", "binary file (application/octet-stream" in term_text(), term_text().splitlines()[-1][:70])
    term("cat nope.txt", "")
    check("terminal cat reports a missing file", "does not exist" in term_text(), term_text().splitlines()[-1][:70])
    term("cd testdir/escape", "")
    check("terminal cd into a blocked link is refused",
          "outside the file browser root" in term_text(), term_text().splitlines()[-1][:70])
    check("prompt unchanged after a refused cd",
          driver.find_elements(By.CSS_SELECTOR, ".term-ps1")[0].text == "guest@web-desktop:/ $")
    shot("23-terminal-fs")

    # ---------- settings report the configuration ----------
    open_app("Settings")
    time.sleep(1.0)
    st = driver.find_elements(By.CSS_SELECTOR, ".window .settings")[0].text
    check("settings show the served root", os.environ.get("PROBE_ROOT") or "/tmp/web-desktop-e2e/root" in st, st[:40].replace("\n", " / "))
    check("settings say read-only", "read-only" in st)
    shot("24-settings-fs")

    # ---------- still alive ----------
    check("app still mounted", len(driver.find_elements(By.CSS_SELECTOR, ".taskbar")) == 1)
finally:
    driver.quit()

failed = [n for n, ok in results if not ok]
print(f"\n{len(results) - len(failed)}/{len(results)} checks passed")
if failed:
    print("failed:", failed)
sys.exit(1 if failed else 0)
