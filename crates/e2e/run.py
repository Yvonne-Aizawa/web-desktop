"""Run the browser suite: bring up a server and its fixtures, run one probe, tear down.

Usage:
    python3 run.py                 # every scenario
    python3 run.py --list          # what there is
    python3 run.py --only trash    # one or more, comma separated
    python3 run.py --keep          # leave fixtures, screenshots and logs in $E2E_WORK

Driven by `cargo xtask e2e`, which builds everything first; run directly when the
build and the Python environment are already there.

A scenario is a probe plus whatever it needs around it: a fixture tree, one or
more servers, and environment. Probes are plain scripts that print `PASS`/`FAIL`
lines and exit non-zero when something failed, so they can be run by hand against
a server that is already up:

    PROBE_URL=http://127.0.0.1:8096 PROBE_ROOT=/tmp/trashtest \
      python3 probes/probe_trash.py
"""

import argparse
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.environ.get("E2E_REPO") or os.path.abspath(os.path.join(HERE, "..", ".."))
WORK = os.environ.get("E2E_WORK") or "/tmp/web-desktop-e2e"
DIST = os.environ.get("E2E_DIST") or os.path.join(REPO, "crates/client/dist")
APPS = os.environ.get("E2E_APPS") or os.path.join(REPO, "apps")
SERVER = os.path.join(REPO, "target/release/server")
PYTHON = os.environ.get("E2E_PYTHON") or sys.executable

# the shell's server, serving the built client and the built apps
WEB = {"kind": "web"}
# one static server over the same dist, with no /api behind it at all
OFFLINE = {"kind": "python", "script": "serve/offline_serve.py", "port": 8090}
# a skewed catalog: an app the shell cannot run, and a catalog it cannot decode
SKEW = {"kind": "python", "script": "serve/skew_serve.py", "port": 8095, "args": ["unknown-app"]}
BROKEN = {"kind": "python", "script": "serve/skew_serve.py", "port": 8096, "args": ["broken"]}
# the proxy, with a stand-in third party to fetch from
PROXY = {"kind": "web", "port": 8096, "env": {
    "WEB_DESKTOP_PROXY_HOSTS": "127.0.0.1",
    "WEB_DESKTOP_PROXY_ALLOW_PRIVATE": "on",
}, "also": [{"kind": "python", "script": "serve/upstream_serve.py", "port": 8099}]}


class Scenario:
    """One probe, and the ground it needs under it."""

    def __init__(self, name, probe, fixture="none", servers=(WEB,), port=8080,
                 write=True, readonly="", env=None, network=False, minutes=6):
        self.name = name
        self.probe = probe
        self.fixture = fixture
        self.servers = list(servers)
        self.port = port
        self.write = write
        self.readonly = readonly
        self.env = env or {}
        self.network = network
        self.timeout = minutes * 60


SCENARIOS = [
    Scenario("shell", "probe.py"),
    Scenario("files", "probe_fs.py", fixture="root"),
    Scenario("images", "probe_img.py", fixture="root"),
    Scenario("editing", "probe_edit.py", fixture="root"),
    Scenario("editor", "probe_editor.py", fixture="root"),
    Scenario("gallery", "probe_gallery.py", fixture="root"),
    Scenario("openwith", "probe_openwith.py", fixture="root"),
    Scenario("served", "probe_served.py", fixture="root"),
    Scenario("compression", "probe_compression.py"),
    Scenario("paint", "probe_paint.py"),
    Scenario("terminal", "probe_term.py"),
    Scenario("offline", "probe_offline.py", servers=(OFFLINE,), port=8090),
    Scenario("skew", "probe_skew.py", servers=(SKEW,), port=8095),
    Scenario("broken", "probe_broken.py", servers=(BROKEN,), port=8096),
    Scenario("proxy", "probe_proxy.py", servers=(PROXY,), port=8096),
    Scenario("trash", "probe_trash.py", fixture="trash", port=8096, readonly="Lumina"),
    Scenario("readonly", "probe_readonly.py", fixture="readonly", port=8098, write=False),
    # the same shell twice: once with the proxy allowed to reach pokeapi.co, once
    # with no proxy at all, to show what the app says when it cannot fetch
    Scenario("pokedex", "probe_pokedex.py", port=8094, network=True, servers=({
        "kind": "web", "port": 8094, "env": {"WEB_DESKTOP_PROXY_HOSTS": "pokeapi.co"},
    }, {
        "kind": "web", "port": 8091, "env": {"WEB_DESKTOP_PROXY_HOSTS": ""},
    })),
]


