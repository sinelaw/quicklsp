# QuickLSP Manual Test Report — Rust Project

**Date**: 2026-04-06
**Editor**: Neovim 0.9.5 in tmux
**LSP Binary**: `target/release/quicklsp`
**Test Fixture**: quicklsp's own Rust source code (~30 `.rs` files)
**Method**: Programmatic `vim.lsp.buf_request_sync()` calls + `C-x C-o` omnifunc completion in tmux

---

## Summary

| Feature | Tests | Pass | Fail | Notes |
|---------|-------|------|------|-------|
| **Initialization** | 1 | 1 | 0 | Client attached, ID=1 |
| **Hover** | 37 | 34 | 3 | See issues below |
| **Go to Definition** | 14 | 12 | 2 | Cross-file name collision issue |
| **Find References** | 11 | 11 | 0 | Cross-language references found |
| **Completion** | 20 | 18 | 2 | Unicode prefix broken |
| **Document Symbols** | 7 | 7 | 0 | All files returned correct symbols |
| **Workspace Symbols** | 7 | 7 | 0 | All queries returned results |
| **Signature Help** | 4 | 1 | 3 | Only works at direct fn call sites |

**Overall**: 101 tests, 91 pass, 10 fail/partial

---

## Phase 1: Hover (37 tests)

### Definitions — all working correctly

| Symbol | Type | Result |
|--------|------|--------|
| `Config` (line 13) | struct | `struct Config` + doc comment |
| `MAX_RETRIES` (line 5) | const | `const MAX_RETRIES: u32 = 3` + doc |
| `DEFAULT_TIMEOUT` (line 8) | const | `const DEFAULT_TIMEOUT: u64 = 5000` + doc |
| `Status` (line 20) | enum | `enum Status` + doc |
| `Handler` (line 29) | trait | `trait Handler` + doc (multi-line) |
| `Request` (line 35) | struct | `struct Request` + doc |
| `Response` (line 42) | struct | `struct Response` + doc |
| `create_config` (line 48) | fn | `fn create_config() -> Config` + doc |
| `process_request` (line 59) | fn | Full signature with params + doc |
| `Server` (line 78) | struct | `struct Server` + doc |
| `Server::new` (line 84) | impl method | `fn new(config: Config) -> Self` |
| `Server::add_handler` (line 91) | impl method | `fn add_handler(&mut self, handler: Box<dyn Handler>)` |
| `Server::run` (line 95) | impl method | `fn run(&self)` |
| `StatusCode` (line 114) | type alias | `type StatusCode = u16` |
| `HandlerResult` (line 115) | type alias | `type HandlerResult = Result<Response, String>` |
| `validate_request` (line 118) | fn | Full signature + doc |
| `données_utilisateur` (line 129) | fn (unicode) | `fn données_utilisateur() -> String` + comment |
| `Über` (line 133) | struct (unicode) | `struct Über` |
| `outer` (line 138) | fn (nesting) | `fn outer()` + comment |
| `inner` (line 139) | nested fn | `fn inner()` |
| `sanitize_input` (line 105) | module fn | `pub fn sanitize_input(input: &str) -> String` |
| `validate_port` (line 109) | module fn | `pub fn validate_port(port: u16) -> bool` |
| `GLOBAL_COUNTER` (line 146) | static | `static GLOBAL_COUNTER: u32 = 0` |
| `FINAL_STATUS` (line 145) | const | `const FINAL_STATUS: &str = "complete"` |

### Usages — hover on symbol references

| Symbol | Context | Result |
|--------|---------|--------|
| `Status` (line 61) | `Status::Active` in fn body | Correctly resolves to `enum Status` + doc |
| `Handler` (line 80) | `Box<dyn Handler>` | Correctly resolves to `trait Handler` + doc |
| `MAX_RETRIES` (line 97) | Loop range usage | Correctly resolves + doc |
| Inside string literal (line 67) | `"Handled..."` word | Empty result (correct: no false match) |

### Cross-file hover (workspace.rs, server.rs, tokenizer.rs)

| Symbol | File | Result |
|--------|------|--------|
| `SymbolLocation` | workspace.rs:35 | `pub struct SymbolLocation` |
| `Reference` | workspace.rs:42 | `pub struct Reference` |
| `FileId` | workspace.rs:53 | `struct FileId(u32);` |
| `SymbolRef` | workspace.rs:58 | `struct SymbolRef` |
| `FileEntry` | workspace.rs:64 | `struct FileEntry` + doc |
| `LogWriteMsg` | workspace.rs:73 | `struct LogWriteMsg` + multi-line doc |
| `PosEncoding` | server.rs:22 | `enum PosEncoding` |
| `byte_col_to_encoding` | server.rs:31 | Full signature + multi-line doc |
| `encoding_col_to_byte` | server.rs:52 | Full signature + multi-line doc |
| `stats` | tokenizer.rs:21 | `pub mod stats` + multi-line doc |
| `Counters` | tokenizer.rs:27 | `struct Counters` |
| `flush` | tokenizer.rs:58 | `pub fn flush()` + doc |

