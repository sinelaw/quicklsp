# Import-Aware Definition Ranking — Design Document

## Problem

When multiple languages define the same symbol name (e.g., `Config` in both Rust and C fixture files), cross-file go-to-definition and hover resolve to the wrong language. The current `rank_definitions()` scoring is:

| Signal | Score | Description |
|--------|-------|-------------|
| Qualifier match | +4 | `Workspace::new` → prefer definitions inside `impl Workspace` |
| Same file | +2 | Prefer definitions in the file being queried from |

When querying from a file that doesn't define the symbol, both candidates score 0 and the winner is determined by hash map iteration order.

**Reproduction**: See `test_rust_cross_language_collision_bugs` in `tests/rust_integration.rs` — 8 failures across `Config`, `process_request`, and `Request`.

---

## Current Architecture

### What exists

- `FileEntry` stores `symbols: Vec<Symbol>` and `lang: Option<LangFamily>` per file
- `SyntaxCache` stores tree-sitter `Tree` per editor-open file (already parsed)
- `definition_score()` is called per candidate during `rank_definitions()`
- Files opened via `didOpen` have their source in `open_sources` and their tree in `syntax_cache`
- Tree-sitter grammars exist for: C, C++, Rust, Go, Python, JavaScript, TypeScript, Java, Ruby

### What doesn't exist

- No import/include/use statement tracking
- No `SymbolKind::Import` variant
- No file dependency graph
- No same-language preference
- Tree-sitter queries don't capture import nodes for any language

---

## Proposed Solution: Two-Tier Scoring

### Tier 1: Same-language preference (+1 score)

**Cost**: Zero. `lang` is already stored in `FileEntry`.

**Implementation**: In `definition_score()`, compare the querying file's `LangFamily` with the candidate's. One enum comparison per candidate.

**Effect**: Fixes 90% of cross-language collisions. `rust_consumer.rs` is Rust → prefers Rust `Config` over C/Go/Python `Config`.

**Limitation**: Doesn't help when two files of the same language define the same symbol.

### Tier 2: Import-stem matching via cached syntax tree (+3 score)

**Cost**: Zero storage. Queries the already-cached tree-sitter parse tree on demand during ranking. One tree-sitter query per `rank_definitions()` call (not per candidate).

**Implementation**:
1. Add per-language tree-sitter query strings for import nodes (static `&str` constants)
2. Add `import_stems()` method to `SyntaxCache` that runs the query on the cached tree
3. Extract file stems from import paths (strip quotes, separators, language prefixes)
4. In `definition_score()`, check if candidate file's stem matches any import stem

**Key insight**: The querying file is always editor-open, so its tree is always in `SyntaxCache`. No new parsing needed.

**Effect**: Precise ranking based on actual import relationships. `rust_consumer.rs` uses `Config` from Rust code → its imports point to Rust files → Rust `Config` scores +3.

### Updated scoring table

| Signal | Score | Cost |
|--------|-------|------|
| Qualifier match | +4 | Existing (zero change) |
| Imported file | +3 | One tree-sitter query per ranking call |
| Same file | +2 | Existing (zero change) |
| Same language | +1 | One enum comparison per candidate |

---

## Design Decisions and Tradeoffs

### Why not store imports in FileEntry?

Storing `Vec<String>` of imports per file would cost ~50-200 bytes per file. For a large repo (65K files) that's 3-13MB extra RAM. The current design strips even `doc_comment` and `signature` from symbols after bulk scan to save memory. Adding import storage goes against this design principle.

The lazy approach (query the cached tree) costs zero bytes of storage. The tree is already there for open files.

### Why not full path resolution?

Resolving `use crate::workspace` → `/home/user/project/src/workspace.rs` requires:
- Language-specific module resolution rules
- Filesystem stat calls
- C/C++ include path configuration

File-stem matching (`workspace` matches any `workspace.rs` in the workspace) is an 80% solution with zero configuration. False positives are rare in practice because file stems tend to be unique within a project.

### Why not just same-language?

Same-language alone (+1) works for cross-language collisions but fails when:
- Two Rust files define the same symbol (e.g., `Config` in `config.rs` and `test_config.rs`)
- The querying file imports one but not the other

Import-stem matching distinguishes these cases without any storage cost.

### Why not a bloom filter per file?

