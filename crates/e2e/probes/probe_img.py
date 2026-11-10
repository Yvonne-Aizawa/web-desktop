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
opts.add_argument("--window-size=1400,950")
driver = webdriver.Firefox(options=opts)
driver.set_window_size(1400, 950)

results = []
def check(name, ok, extra=""):
    results.append((name, bool(ok)))
    print(("PASS  " if ok else "FAIL  ") + name + (f"   [{extra}]" if extra else ""), flush=True)

def shot(n):
    driver.save_screenshot(f"{SHOTS}/{n}.png")

def rows():
    return driver.find_elements(By.CSS_SELECTOR, ".files-list .row")

def row(name):
    for r in rows():
        if r.find_element(By.CSS_SELECTOR, ".row-name").text == name:
            return r
    return None

def wait_rows():
    WebDriverWait(driver, 20).until(lambda d: len(d.find_elements(By.CSS_SELECTOR, ".files-list .row")) > 0)
    time.sleep(0.4)

def open_app(name):
    driver.find_element(By.CSS_SELECTOR, ".start-btn").click()
    time.sleep(0.5)
    for it in driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item"):
        if name in it.text:
            it.click()
            break
    time.sleep(1.3)

def preview_text():
    return driver.find_elements(By.CSS_SELECTOR, ".files-preview")[0].text

def img_state(selector=".preview-image"):
    """naturalWidth/complete/visibility of an <img>, read from the DOM."""
    return driver.execute_script("""
        const i = document.querySelector(arguments[0]);
        if (!i) return null;
        return {complete: i.complete, w: i.naturalWidth, h: i.naturalHeight,
                src: i.getAttribute('src'), lazy: i.getAttribute('loading'),
                visible: !!(i.offsetWidth || i.offsetHeight) &&
                         getComputedStyle(i.closest('a') || i).display !== 'none'};
    """, selector)

def select_file(name):
    r = row(name)
    assert r is not None, f"no row for {name}"
    r.click()
    time.sleep(1.2)

try:
    driver.get(URL)
    WebDriverWait(driver, 30).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(2)
    open_app("Files")
    wait_rows()
    files = driver.find_elements(By.CSS_SELECTOR, ".window .files")[0]
    row("preview-test").click()
    wait_rows()
    names = [r.find_element(By.CSS_SELECTOR, ".row-name").text for r in rows()]
    check("preview fixtures listed", "red.png" in names and "logo.svg" in names, str(names))

    # ---------- row thumbnails ----------
    thumbs = driver.find_elements(By.CSS_SELECTOR, ".files-list .row-thumb")
    check("image rows get a lazy thumbnail", len(thumbs) >= 6, f"{len(thumbs)} thumbnails")
    lazy = driver.execute_script(
        "return [...document.querySelectorAll('.row-thumb')].every(i => i.getAttribute('loading') === 'lazy')")
    check("thumbnails are lazy-loaded", lazy)
    loaded = driver.execute_script("""
        return [...document.querySelectorAll('.row-thumb')]
            .filter(i => i.complete && i.naturalWidth > 0).length;
    """)
    check("thumbnails actually decode", loaded >= 6, f"{loaded} decoded")
    big_row = row("huge.bmp")
    check("oversized image falls back to the icon",
          not big_row.find_elements(By.CSS_SELECTOR, ".row-thumb")
          and big_row.find_element(By.CSS_SELECTOR, ".row-icon").text.strip() == "🖼")
    tiff_row = row("teal.tiff")
    check("undecodable format keeps the image icon, no thumbnail",
          not tiff_row.find_elements(By.CSS_SELECTOR, ".row-thumb")
          and tiff_row.find_element(By.CSS_SELECTOR, ".row-icon").text.strip() == "🖼")
    shot("30-files-thumbnails")

    # ---------- one format at a time ----------
    for name, w, h in [("red.png", 240, 160), ("blue.jpg", 240, 160), ("green.gif", 240, 160),
                       ("purple.webp", 240, 160), ("orange.bmp", 240, 160), ("logo.svg", 120, 80)]:
        select_file(name)
        st = img_state()
        check(f"{name} renders at its real size",
              st and st["complete"] and st["w"] == w and st["h"] == h and st["visible"],
              str(st))
        dims = files.find_element(By.CSS_SELECTOR, ".preview-dims").text
        check(f"{name} reports its dimensions", f"{w} × {h} px" in dims, dims)
        check(f"{name} is served from the raw endpoint",
              st and "/api/fs/raw?path=preview-test%2F" in st["src"], st["src"] if st else "")
    shot("31-preview-png")

    # ---------- decode failures ----------
    select_file("broken.png")
    st = img_state()
    err = files.find_element(By.CSS_SELECTOR, ".files-preview .preview-error").text
    check("a misnamed png reports a decode failure",
          "could not decode" in err and "image/png" in err and st and not st["visible"], err[:70])
    select_file("fake.jpg")
    err = files.find_element(By.CSS_SELECTOR, ".files-preview .preview-error").text
    check("a misnamed jpg reports a decode failure", "could not decode" in err, err[:70])
    shot("32-preview-decode-failure")

    # ---------- formats the browser cannot decode ----------
    select_file("teal.tiff")
    pv = preview_text()
    check("tiff gets a message instead of a broken image",
          "image/tiff" in pv and not driver.find_elements(By.CSS_SELECTOR, ".preview-image"), pv[:60].replace("\n", " / "))

    # ---------- oversized ----------
    select_file("huge.bmp")
    pv = preview_text()
    check("oversized image reports the server limit", "limit is 67108864" in pv, pv[:80].replace("\n", " / "))
    check("oversized image does not attempt an img",
          not driver.find_elements(By.CSS_SELECTOR, ".files-preview .preview-image"))
    shot("33-preview-too-large")

    # ---------- open in a new tab link ----------
    select_file("red.png")
    href = driver.find_element(By.CSS_SELECTOR, ".image-frame").get_attribute("href")
    check("image can be opened in a new tab", "/api/fs/raw?path=preview-test%2Fred.png" in href, href)
    check("link is rel=noopener", driver.find_element(By.CSS_SELECTOR, ".image-frame").get_attribute("rel") == "noopener")

    # ---------- the terminal is honest about images ----------
    open_app("Terminal")
    time.sleep(0.8)
    driver.execute_script("document.querySelector('.term-input').focus()")
    time.sleep(0.1)
    inp = driver.find_elements(By.CSS_SELECTOR, ".term-input")[0]
    inp.send_keys("cat preview-test/red.png")
    inp.send_keys(Keys.ENTER)
    time.sleep(1.0)
    t = driver.find_element(By.CSS_SELECTOR, ".terminal .term-scroll").text
    check("terminal cat points at the Files app for an image",
          "image (image/png" in t and "open it in Files" in t, t.splitlines()[-1][:80])

    check("app still mounted", len(driver.find_elements(By.CSS_SELECTOR, ".taskbar")) == 1)
finally:
    driver.quit()

failed = [n for n, ok in results if not ok]
print(f"\n{len(results) - len(failed)}/{len(results)} checks passed")
if failed:
    print("failed:", failed)
sys.exit(1 if failed else 0)
