import os
WORK = os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e"
REPO = os.environ.get("E2E_REPO") or os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
os.makedirs(os.path.join(WORK, "shots"), exist_ok=True)

import time
from collections import Counter
from PIL import Image
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.firefox.options import Options
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait

opts = Options(); opts.add_argument("--headless"); opts.add_argument("--window-size=1400,950")
d = webdriver.Firefox(options=opts); d.set_window_size(1400, 950)
ok = []
def check(n, c, x=""):
    ok.append(c); print(("PASS  " if c else "FAIL  ") + n + (f"   [{x}]" if x else ""))

try:
    d.get("http://127.0.0.1:8080/")
    WebDriverWait(d, 30).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(1.5)
    d.find_element(By.CSS_SELECTOR, ".start-btn").click(); time.sleep(0.5)
    for it in d.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item"):
        if "Files" in it.text: it.click(); break
    time.sleep(1.5)

    def row(name):
        for r in d.find_elements(By.CSS_SELECTOR, ".files-list .row"):
            if r.find_element(By.CSS_SELECTOR, ".row-name").text == name: return r

    row("preview-test").click(); time.sleep(1.2)

    def paint(name, expect_rgb):
        row(name).click(); time.sleep(1.4)
        el = d.find_element(By.CSS_SELECTOR, ".preview-image")
        rect = el.rect
        check(f"{name}: <img> box matches the file", rect["width"] > 8 and rect["height"] > 8,
              f"{int(rect['width'])}x{int(rect['height'])}")
        d.save_screenshot(os.path.join(WORK, "shots", "paint-full.png"))
        im = Image.open(os.path.join(WORK, "shots", "paint-full.png")).convert("RGB")
        # inset a little to avoid the rounded corners / box-shadow edges
        crop = im.crop((int(rect["x"]) + 3, int(rect["y"]) + 3,
                        int(rect["x"] + rect["width"]) - 3, int(rect["y"] + rect["height"]) - 3))
        crop.save(os.path.join(WORK, "shots", f"paint-{name}.png"))
        common, count = Counter(crop.getdata()).most_common(1)[0]
        near = sum(abs(a - b) <= 12 for a, b in zip(common, expect_rgb)) == 3
        check(f"{name}: dominant painted colour is {expect_rgb}", near,
              f"dominant={common} {count}/{crop.width*crop.height} px")
        return crop

    # fixture colours: magick "xc:red/#FF0000", "xc:blue", "xc:green/#008000",
    # the svg body is #131a2b with a #4ade80 disc
    paint("red.png", (255, 0, 0))
    paint("blue.jpg", (0, 0, 255))
    paint("green.gif", (0, 128, 0))
    crop = paint("logo.svg", (19, 26, 43))
    green = sum(1 for p in crop.getdata() if abs(p[0]-0x4a) < 20 and abs(p[1]-0xde) < 20 and abs(p[2]-0x80) < 20)
    check("logo.svg: green disc drawn too", green / (crop.width * crop.height) > 0.05,
          f"{green/(crop.width*crop.height):.0%} green px")
finally:
    d.quit()
print(f"\n{sum(ok)}/{len(ok)} paint checks passed")
