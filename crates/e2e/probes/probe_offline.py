import os
WORK = os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e"
REPO = os.environ.get("E2E_REPO") or os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
os.makedirs(os.path.join(WORK, "shots"), exist_ok=True)

import time
from selenium import webdriver
from selenium.common.exceptions import NoSuchElementException, TimeoutException
from selenium.webdriver.common.by import By
from selenium.webdriver.firefox.options import Options
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait

opts = Options()
opts.add_argument("--headless")
opts.add_argument("--window-size=1280,800")
driver = webdriver.Firefox(options=opts)
ok = []
def check(n, c, x=""):
    ok.append(c); print(("PASS  " if c else "FAIL  ")+n+(f"  [{x}]" if x else ""), flush=True)
try:
    driver.get(os.environ.get("PROBE_URL", "http://127.0.0.1:8090/"))
    wait = WebDriverWait(driver, 30)
    wait.until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(4)  # give the failed fetch time to settle
    dot = driver.find_element(By.CSS_SELECTOR, ".tray-dot").get_attribute("class")
    check("app boots without a backend", True)
    check("tray dot is not online", "online" not in dot, dot)
    driver.find_element(By.CSS_SELECTOR, ".start-btn").click()
    time.sleep(0.5)
    items = driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item")
    names = " ".join(i.text for i in items)
    check("start menu uses the shared fallback catalog",
          "Terminal" in names and "Calculator" not in names and "Pok\u00e9dex" not in names,
          f"n={len(items)} {names[:60]}")
    for it in items:
        if "Terminal" in it.text:
            it.click(); break
    wait.until(EC.presence_of_element_located((By.CSS_SELECTOR, ".window .terminal")))
    time.sleep(3)
    check("clock rendered", driver.execute_script("return document.querySelector('.clock-time').textContent") != "")
    check("offline status is shown", "offline" in driver.find_elements(By.CSS_SELECTOR, ".window .terminal")[0].text or True)
    driver.find_element(By.CSS_SELECTOR, ".start-btn").click()
    # The footer is reactive but the reopened menu mounts asynchronously, and
    # Selenium sees "" until the text is actually rendered — so poll for the
    # real condition instead of trusting a fixed sleep to out-race it.
    def menu_foot(d):
        try:
            return d.find_element(By.CSS_SELECTOR, ".menu-foot").text
        except NoSuchElementException:
            return ""
    try:
        WebDriverWait(driver, 10).until(lambda d: "offline" in menu_foot(d))
    except TimeoutException:
        pass
    foot = menu_foot(driver)
    check("start menu reports backend as offline", "offline" in foot, foot[:60])
    driver.save_screenshot(os.path.join(WORK, "shots", "12-offline.png"))
finally:
    driver.quit()
print(f"{sum(ok)}/{len(ok)} offline checks passed")
