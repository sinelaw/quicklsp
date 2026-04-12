#!/usr/bin/env python3
"""Drive quicklsp LSP on a SECOND clone with a WARM Layer A cache.

Measures time-to-first-useful-query: how long after `initialized` does
workspace/symbol start returning results? With cache v3, this is
bounded by stat + Layer A reads, not parsing.
"""

import json
import os
import subprocess
import threading
import time

BIN = "/home/user/quicklsp/target/release/quicklsp"
SECOND_CLONE = "/tmp/quicklsp-field-test/ripgrep-clone"
CACHE = "/tmp/qlsp-demo-cache-lsp"

env = os.environ.copy()
env["QUICKLSP_CACHE_DIR"] = CACHE
env["RUST_LOG"] = "warn"

err_log = open("/tmp/quicklsp-field-test/lsp_stderr.log", "wb")
p = subprocess.Popen([BIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                     stderr=err_log, env=env, bufsize=0)

next_id = 1
pending = {}       # id -> threading.Event with attached ._result
pending_lock = threading.Lock()
server_notifications = []


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


def reader_loop():
    """Consume server messages. Reply to server-initiated requests (like
    window/workDoneProgress/create) with a success result so the server's
    .await.is_ok() path completes and the scan can proceed."""
    while True:
        m = read_msg()
        if m is None:
            return
        if "method" in m and "id" in m:
            # Server request — reply with success.
            write_msg({"jsonrpc": "2.0", "id": m["id"], "result": None})
        elif "id" in m and ("result" in m or "error" in m):
            with pending_lock:
                evt = pending.get(m["id"])
                if evt is not None:
                    evt._result = m
                    evt.set()


reader = threading.Thread(target=reader_loop, daemon=True)
reader.start()


def send_request(method, params, timeout=15.0):
    global next_id
    rid = next_id
    next_id += 1
    evt = threading.Event()
    with pending_lock:
        pending[rid] = evt
    write_msg({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
    if not evt.wait(timeout=timeout):
        return None
    return evt._result


def send_notification(method, params):
    write_msg({"jsonrpc": "2.0", "method": method, "params": params})


def uri(path):
    return "file://" + os.path.abspath(path)


t0 = time.time()
init_resp = send_request("initialize", {
    "processId": os.getpid(),
    "rootUri": uri(SECOND_CLONE),
    "capabilities": {
        "window": {"workDoneProgress": True},
    },
})
t_init = time.time() - t0

send_notification("initialized", {})

# Poll workspace/symbol until results appear.
result_hits = 0
t_first = None
deadline = time.time() + 20.0
while time.time() < deadline:
    resp = send_request("workspace/symbol", {"query": "Searcher"}, timeout=5.0)
    hits = (resp or {}).get("result") or []
    if hits:
        result_hits = len(hits)
        t_first = time.time() - t0
        break
    time.sleep(0.05)

print(f"initialize:                   {t_init*1000:.0f} ms", flush=True)
if t_first is not None:
    print(f"first useful workspace query: {t_first*1000:.0f} ms  ({result_hits} hits)", flush=True)
else:
    print("scan never produced results within 20s — failure", flush=True)

# Probe go-to-definition on a Searcher call site.
searcher_file = f"{SECOND_CLONE}/crates/searcher/src/searcher/mod.rs"
with open(searcher_file) as f:
    src = f.read()
send_notification("textDocument/didOpen", {
    "textDocument": {
        "uri": uri(searcher_file), "languageId": "rust", "version": 1, "text": src,
    }
})
time.sleep(0.1)

for i, line in enumerate(src.splitlines()):
    if line.strip().startswith("pub struct Searcher "):
        col = line.index("Searcher")
        break

t1 = time.time()
resp = send_request("textDocument/references", {
    "textDocument": {"uri": uri(searcher_file)},
    "position": {"line": i, "character": col},
    "context": {"includeDeclaration": True},
})
q_ms = (time.time() - t1) * 1000
refs = (resp or {}).get("result") or []
print(f"find-references latency:      {q_ms:.0f} ms  ({len(refs)} refs)", flush=True)

p.kill()
p.wait(timeout=2)
