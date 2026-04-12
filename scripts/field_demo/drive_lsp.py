#!/usr/bin/env python3
"""Drive quicklsp over stdio JSON-RPC against the ripgrep repo."""

import json
import os
import subprocess
import sys
import time

BIN = "/home/user/quicklsp/target/release/quicklsp"
REPO = "/tmp/quicklsp-field-test/ripgrep"
CACHE = os.environ.get("QUICKLSP_CACHE_DIR", "/tmp/qlsp-demo-cache-lsp")

env = os.environ.copy()
env["QUICKLSP_CACHE_DIR"] = CACHE
env.setdefault("RUST_LOG", "warn")

p = subprocess.Popen(
    [BIN],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    env=env,
    bufsize=0,
)

seq = 0


def send(method, params, is_notification=False):
    global seq
    msg = {"jsonrpc": "2.0", "method": method, "params": params}
    if not is_notification:
        seq += 1
        msg["id"] = seq
    body = json.dumps(msg).encode()
    header = f"Content-Length: {len(body)}\r\n\r\n".encode()
    p.stdin.write(header + body)
    p.stdin.flush()


def recv():
    # Read Content-Length header then body.
    header = b""
    while not header.endswith(b"\r\n\r\n"):
        ch = p.stdout.read(1)
        if not ch:
            return None
        header += ch
    length = 0
    for line in header.decode().split("\r\n"):
        if line.lower().startswith("content-length:"):
            length = int(line.split(":", 1)[1].strip())
    body = p.stdout.read(length)
    return json.loads(body)


def wait_for_response(want_id):
    """Drain notifications/other responses until we see the one we want."""
    while True:
        msg = recv()
        if msg is None:
            return None
        if msg.get("id") == want_id and ("result" in msg or "error" in msg):
            return msg


def uri(path):
    return "file://" + os.path.abspath(path)


# --- initialize ---
t0 = time.time()
send("initialize", {
    "processId": os.getpid(),
    "rootUri": uri(REPO),
    "capabilities": {},
})
init_id = seq
init_resp = wait_for_response(init_id)
send("initialized", {}, is_notification=True)

# Give the scan time to complete (the server indexes on initialize).
# Poll by pinging with a fast request.
time.sleep(2.5)  # rough sync — ripgrep cold takes ~2s
elapsed_init = time.time() - t0
print(f"initialize + scan: {elapsed_init:.2f}s")

# --- textDocument/definition on "Searcher" in crates/searcher/src/searcher/mod.rs ---
# Line 597: `pub struct Searcher {` → cursor on the word "Searcher" at col ~12
searcher_file = f"{REPO}/crates/searcher/src/searcher/mod.rs"
with open(searcher_file) as f:
    searcher_src = f.read()
send(
    "textDocument/didOpen",
    {
        "textDocument": {
            "uri": uri(searcher_file),
            "languageId": "rust",
            "version": 1,
            "text": searcher_src,
        }
    },
    is_notification=True,
)
time.sleep(0.3)

# Find a usage of `SearcherBuilder` inside the file to test go-to-def.
for lineno, line in enumerate(searcher_src.splitlines()):
    if "SearcherBuilder::new" in line and lineno > 20:
        col = line.index("SearcherBuilder") + 3  # cursor inside the word
        break
else:
    raise RuntimeError("SearcherBuilder call not found")

print(f"requesting definition of SearcherBuilder at mod.rs:{lineno}:{col}")
send(
    "textDocument/definition",
    {
        "textDocument": {"uri": uri(searcher_file)},
        "position": {"line": lineno, "character": col},
    },
)
resp = wait_for_response(seq)
result = resp.get("result")
if isinstance(result, list) and result:
    r = result[0]
    print("  → definition:", r["uri"].replace("file://", ""),
          f"line {r['range']['start']['line']+1}")
else:
    print("  → no result:", result)

# --- textDocument/references on `Searcher` ---
# Line 597 is `pub struct Searcher {` — point at "Searcher"
for lineno, line in enumerate(searcher_src.splitlines()):
    if line.startswith("pub struct Searcher "):
        col = line.index("Searcher")
        break
print(f"requesting references of Searcher at mod.rs:{lineno}:{col}")
send(
    "textDocument/references",
    {
        "textDocument": {"uri": uri(searcher_file)},
        "position": {"line": lineno, "character": col},
        "context": {"includeDeclaration": True},
    },
)
resp = wait_for_response(seq)
refs = resp.get("result") or []
print(f"  → {len(refs)} references across ripgrep")
for r in refs[:5]:
    print("     ", r["uri"].replace("file://", "").replace(REPO + "/", ""),
          f"line {r['range']['start']['line']+1}")
if len(refs) > 5:
    print(f"      … ({len(refs) - 5} more)")

# --- workspace/symbol search ---
send("workspace/symbol", {"query": "SearcherBuilder"})
resp = wait_for_response(seq)
syms = resp.get("result") or []
print(f"workspace/symbol 'SearcherBuilder' → {len(syms)} hits")
for s in syms[:3]:
    loc = s.get("location", {})
    print("     ", loc.get("uri", "").replace("file://", "").replace(REPO + "/", ""),
          "-", s.get("name"))

# --- shutdown ---
send("shutdown", None)
wait_for_response(seq)
send("exit", None, is_notification=True)
p.wait(timeout=5)
