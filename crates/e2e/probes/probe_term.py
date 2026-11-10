
import json, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.common.keys import Keys
from selenium.webdriver.firefox.options import Options
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait

opts = Options(); opts.add_argument("--headless"); opts.add_argument("--window-size=1400,900")
d = webdriver.Firefox(options=opts); d.set_window_size(1400, 900)
try:
    d.get("http://127.0.0.1:8080/")
    WebDriverWait(d, 30).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".taskbar")))
    time.sleep(1.5)
    d.execute_script("""
        window.__err = [];
        const oe = console.error.bind(console);
        console.error = function(...a) { window.__err.push(a.map(String).join(' ')); oe(...a); };
        window.addEventListener('error', e => window.__err.push('onerror: ' + (e.message||'') + ' | ' + (e.error && e.error.stack || '')));
    """)
    d.find_element(By.CSS_SELECTOR, ".start-btn").click(); time.sleep(0.5)
    for it in d.find_elements(By.CSS_SELECTOR, ".start-menu .menu-item"):
        if "Terminal" in it.text:
            it.click(); break
    time.sleep(1.2)
    inp = d.find_element(By.CSS_SELECTOR, ".term-input")
    d.execute_script("document.querySelector('.term-input').focus()")
    inp.send_keys("ls")
    inp.send_keys(Keys.ENTER)
    time.sleep(1.5)
    print("term text:", repr(d.find_element(By.CSS_SELECTOR, ".term-scroll").text[:200]))
    print("input value:", repr(d.execute_script("return document.querySelector('.term-input').value")))
    print("errors:", json.dumps(d.execute_script("return window.__err"))[:1500])
finally:
    d.quit()
