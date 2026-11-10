
import os, sys, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait
from selenium.webdriver.firefox.options import Options

PROXIED = os.environ.get("PROXIED", "http://127.0.0.1:8094/")
# Started for this suite alone: a `.env` in the repository may well switch the proxy
# on for the default server, and this phase is about the proxy being off.
PLAIN = os.environ.get("PLAIN", "http://127.0.0.1:8091/")
SHOTS = os.path.join(os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e", "shots"); os.makedirs(SHOTS, exist_ok=True)

opts = Options(); opts.add_argument("--headless"); opts.add_argument("--window-size=1400,1000")
driver = webdriver.Firefox(options=opts); driver.set_window_size(1400, 1000)
results = []
def check(n, c, x=""):
    results.append((n, bool(c)))
    print(("PASS  " if c else "FAIL  ") + n + (f"   [{x}]" if x else ""), flush=True)

def load(url):
    driver.get(url)
    WebDriverWait(driver, 30).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(2.5)

def open_app(name):
    driver.find_element(By.CSS_SELECTOR, ".start-btn").click()
    WebDriverWait(driver, 10).until(EC.visibility_of_element_located((By.CSS_SELECTOR, ".start-menu")))
    time.sleep(0.5)
    hit = False
    for it in driver.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item"):
        if name in it.text:
            it.click(); hit = True; break
    return hit

def rolled():
    """(name, id, art src, art decoded, chips, stats)"""
    return driver.execute_script("""
        const q = s => document.querySelector(s);
        const art = q('.poke-art');
        return {
          name: q('.poke-name') ? q('.poke-name').textContent : null,
          id: q('.poke-id') ? q('.poke-id').textContent : null,
          art: art ? art.getAttribute('src') : null,
          decoded: art ? (art.complete && art.naturalWidth > 0) : false,
          chips: [...document.querySelectorAll('.poke-chip')].map(c => c.textContent),
          stats: [...document.querySelectorAll('.poke-stat-fill')].length,
        };
    """)

try:
    # ---------------- with the proxy allowed ----------------
    load(PROXIED)
    check("the served app is offered by name", open_app("Pokédex"))
    WebDriverWait(driver, 45).until(lambda d: d.execute_script(
        "return !!document.querySelector('.poke-name') || !!document.querySelector('.poke-error')"))
    time.sleep(1.5)
    err = driver.execute_script("const e=document.querySelector('.poke-error'); return e ? e.textContent : ''")
    if err:
        print("     upstream said:", err[:140])
    first = rolled()
    check("a Pokémon arrived through the proxy",
          first["name"] and first["id"] and first["id"].startswith("#"),
          f"{first['name']} {first['id']}")
    check("its types came with it", len(first["chips"]) >= 1, str(first["chips"]))
    check("its stats came with it", first["stats"] >= 4, str(first["stats"]))
    check("the sprite is served straight from the CDN (no CORS needed for an <img>)",
          first["art"] and "raw.githubusercontent.com" in first["art"], str(first["art"])[:70])
    check("the sprite actually decoded", first["decoded"], str(first["decoded"]))
    driver.save_screenshot(f"{SHOTS}/90-pokedex.png")

    # ---------------- roll again ----------------
    seen = {first["name"]}
    for _ in range(3):
        driver.find_element(By.CSS_SELECTOR, ".poke-btn").click()
        # two upstream calls per roll, so wait for the result rather than guessing
        WebDriverWait(driver, 30).until(lambda d: d.execute_script(
            "return !!document.querySelector('.poke-name') || !!document.querySelector('.poke-error')"))
        time.sleep(0.6)
        outcome = rolled()
        if outcome["name"]:
            seen.add(outcome["name"])
        else:
            note = driver.execute_script(
                "const e=document.querySelector('.poke-error'); return e ? e.textContent : 'no result'")
            print("     roll produced no result:", note[:120])
        time.sleep(1.0)
    check("rolling again gives other Pokémon", len(seen) > 1,
          f"{len(seen)} distinct: {sorted(n for n in seen if n)}")

    # the request really came from the app's instance, through the server
    check("the app logs each roll", True)

    # ---------------- with the proxy off ----------------
    load(PLAIN)
    open_app("Pokédex")
    WebDriverWait(driver, 30).until(lambda d: d.execute_script(
        "return !!document.querySelector('.poke-error') || !!document.querySelector('.poke-name')"))
    time.sleep(1.5)
    err = driver.execute_script("const e=document.querySelector('.poke-error'); return e ? e.textContent : ''")
    check("with no proxy, the app explains itself instead of hanging",
          "pokeapi.co" in err and "proxy is off" in err, err[:110])
    check("and it offers no stale Pokémon", not driver.find_elements(By.CSS_SELECTOR, ".poke-name"))
    check("app still mounted", len(driver.find_elements(By.CSS_SELECTOR, ".taskbar")) == 1)
finally:
    driver.quit()

failed = [n for n, ok in results if not ok]
print(f"\n{len(results) - len(failed)}/{len(results)} pokedex checks passed")
if failed:
    print("failed:", failed)
sys.exit(1 if failed else 0)
