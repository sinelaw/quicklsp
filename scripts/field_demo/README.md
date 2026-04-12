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

## 4. End-user experience: Neovim driving QuickLSP

`nvim_config/init.lua` is a recommended-UX Neovim config (nvim-lspconfig +
nvim-cmp + LuaSnip, optional Telescope on Nvim 0.11+). It registers
QuickLSP as a custom server for all its supported filetypes and wires up
the familiar `gd / gD / gr / gi / K / <leader>ws / <leader>rn` keymap.

Run headless against ripgrep to measure cold vs warm first-query latency
as observed by the real Neovim LSP client:

```bash
./scripts/field_demo/run_nvim_bench.sh
```

Observed (Nvim 0.10.4, ripgrep ~100 Rust files):

```
[cold  (empty cache)   ] attach=194 ms  ready=2215 ms  refs=19 ms (100 refs)
[warm  (Layer A hot)   ] attach=210 ms  ready=478 ms   refs=27 ms
[rescan(stat-fresh)    ] attach=196 ms  ready=433 ms   refs=43 ms
```

`nvim_demo.lua` exercises definition / references / workspace-symbol on a
real file; `nvim_probe.lua` + `nvim_probe2.lua` enumerate server
capabilities and exercise each common operation.

## 5. Notable behaviors vs mainstream LSP servers

Observations from driving QuickLSP inside Neovim on ripgrep, compared to
what users expect from `rust-analyzer`, `gopls`, or `pyright`:

| Operation | QuickLSP | Typical mainstream server |
|---|---|---|
| Go-to-definition | ~1–2 ms, precise | 5–50 ms, precise (needs type info) |
| Find references (100-file repo) | 20–40 ms, 100 refs | 50–500 ms, semantically filtered |
| Hover | Code fence with signature line | Rich markdown + types + docs |
| Document symbol | ~2 ms, full outline | Similar |
| Workspace symbol | Edit-distance fuzzy match | Substring match |
| Completion | Works on any prefix, no types | Type-aware, contextual |
| Diagnostics | **None** | Errors, warnings, unused lints |
| Rename / code-action | Not provided | Provided |
| Semantic tokens / inlay hints / call hierarchy | Not provided | Provided |

**Rough edges a user will notice:**

1. **No diagnostics at all.** No red squigglies, no warnings, no unused-variable nags. The editor feels "dead" compared to `rust-analyzer`. This is by design — the tokenizer has no type system.

2. **Workspace symbol is edit-distance-fuzzy, not substring.** Typing `Builder` in `<leader>ws` returns the 2 definitions literally named "Builder", not the 21 `*Builder` structs in ripgrep. Users habitually type short substrings to discover related types and will be surprised. A mitigation is to type near-exact names.

3. **Hover is sparse.** Returns a code fence containing the matched signature line. No doc-comment rendering, no resolved types. Good enough for "what is this thing?" questions, not for "show me the API".

4. **References include comment mentions.** Of 100 refs for `Searcher` in ripgrep, 8 fell on comment lines. Word-boundary regex can't distinguish code from comments. Not wrong, but different from semantic servers.

5. **Unsupported operations are fast-failing, not silent.** `rename`, `code-action`, `formatting`, `semanticTokens`, `inlayHint`, `typeDefinition`, `implementation`, `callHierarchy` — all absent from `serverCapabilities`. Neovim's client issues a `"method ... is not supported"` warning instead of sending the request. Keystrokes like `<leader>rn` produce a toast, not a stall — but also produce no action.

6. **Strict spec compliance on `signatureHelpContext`.** The server's serde deserialisation requires LSP 3.16's mandatory `isRetrigger` field. Nvim 0.10+ sends it; hand-crafted clients may see `missing field 'isRetrigger'` errors.

7. **Workspace symbol result set grows asynchronously.** For ~2 seconds after `initialized`, `workspace/symbol` returns only the currently-open buffer's symbols; once the background scan matures, the full repo's symbols appear. This matches rust-analyzer's async indexing in principle but the transition is faster and less visible (no built-in status indicator).

8. **Progress notifications work, but Nvim's default UI doesn't render them.** Quicklsp emits `$/progress` events during indexing; plugins like `fidget.nvim` show a progress bar, a vanilla config stays quiet.

9. **Cache-v3 behavior is transparent.** On a second clone (or a new `git worktree`, or a restart), "first useful query" drops from ~2.2 s to ~450 ms inside Neovim — that's the headline user-visible property. No cache-management commands are needed; the cache at `$XDG_CACHE_HOME/quicklsp/` just works.

### Setting the expectation

Think of QuickLSP as a **fast, language-agnostic structural server**: it shines at navigation (definition, references, workspace symbols, quick hover) across 8 language families with near-zero config, and its cache makes branch/worktree switching essentially free. It is not a replacement for a type-aware server when you need diagnostics, rename-refactor, or rich hover docs.
