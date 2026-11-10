"""Run the browser suite: bring up a server and its fixtures, run one probe, tear down.

Usage:
    python3 run.py                 # every scenario, four at a time
    python3 run.py --jobs 8        # ... or some other level of parallelism
    python3 run.py --list          # what there is
    python3 run.py --only trash    # one or more, comma separated
    python3 run.py --keep          # leave fixtures, screenshots and logs in $E2E_WORK

Driven by `cargo xtask e2e`, which builds everything first; run directly when the
build and the Python environment are already there.

A scenario is a probe plus whatever it needs around it: a fixture tree, one or
more servers, and environment. Scenarios are independent by construction — each
gets its own ports (handed out at 8200 and up), its own slice of $E2E_WORK for
fixtures and screenshots, and its own browser — so several run at once, four by
default. Probes are plain scripts that print `PASS`/`FAIL` lines and exit non-zero
when something failed, so they can be run by hand against a server that is
already up:

    PROBE_URL=http://127.0.0.1:8096 PROBE_ROOT=/tmp/trashtest \
      python3 probes/probe_trash.py
"""

import argparse
import concurrent.futures
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

# a run is over when no scenario is failed, crashed, or never got its server up
BAD = ("failed", "crashed", "server did not start")

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
}, "also": [{"kind": "python", "script": "serve/upstream_serve.py", "port": 8099,
             "role": "upstream"}]}


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
        # filled in by assign_ports: the flattened server specs, ports included
        self.specs = []


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
        "kind": "web", "port": 8094, "role": "proxied",
        "env": {"WEB_DESKTOP_PROXY_HOSTS": "pokeapi.co"},
    }, {
        "kind": "web", "port": 8091, "role": "plain", "env": {"WEB_DESKTOP_PROXY_HOSTS": ""},
    })),
]


def log(message):
    print(message, flush=True)


def fixture_dir(scenario):
    """Which tree to build. A web server needs *some* root, even when the probe
    only looks at the shell, so `none` still gets one."""
    return scenario.fixture if scenario.fixture != "none" else "root"


def servers_for(scenario):
    """Every server a scenario needs, with the port filled in."""
    out = []
    for spec in scenario.servers:
        spec = dict(spec)
        for extra in spec.pop("also", []):
            out.append(extra)
        out.append(spec)
    for spec in out:
        spec.setdefault("port", scenario.port)
    return out


def assign_ports(scenarios, base=8200):
    """Hand every scenario ports that are unique across the run, so scenarios
    can run at the same time.

    The declared ports deliberately collide (most scenarios want 8080); every
    probe learns its URL from PROBE_URL, so only uniqueness within a run matters.
    The server a probe talks to keeps the scenario's port; side servers (the
    pokedex's plain shell, the proxy's upstream) each get their own."""
    port = base
    for scenario in scenarios:
        specs = servers_for(scenario)
        main = port
        port += 1
        for spec in specs:
            if spec["port"] == scenario.port:
                spec["port"] = main
            else:
                spec["port"] = port
                port += 1
        scenario.port = main
        scenario.specs = specs


def server_env(scenario, spec, root, work):
    env = dict(os.environ)
    env.update({
        "E2E_REPO": REPO,
        "E2E_WORK": work,
        "E2E_DIST": DIST,
        "WEB_DESKTOP_DIST": DIST,
        "WEB_DESKTOP_ADDR": f"127.0.0.1:{spec['port']}",
    })
    if spec.get("kind", "web") == "web":
        env["WEB_DESKTOP_ROOT"] = root
        env["WEB_DESKTOP_APPS"] = APPS
        env["WEB_DESKTOP_FS"] = "on"
        env["WEB_DESKTOP_FS_WRITE"] = "on" if scenario.write else "off"
        if scenario.readonly:
            env["WEB_DESKTOP_FS_READONLY"] = scenario.readonly
    env.update(spec.get("env", {}))
    return env