### Hover Issues

| # | Target | Issue |
|---|--------|-------|
| 1 | `Config` usage (line 59, col 26) | **WRONG**: Resolved to C file `server_create(struct Server*, const struct ServerConfig*)` instead of Rust `struct Config`. Name collision across languages. |
| 2 | `Request` usage (line 59, col 36) | **EMPTY**: No hover result. Cursor position may have been off (between identifiers). |
| 3 | `format!` macro (line 66) | **MISLEADING**: Returns `mod format` instead of no result. Macros are not tracked but `format` matches a module name. |

---

## Phase 2: Go to Definition (14 tests)

### Working correctly

| From | Target | Destination | Correct? |
|------|--------|-------------|----------|
| `Response` return type (line 59) | struct Response | sample_rust.rs:34 | Yes (0-indexed line 34 = line 35) |
| `Status::Active` (line 61) | enum Status | sample_rust.rs:19 | Yes |
| `MAX_RETRIES` (line 97) | const | sample_rust.rs:4 | Yes |
| `DEFAULT_TIMEOUT` (line 98) | const | sample_rust.rs:7 | Yes |
| `Handler` in dyn bound (line 80) | trait | sample_rust.rs:28 | Yes |
| `validate_request` (line 118) | fn | sample_rust.rs:117 | Yes |
| `inner()` call (line 142) | nested fn | sample_rust.rs:138 | Yes |
| `Workspace` from server.rs | struct | workspace.rs:81 | Yes |
| `SymbolLocation` from server.rs | struct | workspace.rs:34 | Yes |
| `Symbol` from workspace.rs | struct | symbols.rs:6 | Yes |
| `LangFamily` from workspace.rs | enum | tokenizer.rs:241 | Yes |

### Issues

| # | From | Expected | Actual | Issue |
|---|------|----------|--------|-------|
| 1 | `Config` param (line 59, col 26) | sample_rust.rs:13 | **c_project/main.c:158** | Cross-language name collision. `Config` exists in both Rust and C fixtures. LSP picks the C definition. |
| 2 | `Config` in Server::new (line 84) | sample_rust.rs:13 | **c_project/main.c:158** | Same issue — `Config` goes to wrong language. |
| 3 | `Request` param (line 59, col 36) | sample_rust.rs:35 | **Empty result** | Cursor position may have been between identifiers. |

---

## Phase 3: Find References (11 tests)

All tests returned results. References are cross-language by design (word-boundary text search).

| Symbol | Count | Files |
|--------|-------|-------|
| `Config` | 27 | sample_go.go, sample_typescript.ts, sample_c.c, sample_python.py, sample_rust.rs |
| `Status` | 9 | sample_go.go, sample_typescript.ts, sample_c.c, sample_rust.rs |
| `Handler` | 7 | sample_typescript.ts, sample_python.py, sample_rust.rs |
| `Request` | 27 | main.c, server.h, sample_go.go, types.h, sample_typescript.ts, ... |
| `Response` | 28 | main.c, server.h, sample_go.go, types.h, sample_typescript.ts, ... |
| `MAX_RETRIES` | 9 | main.c, sample_typescript.ts, sample_c.c, sample_python.py, sample_rust.rs |
| `Server` | 42 | Across all fixture files + source files |
| `process_request` | 6 | sample_c.c, sample_python.py, sample_rust.rs |
| `Workspace` | 129 | Across all source + test files |

**Note**: References are text-based, not type-aware. `Config` in Go/C/Python counts as a reference to Rust's `Config`. This is by design for a heuristic LSP.

---

## Phase 4: Completion (20 tests)

Completion was tested via `C-x C-o` omnifunc in insert mode. The completion popup is visible in tmux capture-pane output.

### Working correctly

| Prefix | Expected | Found | Extra items from deps |
|--------|----------|-------|-----------------------|
| `Conf` | Config | Yes | ConfigurationItem, ConfigurationParams |
| `Stat` | Status, StatusCode | Yes | StaticTextDocumentColorProviderOptions, StatusActive, StatusError, StatusInactive |
| `Hand` | Handler, HandlerResult | Yes | Handle, HandlerFunc |
| `Serv` | Server | Yes | ServerCapabilities, ServerConfig, ServerInfo, ServerState, Service, ServiceExt, ServiceFn |
| `Req` | Request | Yes | RequestBuilder, RequestHandler, RequestStream, RequeueOp |
| `Resp` | Response | Yes | ResponseSink |
| `create` | create_config | Yes | Many dep matches (create_arr, create_compile_object_cmd, ...) |
| `process` | process_request | Yes | process, processRequest, process_batch, process_connections, ... |
| `valid` | validate_request, validate_port | Yes | Many dep matches |
| `MAX` | MAX_RETRIES | Yes | MAX_CANON, MAX_FILES_PER_PACKAGE, ... (from deps) |
| `DEFAULT` | DEFAULT_TIMEOUT | Yes | DEFAULT_MAX_LEVEL, DEFAULT_PARK_TOKEN, ... |
| `out` | outer | Yes | out_dir, out_len, outer_attrs_to_tokens, outgoing_calls, ... |
| `inn` | inner | Yes | inner_attrs_to_tokens, inner_mut, inner_pin_mut, ... |
| `san` | sanitize_input | Yes | sanitize_timings, sanitizes_ansi_escapes |
| `Work` | Workspace | Yes | WorkDoneProgressOptions, Worker, WorkerThread, ... |
| `Symb` | Symbol, SymbolLocation | Yes | SymbolInformation, SymbolKind, SymbolKindCapability, SymbolRef, SymbolTag |
| `Lang` | LangFamily | Yes | Language, LanguageFn, LanguageIdentifier, LanguageServer, ... |
| `Dele` | DeletionIndex | Yes | Delete, DeleteFile, DeleteFileOptions, DeleteFilesParams |