def log(message):
    print(message, flush=True)


def fixture_dir(scenario):
    """Which tree to build. A web server needs *some* root, even when the probe
    only looks at the shell, so `none` still gets one."""
    return scenario.fixture if scenario.fixture != "none" else "root"


def servers_for(scenario):
    """Every server a scenario needs, with the fixture root and port filled in."""
    out = []
    for spec in scenario.servers:
        spec = dict(spec)
        for extra in spec.pop("also", []):
            out.append(extra)
        out.append(spec)
    for spec in out:
        spec.setdefault("port", scenario.port)
    return out


def server_env(scenario, spec):
    env = dict(os.environ)
    env.update({
        "E2E_REPO": REPO,
        "E2E_WORK": WORK,
        "E2E_DIST": DIST,
        "WEB_DESKTOP_DIST": DIST,
        "WEB_DESKTOP_ADDR": f"127.0.0.1:{spec.get('port', scenario.port)}",
    })
    if spec.get("kind", "web") == "web":
        env["WEB_DESKTOP_ROOT"] = os.path.join(WORK, fixture_dir(scenario))
        env["WEB_DESKTOP_APPS"] = APPS
        env["WEB_DESKTOP_FS"] = "on"
        env["WEB_DESKTOP_FS_WRITE"] = "on" if scenario.write else "off"
        if scenario.readonly:
            env["WEB_DESKTOP_FS_READONLY"] = scenario.readonly
    env.update(spec.get("env", {}))
    return env


def start_server(scenario, spec, logfile):
    if spec.get("kind", "web") == "python":
        cmd = [PYTHON, os.path.join(HERE, spec["script"]), str(spec.get("port", scenario.port)),
               *spec.get("args", [])]
    else:
        if not os.path.exists(SERVER):
            sys.exit(f"{SERVER} is missing: run `cargo xtask e2e` (or `cargo build --release -p server`)")
        cmd = [SERVER]
    fh = open(logfile, "ab")
    return subprocess.Popen(cmd, env=server_env(scenario, spec), stdout=fh, stderr=subprocess.STDOUT,
                            cwd=WORK, start_new_session=True)