A bloom filter (32 bytes/file) would be more compact than `Vec<String>` for stored imports, but since we're doing lazy extraction from the cached tree, there's nothing to store. Bloom filters would only make sense if we needed import info for non-open files (which we don't — ranking always operates from an open file).

### Why not track imports during bulk scan?

During `scan_directory`, files are parsed and trees are discarded immediately to keep memory low. Adding import extraction there would require either:
- Storing imports (costs RAM) — rejected above
- Re-parsing trees later (costs CPU) — pointless since trees are cached for open files

The querying file is always open. Non-open files don't need import info for ranking.

---

## Import Node Types Per Language

These are the tree-sitter node types needed for import extraction queries:

| Language | Node type | Path child | Example |
|----------|-----------|------------|---------|
| **Rust** | `use_declaration` | `argument: (_)` | `use crate::workspace::Config;` |
| **Rust** | `mod_item` | `name: (identifier)` | `mod workspace;` |
| **C/C++** | `preproc_include` | `path: (string_literal)` | `#include "types.h"` |
| **C/C++** | `preproc_include` | `path: (system_lib_string)` | `#include <stdio.h>` |
| **Python** | `import_statement` | `name: (dotted_name)` | `import os.path` |
| **Python** | `import_from_statement` | `module_name: (dotted_name)` | `from workspace import Symbol` |
| **Go** | `import_spec` | `path: (interpreted_string_literal)` | `import "fmt"` |
| **TS/JS** | `import_statement` | `source: (string)` | `import { x } from './foo'` |
| **Java** | `import_declaration` | `(scoped_identifier)` | `import java.util.List;` |
| **Ruby** | Method call pattern | N/A (require is a function) | `require "foo"` |

### Stem extraction examples

| Raw import path | Extracted stems |
|----------------|-----------------|
| `"types.h"` | `types.h`, `types` |
| `<stdio.h>` | `stdio.h`, `stdio` |
| `crate::workspace` | `workspace` |
| `crate::parsing::symbols` | `symbols`, `parsing` |
| `mod workspace` | `workspace` |
| `from workspace import Symbol` | `workspace` |
| `"fmt"` | `fmt` |
| `'./foo'` | `foo` |
| `os.path` | `path`, `os` |

---

## C/C++ Include Path Support

### Problem

`#include <stdio.h>` references a system header. `#include "types.h"` references a local file. The stem-matching heuristic works for local includes but system includes may produce false matches.

### Approach

1. **Default**: Match by file stem only. `#include "types.h"` → boost any `types.h` in workspace. System includes (`<...>`) are still matched but typically don't collide with workspace files.

2. **Optional**: Accept `include_paths: Vec<PathBuf>` in `Workspace` configuration. When set, resolve `#include "foo.h"` to an absolute path by searching:
   - Directory of the including file
   - Each include path in order

3. **Configuration source**: Include paths could come from:
   - `compile_commands.json` (standard CMake/build system output)
   - `.clangd` configuration
   - LSP `initializationOptions`
   - A `.quicklsp.toml` project config file

4. **Deferred**: Full include path resolution is not needed for the initial fix. Same-language preference (+1) already eliminates most cross-language collisions for C projects. Include path support can be added incrementally.

---

## Implementation Plan

### Phase 1: Same-language preference (minimal, high impact)

1. In `definition_score()`, add:
   ```rust
   // +1: Same language family
   if let Some(cur_lang) = current_lang {
       let def_lang = self.files.get(&loc.file).and_then(|e| e.lang);
       if def_lang == Some(cur_lang) {
           score += 1;
       }
   }
   ```
2. In `rank_definitions()`, look up `current_lang` once from `self.files`.
3. Add unit tests.

**Estimated diff**: ~20 lines in `workspace.rs`.

### Phase 2: Import-stem matching (zero storage, on-demand)

1. Add `import_query_for_ext()` — static function returning per-language query strings
2. Add `import_path_to_stems()` — strips quotes/separators, extracts file stems
3. Add `SyntaxCache::import_stems()` — runs query on cached tree, returns `Vec<String>`
4. In `rank_definitions()`, call `import_stems()` once for the querying file
5. In `definition_score()`, check `file_matches_import_stems()`
6. Add unit tests for stem extraction and integration tests for ranking

**Estimated diff**: ~150 lines in `syntax_cache.rs`, ~30 lines in `workspace.rs`.

### Phase 3: C/C++ include paths (optional, deferred)

1. Add `include_paths: Vec<PathBuf>` to `Workspace`
2. Add `set_include_paths()` method
3. In the LSP server `initialize()`, read include paths from `initializationOptions`
4. In `file_matches_import_stems()`, resolve `#include` paths using include_paths

**Estimated diff**: ~50 lines.

---

## Files to Modify

| File | Changes |
|------|---------|
| `src/workspace.rs` | `rank_definitions()`, `definition_score()`, add `file_matches_import_stems()` |
| `src/syntax_cache.rs` | Add `import_stems()`, `import_query_for_ext()`, `import_path_to_stems()` |
| `tests/rust_integration.rs` | `test_rust_cross_language_collision_bugs` should start passing |

---

## Test Coverage

The failing integration test `test_rust_cross_language_collision_bugs` exercises exactly this scenario:
- `rust_consumer.rs` queries `Config`, `process_request`, `Request` cross-file
- Without same-file preference, the C/Go/Python definitions currently win
- With same-language (+1), Rust definitions should win
- With import-stem matching (+3), the fix is precise even for same-language collisions

8 failing checks (4 bugs × 2 phases) should become 0 after implementation.
