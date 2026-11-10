import os, subprocess, sys, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.common.keys import Keys
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait
from selenium.webdriver.firefox.options import Options

BASE = os.environ.get("PROBE_URL", "http://127.0.0.1:8080/")
ROOT = os.environ.get("PROBE_ROOT", os.environ.get("PROBE_ROOT") or "/tmp/web-desktop-e2e/root")
SHOTS = os.path.join(os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e", "shots")
os.makedirs(SHOTS, exist_ok=True)

# fixtures: three decodable photos, one format browsers cannot decode, one text file
album = f"{ROOT}/album"
subprocess.run(["rm", "-rf", album], capture_output=True)
os.makedirs(f"{album}/trip", exist_ok=True)
for name, colour in (("red.png", "red"), ("blue.jpg", "blue"), ("green.gif", "green")):
    subprocess.run(["magick", "-size", "200x140", f"xc:{colour}", f"{album}/{name}"], check=True)
subprocess.run(["magick", "-size", "200x140", "xc:teal", f"{album}/trip/teal.png"], check=True)
subprocess.run(["magick", "-size", "60x40", "xc:white", f"{album}/scan.tiff"], check=True)
open(f"{album}/notes.txt", "w").write("not a photo\n")

opts = Options(); opts.add_argument("--headless"); opts.add_argument("--window-size=1500,1000")
driver = webdriver.Firefox(options=opts); driver.set_window_size(1500, 1000)
results = []
def check(n, c, x=""):
    results.append((n, bool(c)))
    print(("PASS  " if c else "FAIL  ") + n + (f"   [{x}]" if x else ""), flush=True)

def open_app(name):
    driver.find_element(By.CSS_SELECTOR, ".start-btn").click()
    WebDriverWait(driver, 10).until(EC.visibility_of_element_located((By.CSS_SELECTOR, ".start-menu")))
    time.sleep(0.4)
    for it in driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item"):
        if name in it.text:
            it.click()
            return True
    return False

def tiles(selector=""):
    return driver.find_elements(By.CSS_SELECTOR, f".gallery-tile{selector}")

def tile_names(selector=""):
    return [t.find_element(By.CSS_SELECTOR, ".tile-name").text for t in tiles(selector)]

def tile(name):
    for t in tiles():
        if t.find_element(By.CSS_SELECTOR, ".tile-name").text == name:
            return t

def decode_counts():
    return driver.execute_script("""
        const imgs = [...document.querySelectorAll('.tile-image')];
        return {total: imgs.length, ok: imgs.filter(i => i.complete && i.naturalWidth > 0).length};
    """)

try:
    driver.get(BASE)
    WebDriverWait(driver, 30).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(2)
    check("the gallery is offered in the start menu", open_app("Gallery"))
    WebDriverWait(driver, 20).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".gallery")))
    time.sleep(1.2)

    # ---------------- navigate to the album ----------------
    check("the gallery shows the root's folders as tiles", "album" in tile_names(".folder"), str(tile_names(".folder")))
    tile("album").click()
    time.sleep(1.5)
    check("a folder opens", "/album" in driver.find_element(By.CSS_SELECTOR, ".gallery-bar").text,
          driver.find_element(By.CSS_SELECTOR, ".gallery-bar").text.replace("\n", " / "))
    photos = tile_names(".photo")
    check("photos are tiles", set(photos) == {"red.png", "blue.jpg", "green.gif"}, str(photos))
    check("a non-photo is not a tile", "notes.txt" not in tile_names())
    check("subfolders are still navigable", "trip" in tile_names(".folder"), str(tile_names(".folder")))
    check("the count reads right",
          "3 photos · 1 folder" in driver.find_element(By.CSS_SELECTOR, ".count").text,
          driver.find_element(By.CSS_SELECTOR, ".count").text)

    decoded = decode_counts()
    check("the tiles actually decode", decoded["ok"] == decoded["total"] == 3, str(decoded))

    # a format the browser cannot decode is reported, not shown broken
    note = driver.find_element(By.CSS_SELECTOR, ".gallery-note").text
    check("an undecodable image format is reported", "1 other file" in note and "cannot show" in note, note)

    # ---------------- the viewer ----------------
    tile("blue.jpg").click()
    WebDriverWait(driver, 10).until(EC.visibility_of_element_located((By.CSS_SELECTOR, ".gallery-viewer")))
    time.sleep(1.5)
    bar = driver.find_element(By.CSS_SELECTOR, ".viewer-bar").text
    check("the viewer names the photo and its size", "blue.jpg" in bar and "200 × 140 px" in bar,
          bar.replace("\n", " / "))
    check("the full image is shown",
          driver.execute_script("const i=document.querySelector('.viewer-image');"
                                "return i && i.complete && i.naturalWidth===200 && i.naturalHeight===140"))

    driver.find_element(By.CSS_SELECTOR, ".gallery-viewer").send_keys(Keys.ARROW_RIGHT)
    time.sleep(1.2)
    check("→ steps to the next photo",
          "green.gif" in driver.find_element(By.CSS_SELECTOR, ".viewer-bar").text,
          driver.find_element(By.CSS_SELECTOR, ".viewer-name").text)
    driver.find_element(By.CSS_SELECTOR, ".gallery-viewer").send_keys(Keys.ARROW_LEFT)
    time.sleep(1.2)
    check("← steps back", "blue.jpg" in driver.find_element(By.CSS_SELECTOR, ".viewer-name").text,
          driver.find_element(By.CSS_SELECTOR, ".viewer-name").text)
    driver.find_element(By.CSS_SELECTOR, ".gallery-viewer").send_keys(Keys.ARROW_LEFT)
    time.sleep(1.2)
    check("it wraps around", "red.png" in driver.find_element(By.CSS_SELECTOR, ".viewer-name").text,
          driver.find_element(By.CSS_SELECTOR, ".viewer-name").text)
    driver.save_screenshot(f"{SHOTS}/80-gallery-viewer.png")

    # ---------------- set as wallpaper ----------------
    bg_before = driver.find_element(By.CSS_SELECTOR, ".desktop").get_attribute("style")
    for b in driver.find_elements(By.CSS_SELECTOR, ".viewer-bar .ghost-btn"):
        if "wallpaper" in b.text:
            b.click()
            break
    time.sleep(1.0)
    bg_after = driver.find_element(By.CSS_SELECTOR, ".desktop").get_attribute("style")
    check("the photo becomes the desktop background",
          "fs/raw" in bg_after and bg_after != bg_before, bg_after[:90])
    check("the viewer says so", "wallpaper" in driver.find_element(By.CSS_SELECTOR, ".files-notice").text,
          driver.find_element(By.CSS_SELECTOR, ".files-notice").text)
    driver.save_screenshot(f"{SHOTS}/81-gallery-wallpaper.png")

    # ---------------- escape closes, and the folder follows the viewer ----------------
    driver.find_element(By.CSS_SELECTOR, ".gallery-viewer").send_keys(Keys.ESCAPE)
    time.sleep(0.6)
    check("Escape closes the viewer", not driver.find_elements(By.CSS_SELECTOR, ".gallery-viewer"))
    tile("trip").click()
    time.sleep(1.4)
    check("a nested folder opens", tile_names(".photo") == ["teal.png"], str(tile_names(".photo")))
    check("no viewer is left open over it", not driver.find_elements(By.CSS_SELECTOR, ".gallery-viewer"))
    check("app still mounted", len(driver.find_elements(By.CSS_SELECTOR, ".taskbar")) == 1)
finally:
    driver.quit()
    subprocess.run(["rm", "-rf", album], capture_output=True)

failed = [n for n, ok in results if not ok]
print(f"\n{len(results) - len(failed)}/{len(results)} gallery checks passed")
if failed:
    print("failed:", failed)
sys.exit(1 if failed else 0)
