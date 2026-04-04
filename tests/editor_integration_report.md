# Editor Integration Test Report

**Date:** 2026-04-04
**Editor:** Vim 9.1 + vim-lsp plugin
**Method:** Automated tmux-driven testing against quicklsp's own repository
**Build:** Release profile (optimized)

## Test Environment

- quicklsp built with `cargo build --release`
- vim-lsp plugin (prabirshrestha/vim-lsp)
- LSP registered for `rust` filetype with workspace root `/home/user/quicklsp`
- Repository has 102 Cargo dependencies (4233 .rs files in dependency tree)

## Feature Test Results

### Go to Definition ✅ PASS

| Symbol | From File | Expected Target | Actual Target | Correct? |
|--------|-----------|-----------------|---------------|----------|
| `Symbol` | workspace.rs:21 | parsing/symbols.rs:7 | parsing/symbols.rs:7 | ✅ |
| `DeletionIndex` | workspace.rs:21 | fuzzy/deletion_neighborhood.rs:18 | fuzzy/deletion_neighborhood.rs:18 | ✅ |
| `DashMap` (ext dep) | workspace.rs:17 | dashmap crate lib.rs:88 | dashmap-5.5.3/src/lib.rs:88 | ✅ |

**Accuracy:** 3/3 correct. Cross-file workspace definitions and external dependency definitions both resolve accurately.

### Find References ✅ PASS

| Symbol | Expected Refs | Actual Refs | Correct? |
|--------|---------------|-------------|----------|
| `Workspace` | Multiple in server.rs, deps/mod.rs, workspace.rs | 10+ refs across 3 files with correct line/col | ✅ |
| `scan_directory` | 7 refs (1 in server.rs, 6 in workspace.rs) | 7 refs matching exactly | ✅ |

**Accuracy:** All reference locations verified against `grep` output — line numbers and column ranges match exactly.

### Document Symbols ✅ PASS

Listed 50 symbols from `workspace.rs` including:
- Structs: `SymbolLocation` (L27), `Reference` (L34), `FileEntry` (L43), `Workspace` (L49), `ScanStats` (L385)
- Functions: `new` (L62), `index_file` (L73), `scan_directory` (L124), `update_file` (L192), etc.
- Constants: `MAX_SCAN_DEPTH` (L137)
- Modules: `tests` (L475)
- Test functions: all 15 test functions listed

**Accuracy:** Spot-checked multiple symbols against source — all line numbers correct.

### Workspace Symbol Search ✅ PASS

| Query | Results | Correct? |
|-------|---------|----------|
| `QuickLspServer` | `src/lsp/server.rs:18 struct` | ✅ |
| `scan_directory` | `workspace.rs:124 function`, `scan_dir_recursive:139 function` (fuzzy) | ✅ |
| `DependencyIndex` | `src/deps/mod.rs:57 struct` | ✅ |
| `Tokenizer` | (empty - no such symbol) | ✅ correct empty result |

### Hover ✅ PASS (workspace) / ⚠️ PARTIAL (dependencies)

| Symbol | Type | Result | Correct? |
|--------|------|--------|----------|
| `scan_directory` | workspace fn | Full doc comment displayed correctly | ✅ |
| `DashMap` | ext dep struct | Showed DashMap doc about locking behaviour | ✅ |
| `new` | workspace fn | Showed `pub const fn new(val: T) -> Mutex<R, T>` | ❌ Wrong — showed std Mutex::new instead of Workspace::new |
| `PathBuf` | std lib | Timed out (stuck retrieving) | ❌ Timeout |
| `Url` | ext dep | Timed out (stuck retrieving) | ❌ Timeout |

**Issues:**
1. **Name collision on common identifiers**: `new` is too generic — hover returned `Mutex::new` signature instead of the contextually correct `Workspace::new`. The heuristic lookup finds the first match by name without considering scope/context.
2. **Hover timeout on missing symbols**: When a symbol isn't in the dependency index, `hover_info()` calls `refresh_if_stale()` + `index_pending()` synchronously on the LSP request thread, blocking the response while 4233 files are being indexed.

### Completion ⚠️ UNTESTED

Could not reliably trigger `C-x C-o` (omni-complete) through tmux key sending. The `^X mode` detection works but the follow-up `C-o` keystroke doesn't chain properly in the tmux→vim pipeline. Manual testing would be needed.

### Signature Help ⚠️ UNTESTED

Not tested via tmux in this session.

## Critical Performance Issue 🔴

### Dependency Indexing Blocks Server

- The background indexing task (`DependencyIndex::index_pending`) runs on a `spawn_blocking` thread but consumes **100% CPU for 6+ minutes** (release build) indexing 4233 .rs files from 102 Cargo dependencies.
- The bottleneck is in `quicklsp::parsing::tokenizer::scan()` → `core::str::count::do_count_chars()` (confirmed via gdb backtrace).
- During this time:
  - **Workspace features work** (go-to-def, references, symbols on project files) thanks to DashMap concurrent access ✅
  - **Hover on missing dep symbols blocks indefinitely** because `hover_info()` synchronously calls `index_pending()` on the request thread ❌
  - Memory stabilizes at ~243MB (no leak, just slow progress)

### Concurrent Design Works Well

Despite the performance issue, the DashMap-based concurrent architecture proves out:
- LSP queries on already-indexed content work immediately during background indexing
- No deadlocks or data races observed
- File opens/edits work correctly alongside background scanning

## Recommendations

1. **Don't block LSP requests on dependency indexing**: `hover_info()` should return `None` for unknown symbols rather than triggering synchronous re-indexing.
2. **Throttle/batch dependency indexing**: Index dependencies incrementally with yields to avoid starving the event loop.
3. **Profile tokenizer performance**: `scan()` is CPU-bound on character counting — consider optimizing the hot path or limiting scan depth for large files.
4. **Scope-aware hover**: For common names like `new`, `get`, `set`, consider the cursor's context (file, surrounding type) to disambiguate.
5. **vim-lsp autoregistration**: The `au User lsp_setup` autocmd pattern didn't fire automatically — may need a different registration approach.
