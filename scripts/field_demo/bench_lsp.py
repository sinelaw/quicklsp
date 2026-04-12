#!/usr/bin/env python3
"""Measure LSP initialize→first-useful-query on both cold and warm scans
of the same repo. Reports both wall-clock and counter snapshots.
"""
import json
import os
import shutil
import subprocess
import threading
import time

BIN = "/home/user/quicklsp/target/release/quicklsp"
REPO_A = "/tmp/quicklsp-field-test/ripgrep"
REPO_B = "/tmp/quicklsp-field-test/ripgrep-clone"
CACHE = "/tmp/qlsp-bench-cache"


def run_lsp(root, cache, label):
    env = os.environ.copy()
    env["QUICKLSP_CACHE_DIR"] = cache
    env["RUST_LOG"] = "warn"
    p = subprocess.Popen([BIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.DEVNULL, env=env, bufsize=0)
    next_id = [1]
    pending = {}
    lock = threading.Lock()

    def write_msg(msg):
        body = json.dumps(msg).encode()
        p.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
        p.stdin.flush()

    def read_msg():
        header = b""
        while not header.endswith(b"\r\n\r\n"):
            ch = p.stdout.read(1)
            if not ch:
                return None
            header += ch
        length = 0
        for line in header.decode().split("\r\n"):
            if line.lower().startswith("content-length:"):
                length = int(line.split(":", 1)[1])
        return json.loads(p.stdout.read(length))

    def reader():
        while True:
            m = read_msg()
            if m is None:
                return
            if "method" in m and "id" in m:
                write_msg({"jsonrpc": "2.0", "id": m["id"], "result": None})
            elif "id" in m:
                with lock:
                    ev = pending.get(m["id"])
                    if ev:
                        ev._result = m
                        ev.set()

    threading.Thread(target=reader, daemon=True).start()

    def request(method, params, timeout=20.0):
        rid = next_id[0]
        next_id[0] += 1
        ev = threading.Event()
        with lock:
            pending[rid] = ev
        write_msg({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        if not ev.wait(timeout):
            return None
        return ev._result

    def notify(method, params):
        write_msg({"jsonrpc": "2.0", "method": method, "params": params})

    t0 = time.time()
    request("initialize", {
        "processId": os.getpid(),
        "rootUri": "file://" + os.path.abspath(root),
        "capabilities": {"window": {"workDoneProgress": True}},
    })
    t_init = time.time() - t0
    notify("initialized", {})

    hits = 0
    t_first = None
    deadline = time.time() + 20
    while time.time() < deadline:
        resp = request("workspace/symbol", {"query": "Searcher"}, timeout=5)
        r = (resp or {}).get("result") or []
        if r:
            hits = len(r)
            t_first = time.time() - t0
            break
        time.sleep(0.02)

    print(f"[{label}]  init={t_init*1000:.0f}ms  first-query={'---' if t_first is None else f'{t_first*1000:.0f}ms'}  ({hits} hits)", flush=True)
    p.kill()
    p.wait(timeout=2)


# Fresh cache, cold scan of ripgrep.
shutil.rmtree(CACHE, ignore_errors=True)
run_lsp(REPO_A, CACHE, "A cold   (empty cache)")

# Same cache, second repo (different path, identical content).
run_lsp(REPO_B, CACHE, "B warm   (Layer A hit)")

# Same cache, same repo again.
run_lsp(REPO_A, CACHE, "A rescan (stat-fresh)")