def wait_for(port, timeout=40):
    """Wait for a server to answer, so a probe never races its own backend."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/", timeout=2) as r:
                if r.status < 500:
                    return True
        except urllib.error.HTTPError:
            # answering at all is enough: the stand-in upstream has no index
            return True
        except (urllib.error.URLError, OSError):
            time.sleep(0.4)
    return False


def stop(processes):
    for proc in processes:
        if proc.poll() is not None:
            continue
        os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
    for proc in processes:
        try:
            proc.wait(timeout=8)
        except subprocess.TimeoutExpired:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)


def probe_env(scenario, root):
    env = dict(os.environ)
    env.update({
        "E2E_REPO": REPO,
        "E2E_WORK": WORK,
        "E2E_DIST": DIST,
        # the probe corrupts a copy of one app's version directory
        "E2E_APPS": os.path.join(APPS, "calculator"),
        "PROBE_URL": f"http://127.0.0.1:{scenario.port}/",
        "PROBE_ROOT": root,
        # the pokedex probe serves the same shell twice: with the proxy on, and off
        "PROXIED": "http://127.0.0.1:8094/",
        "PLAIN": "http://127.0.0.1:8091/",
    })
    env.update(scenario.env)
    return env


def network_up(host="pokeapi.co", port=443, timeout=6):
    try:
        socket.create_connection((host, port), timeout=timeout).close()
        return True
    except OSError:
        return False


def run_scenario(scenario, keep):
    logs = os.path.join(WORK, "logs")
    os.makedirs(logs, exist_ok=True)
    logfile = os.path.join(logs, f"{scenario.name}.log")

    root = subprocess.run([PYTHON, os.path.join(HERE, "fixtures.py"), fixture_dir(scenario)],
                          capture_output=True, text=True, check=True).stdout.strip()

    processes = []
    try:
        for spec in servers_for(scenario):
            processes.append(start_server(scenario, spec, logfile))
        for spec in servers_for(scenario):
            if not wait_for(spec.get("port", scenario.port)):
                return {"name": scenario.name, "status": "server did not start",
                        "checks": 0, "failed": 0, "output": ""}

        started = time.time()
        with open(logfile, "ab") as fh:
            fh.write(f"\n=== {scenario.probe} {time.strftime('%H:%M:%S')}\n".encode())
        proc = subprocess.run([PYTHON, os.path.join(HERE, "probes", scenario.probe)],
                              env=probe_env(scenario, root), cwd=REPO, capture_output=True,
                              text=True, timeout=scenario.timeout)
        output = proc.stdout + proc.stderr
        with open(logfile, "ab") as fh:
            fh.write(output.encode())
        passed = output.count("\nPASS") + (1 if output.startswith("PASS") else 0)
        failed = output.count("\nFAIL") + (1 if output.startswith("FAIL") else 0)
        status = "ok" if proc.returncode == 0 and not failed else "failed"
        if "Traceback (most recent call last)" in output:
            # a probe that dies half-way must not look like a pass
            status = "crashed"
        return {"name": scenario.name, "status": status, "checks": passed + failed,
                "failed": failed, "output": output, "seconds": time.time() - started}
    finally:
        stop(processes)
        if not keep:
            shutil.rmtree(os.path.join(WORK, "root"), ignore_errors=True)
            shutil.rmtree(os.path.join(WORK, "trash"), ignore_errors=True)
            shutil.rmtree(os.path.join(WORK, "readonly"), ignore_errors=True)
        # the served-app probe copies a version to corrupt it, and cleans up after
        # itself unless it crashed
        for spare in ("0.1.1", "0.1.2"):
            shutil.rmtree(os.path.join(APPS, "calculator", spare), ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--only", help="comma separated scenario names")
    parser.add_argument("--list", action="store_true", help="list the scenarios and exit")
    parser.add_argument("--keep", action="store_true", help="keep fixtures, screenshots and logs")
    parser.add_argument("--skip-network", action="store_true", help="skip the probes that need the internet")
    parser.add_argument("--fail-fast", action="store_true", help="stop at the first failure")
    args = parser.parse_args()

    scenarios = SCENARIOS
    if args.only:
        wanted = [n.strip() for n in args.only.split(",") if n.strip()]
        by_name = {s.name: s for s in SCENARIOS}
        missing = [n for n in wanted if n not in by_name]
        if missing:
            sys.exit(f"unknown scenario(s): {', '.join(missing)}")
        scenarios = [by_name[n] for n in wanted]

    if args.list:
        for s in SCENARIOS:
            log(f"  {s.name:12} probes/{s.probe}")
        return 0

    os.makedirs(WORK, exist_ok=True)
    results = []
    for scenario in scenarios:
        log(f"\n=== {scenario.name}: {scenario.probe}")
        if scenario.network and (args.skip_network or not network_up()):
            log("    skipped: needs the internet (pokeapi.co), and --skip-network is in effect")
            results.append({"name": scenario.name, "status": "skipped", "checks": 0, "failed": 0,
                            "output": ""})
            continue
        result = run_scenario(scenario, args.keep)
        results.append(result)
        for line in result["output"].splitlines():
            if line.startswith("FAIL") or line.startswith("PASS"):
                log("    " + line)
        log(f"    {result['status']}: {result['checks']} checks, {result['failed']} failed")

        if result["status"] in ("failed", "crashed", "server did not start"):
            for line in result["output"].splitlines():
                if "Traceback" in line or line.strip().startswith("File \"") or "Error" in line:
                    log("    " + line)
            if args.fail_fast:
                break

    bad = [r for r in results if r["status"] in ("failed", "crashed", "server did not start")]
    skipped = [r for r in results if r["status"] == "skipped"]
    checks = sum(r["checks"] for r in results)
    log("\n" + "=" * 68)
    for r in results:
        name = r["name"]
        mark = {"ok": "ok", "skipped": "skipped"}.get(r["status"], r["status"].upper())
        log(f"  {name:12} {mark:8} {r['checks']:4} checks"
            + (f", {r['failed']} failed" if r["failed"] else ""))
    log(f"  {'total':12} {checks} checks in {len(results)} scenarios"
        + (f", {len(skipped)} skipped" if skipped else ""))
    log(f"  logs and screenshots: {WORK}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
