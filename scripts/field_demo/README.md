# Field demo: cache v3 on a real repo

Exercises the cache v3 design (see `docs/cache-v3-design.md`) against
ripgrep as a real-world Rust codebase. Combines a direct CLI driver
that reports counter snapshots with LSP-protocol drivers that measure
end-user latency.

## Prereqs

```bash
cargo build --release --bin quicklsp --bin quicklsp-cache-demo
git clone --depth=50 https://github.com/BurntSushi/ripgrep.git /tmp/ripgrep
```

## 1. Counter-level scenarios via the CLI driver

```bash
export QUICKLSP_CACHE_DIR=/tmp/qlsp-demo
rm -rf "$QUICKLSP_CACHE_DIR"

# Cold scan — every file is parsed and written to Layer A.
./target/release/quicklsp-cache-demo cold /tmp/ripgrep

# Rescan — stat-fresh, zero bytes read, zero parses, 100% Layer A hits.
./target/release/quicklsp-cache-demo rescan /tmp/ripgrep

# Clone reuse — Layer A hits for every file in the copy.
cp -r /tmp/ripgrep /tmp/ripgrep-clone
./target/release/quicklsp-cache-demo clone /tmp/ripgrep /tmp/ripgrep-clone

# Parent subsumption — rows copied from a previously-indexed subtree.
./target/release/quicklsp-cache-demo subsume-parent /tmp/ripgrep /tmp/ripgrep/crates/searcher

# Branch-switch simulation — edit ~5% of files, observe parses scale.
./target/release/quicklsp-cache-demo edit /tmp/ripgrep-clone
```

Each run prints:

```
wall_ms              : ...
files_stat_called    : ...
files_bytes_read     : ...
files_blake3_hashed  : ...
files_parsed         : ...
layer_a_hits         : ...
layer_a_writes       : ...
manifest_rows_copied : ...
reparse_ratio        : ...   hash_savings: ...   layer_a_hit_rate: ...
```

## 2. End-user latency through the LSP protocol

`bench_lsp.py` drives the actual `quicklsp` binary over stdio JSON-RPC
as an editor would, and measures time from `initialize` to first
useful `workspace/symbol` result.

```bash
python3 scripts/field_demo/bench_lsp.py
```

Typical output on ripgrep (~100 Rust files, ~1.7 MiB):

```
[A cold   (empty cache)]  init=23ms  first-query=2147ms  (6 hits)
[B warm   (Layer A hit)]  init=24ms  first-query=237ms   (6 hits)
[A rescan (stat-fresh)]   init=22ms  first-query=215ms   (6 hits)
```

The ~9× speedup between cold and warm is the end-user property the
design promises: opening a fresh clone (or a new git worktree) of a
repo you've worked on before is bounded by stat + Layer A reads, not
by parsing.

## 3. Rich LSP interaction

`drive_lsp.py` issues `textDocument/definition`,
`textDocument/references`, and `workspace/symbol` against a real
ripgrep source file and prints the results:

```bash
python3 scripts/field_demo/drive_lsp.py
```