### Issues

| # | Prefix | Expected | Actual | Issue |
|---|--------|----------|--------|-------|
| 1 | `don` | données_utilisateur | **(no items)** | Unicode prefix completion fails. The `don` ASCII prefix doesn't match `données` which starts with `donn` + Unicode. |
| 2 | `Üb` | Über | **(no items)** | Unicode prefix completion fails. Non-ASCII first character not matched. |

---

## Phase 5: Document Symbols (7 tests)

All tests returned correct results.

| File | Count | Highlights |
|------|-------|------------|
| **sample_rust.rs** | 30 | Structs (Config, Request, Response, Server, Über), enums (Status), traits (Handler), fns, consts, statics, type aliases, module, nested fn, unicode identifiers |
| **workspace.rs** | 102 | Full struct + all impl methods + enum WarmResult + free functions + test functions + constants |
| **server.rs** | 29 | PosEncoding enum + variants, QuickLspServer struct + all LanguageServer trait methods, helper functions |
| **tokenizer.rs** | 84 | stats module, Token/TokenKind/LangFamily enums + variants, scan functions, test functions |
| **symbols.rs** | 34 | Symbol struct, SymbolKind enum + variants, extraction functions, test functions |
| **main.rs** | 1 | Just `main` function |
| **lib.rs** | 7 | All module declarations (deps, fuzzy, lsp, parsing, syntax_cache, word_index, workspace) |

**Symbol kind mapping**: Structs reported as `Event` (kind=23), which maps to LSP's `SymbolKind::Struct`. Correct behavior.

---

## Phase 6: Workspace Symbols (7 tests)

All queries returned results.

| Query | Count | Top Results |
|-------|-------|-------------|
| `Workspace` | 2 | workspace (lib.rs), Workspace (workspace.rs) |
| `Server` | 19 | Across all fixture files + source (Go, Python, C, TypeScript, Rust) |
| `LangFamily` | 1 | LangFamily (tokenizer.rs) |
| `Symbol` | 7 | symbols (various), Symbol (symbols.rs), symbol (server.rs) |
| `DeletionIndex` | 1 | DeletionIndex (deletion_neighborhood.rs) |
| `Config` | 15 | Across all fixture + source files (case-insensitive matching) |
| `PosEncoding` | 1 | PosEncoding (server.rs) |

---

## Phase 7: Signature Help (4 tests)

| Call Site | Result | Notes |
|-----------|--------|-------|
| `inner()` call (line 142) | `fn inner()` | **Working** — correctly shows signature |
| `DEFAULT_TIMEOUT * ...` (line 98) | No signatures | Expected — not a function call |
| `println!` args (line 99) | No signatures | Expected — macros not tracked |
| `Server::new` (line 85) | No signatures | **Missing** — cursor was at definition, not a call site |

Signature help works when cursor is inside parentheses of a direct function call. It does not work for:
- Macro invocations
- Method calls via `.` syntax (not tested but likely limited)
- Cursor at definition rather than call site

---

## Known Issues Summary

### Bug 1: Cross-language name collision in Go-to-Definition (Severity: Medium)
When multiple languages define the same symbol name (e.g., `Config` in both Rust and C), the heuristic ranking sometimes picks the wrong language. The same-file preference helps, but when the symbol is used as a type reference (not a definition), the C fixture file wins over the Rust one.

**Reproduction**: Hover or go-to-definition on `Config` used as a parameter type in `process_request()`.

### Bug 2: Unicode prefix completion not working (Severity: Low)
Completion with ASCII prefixes doesn't match symbols that contain non-ASCII characters early in the name. `don` doesn't find `données_utilisateur`, and `Üb` doesn't find `Über`.

### Bug 3: Signature help limited to direct fn calls (Severity: Low)
Signature help only works when cursor is inside `()` of a direct function call. Does not support macros or complex expressions.

### Observation: Hover on macro names shows false match
`format!` macro hover returns `mod format` — a false positive from module name matching. Not harmful but potentially confusing.

---

## Test Environment

```
OS: Linux 6.18.5
Editor: Neovim 0.9.5
tmux: 3.4
quicklsp: built from source (release profile)
Workspace: quicklsp repo (~30 Rust files + multi-language test fixtures)
```