def start_server(scenario, spec, logfile, root, work):
    if spec.get("kind", "web") == "python":
        cmd = [PYTHON, os.path.join(HERE, spec["script"]), str(spec["port"]),
               *spec.get("args", [])]
    else:
        if not os.path.exists(SERVER):
            sys.exit(f"{SERVER} is missing: run `cargo xtask e2e` (or `cargo build --release -p server`)")
        cmd = [SERVER]
    fh = open(logfile, "ab")
    return subprocess.Popen(cmd, env=server_env(scenario, spec, root, work), stdout=fh, stderr=subprocess.STDOUT,
                            cwd=work, start_new_session=True)


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


def probe_env(scenario, root, work):
    env = dict(os.environ)
    env.update({
        "E2E_REPO": REPO,
        "E2E_WORK": work,
        "E2E_DIST": DIST,
        # the probe corrupts a copy of one app's version directory
        "E2E_APPS": os.path.join(APPS, "calculator"),
        "PROBE_URL": f"http://127.0.0.1:{scenario.port}/",
        "PROBE_ROOT": root,
    })
    # a probe with side servers learns where they are by role
    for spec in scenario.specs:
        if spec.get("role") == "proxied":
            env["PROXIED"] = f"http://127.0.0.1:{spec['port']}/"
        elif spec.get("role") == "plain":
            env["PLAIN"] = f"http://127.0.0.1:{spec['port']}/"
        elif spec.get("role") == "upstream":
            env["UPSTREAM"] = f"http://127.0.0.1:{spec['port']}"
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

    # a slice of the work dir per scenario, so scenarios running at the same
    # time never rebuild or delete each other's fixtures and screenshots
    work = os.path.join(WORK, scenario.name)
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work)
    root = subprocess.run([PYTHON, os.path.join(HERE, "fixtures.py"), fixture_dir(scenario)],
                          capture_output=True, text=True, check=True,
                          env=dict(os.environ, E2E_WORK=work)).stdout.strip()

    processes = []
    try:
        for spec in scenario.specs:
            processes.append(start_server(scenario, spec, logfile, root, work))
        for spec in scenario.specs:
            if not wait_for(spec["port"]):
                return {"name": scenario.name, "status": "server did not start",
                        "checks": 0, "failed": 0, "output": ""}

        started = time.time()
        with open(logfile, "ab") as fh:
            fh.write(f"\n=== {scenario.probe} {time.strftime('%H:%M:%S')}\n".encode())
        proc = subprocess.run([PYTHON, os.path.join(HERE, "probes", scenario.probe)],
                              env=probe_env(scenario, root, work), cwd=REPO, capture_output=True,
                              text=True, timeout=scenario.timeout)
        output = proc.stdout + proc.stderr
        with open(logfile, "ab") as fh:
            fh.write(output.encode())
        # every probe prints its checks as "PASS  " / "FAIL  " lines (two
        # spaces); counting prefixes rather than substrings keeps a probe's
        # own trailing "FAILED: <name>" summary from counting twice
        lines = output.splitlines()
        passed = sum(1 for line in lines if line.startswith("PASS  "))
        failed = sum(1 for line in lines if line.startswith("FAIL  "))
        status = "ok" if proc.returncode == 0 and not failed else "failed"
        if "Traceback (most recent call last)" in output:
            # a probe that dies half-way must not look like a pass
            status = "crashed"
        return {"name": scenario.name, "status": status, "checks": passed + failed,
                "failed": failed, "output": output, "seconds": time.time() - started}
    finally:
        stop(processes)
        if not keep:
            shutil.rmtree(work, ignore_errors=True)
        # the served-app probe copies a version to corrupt it, and cleans up
        # after itself unless it crashed; only its own teardown may do that,
        # so a scenario finishing alongside it cannot delete the copies
        # mid-flight
        if scenario.probe == "probe_served.py":
            for spare in ("0.1.1", "0.1.2"):
                shutil.rmtree(os.path.join(APPS, "calculator", spare), ignore_errors=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--only", help="comma separated scenario names")
    parser.add_argument("--list", action="store_true", help="list the scenarios and exit")
    parser.add_argument("--keep", action="store_true", help="keep fixtures, screenshots and logs")
    parser.add_argument("--skip-network", action="store_true", help="skip the probes that need the internet")
    parser.add_argument("--fail-fast", action="store_true",
                        help="stop at the first failure (with --jobs > 1, scenarios already running still finish)")
    parser.add_argument("--jobs", type=int, default=4, metavar="N",
                        help="scenarios to run at once (default: 4)")
    parser.add_argument("--port-base", type=int, default=8200, metavar="PORT",
                        help="hand ports out from here (default: 8200), so a second "
                             "run of the suite alongside this one cannot collide")
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

    assign_ports(scenarios, args.port_base)
    jobs = max(1, min(args.jobs, len(scenarios)))
    results = {}

    def attempt(scenario):
        """One scenario, as a unit of work: skip it, run it, or report that the
        driver itself died — which must not look like a pass."""
        if scenario.network and (args.skip_network or not network_up()):
            return {"name": scenario.name, "status": "skipped", "checks": 0, "failed": 0,
                    "output": ""}
        try:
            return run_scenario(scenario, args.keep)
        except Exception as err:
            return {"name": scenario.name, "status": "crashed", "checks": 0, "failed": 0,
                    "output": f"Traceback (most recent call last)\ndriver error: {err}"}

    def details(result):
        """A scenario's lines, after it has run."""
        for line in result["output"].splitlines():
            if line.startswith("FAIL") or line.startswith("PASS"):
                log("    " + line)
        seconds = result.get("seconds")
        log(f"    {result['status']}: {result['checks']} checks, {result['failed']} failed"
            + (f" in {seconds:.0f}s" if seconds is not None else ""))
        if result["status"] in BAD:
            for line in result["output"].splitlines():
                if "Traceback" in line or line.strip().startswith("File \"") or "Error" in line:
                    log("    " + line)

    began = time.time()
    if jobs == 1:
        for scenario in scenarios:
            log(f"\n=== {scenario.name}: {scenario.probe}")
            result = attempt(scenario)
            results[scenario.name] = result
            details(result)
            if args.fail_fast and result["status"] in BAD:
                break
    else:
        log(f"running {jobs} scenarios at a time")
        with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
            futures = {pool.submit(attempt, s): s for s in scenarios}
            for future in concurrent.futures.as_completed(futures):
                result = future.result()
                results[result["name"]] = result
                # as a block: with scenarios running at the same time, one's
                # lines must not be scattered among the others'
                log(f"\n=== {result['name']}: {futures[future].probe}")
                details(result)
                if args.fail_fast and result["status"] in BAD:
                    # stop launching new scenarios; the running ones still finish
                    for pending in futures:
                        pending.cancel()
                    break
            # whatever finished while we were breaking out still gets reported
            for future, scenario in futures.items():
                if future.cancelled():
                    results[scenario.name] = {"name": scenario.name, "status": "skipped",
                                              "checks": 0, "failed": 0, "output": ""}
                elif scenario.name not in results:
                    result = future.result()
                    results[scenario.name] = result
                    log(f"\n=== {result['name']}: {scenario.probe}")
                    details(result)

    ordered = [results[s.name] for s in scenarios if s.name in results]
    bad = [r for r in ordered if r["status"] in BAD]
    skipped = [r for r in ordered if r["status"] == "skipped"]
    checks = sum(r["checks"] for r in ordered)
    log("\n" + "=" * 68)
    for r in ordered:
        name = r["name"]
        mark = {"ok": "ok", "skipped": "skipped"}.get(r["status"], r["status"].upper())
        log(f"  {name:12} {mark:8} {r['checks']:4} checks"
            + (f", {r['failed']} failed" if r["failed"] else ""))
    log(f"  {'total':12} {checks} checks in {len(ordered)} scenarios"
        + (f", {len(skipped)} skipped" if skipped else "")
        + f" ({time.time() - began:.0f}s)")
    log(f"  logs and screenshots: {WORK}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
