# Go-to-Definition on Rust Locals — Interactive Test

Manual test of quicklsp's `textDocument/definition` behavior on Rust
**local variables** (let-bindings and function parameters), driven
through two editors in a tmux session.

## Setup

- quicklsp binary: `target/release/quicklsp` (release build)
- Test corpus: [sinelaw/fresh](https://github.com/sinelaw/fresh)
  (`/tmp/fresh`, shallow clone)
- Target file: `crates/fresh-editor/src/model/buffer.rs` (8k lines)
- Target functions:
  - `TextBuffer::from_bytes_raw` (line 467)
  - `TextBuffer::from_bytes` (line 508) — contains locals that
    **shadow** names from `from_bytes_raw`

## Phase 1: nvim + quicklsp (direct LSP attach)

Minimal `init.lua` attaches quicklsp on `FileType=rust`, `root_dir=/tmp/fresh`.
Tests invoke `vim.lsp.buf.definition()` via `gd`; the cursor landing position
is read from tmux screen capture.

| # | Reference         | Cursor (line:col) | Expected → Actual                           | Result |
|---|-------------------|-------------------|----------------------------------------------|--------|
| 1 | `config` usage    | `config.rs` 5721:24 | `let config = Config::default();` @ 5713  | PASS |
| 2 | `bindings` usage  | `config.rs` 5747:38 | `let bindings = config.resolve_keymap…` @ 5745 | PASS |
| 3 | `enter_bindings`  | `config.rs` 5754:35 | `let enter_bindings: Vec<_> = …` @ 5747   | PASS |
| 4 | `has_insert_newline` | `config.rs` 5756:13 | `let has_insert_newline = …` @ 5754     | PASS |
| 5 | `config` usage    | `config.rs` 5745:24 | `let config = Config::default();` @ 5744  | PASS |
| 6 | `bytes` usage     | `buffer.rs` 478:58  | `let bytes = content.len();` @ 468        | PASS |
| 7 | `line_feed_cnt`   | `buffer.rs` 478:65  | `let line_feed_cnt = …` @ 475             | PASS |
| 8 | `piece_tree`      | `buffer.rs` 483:26  | `let piece_tree = if bytes > 0 { …` @ 477 | PASS |

## Phase 2: fresh editor + quicklsp (universal LSP)

### Enabling quicklsp

Followed the in-editor flow:

1. `Ctrl+P` → `settings` → Open Settings
2. `/` → `universal lsp` → Enter → focus `Universal Lsp` → Enter (edit entry)
3. Tab into quicklsp server config → toggle `Enabled` to `[✓]` with Space
4. `Ctrl+S` to save the inner edit dialog → Esc → Tab to `[ Save ]` → Enter

Two gotchas found in the editor's config flow (both worth filing upstream):

- The outer map key saved as `""` instead of `quicklsp`.
- The default `only_features` whitelist for quicklsp is
  `["hover", "signature_help", "document_symbols", "workspace_symbols"]`
  — **`definition` is not included**, so `textDocument/definition` is never
  routed to quicklsp from a default install.

Final `~/.config/fresh/config.json` after manual correction:

```json
{
  "universal_lsp": {
    "quicklsp": [{
      "command": "quicklsp",
      "enabled": true,
      "auto_start": true,
      "only_features": ["hover","signature_help","document_symbols",
                        "workspace_symbols","definition","references"],
      "root_markers": ["Cargo.toml","package.json","go.mod",
                       "pyproject.toml","requirements.txt",".git"]
    }]
  }
}
```

### Tests

Status bar confirmed: `LSP [rust/QuickLSP: ready, rust/rust-analyzer: error]`
(rust-analyzer not installed; quicklsp answered all requests.)
Navigation via `Ctrl+P :LINE`, cursor arrows, `F12` (Go to Definition).

| # | Reference         | Cursor (line:col) | Jump → (verified on-screen)          | Result |
|---|-------------------|-------------------|---------------------------------------|--------|
| 1 | `bytes` in `from_bytes_raw`  | 478:58 | 468:13 `let bytes = content.len();`    | PASS |
| 2 | `line_feed_cnt`              | 478:65 | 475:13 `let line_feed_cnt = …`         | PASS |
| 3 | `piece_tree`                 | 483:26 | 477:13 `let piece_tree = if bytes > 0` | PASS |
| 4 | `buffer` in `TextBuffer { buffers: vec![buffer] }` | 493:27 | 474:13 `let buffer = StringBuffer::new(0, content);` | PASS |
| 5 | **Shadowed** `bytes` in `from_bytes` | 521:29 | 512:13 `let bytes = utf8_content.len();` (not 468) | PASS |
| 6 | Function **parameter** `content` reference | 471:53 | 467:23 (`fn from_bytes_raw(content: …)`) | PASS |

## Observations

- **Lexical scoping works.** Test 5 is the money test: both `from_bytes_raw`
  and `from_bytes` declare `let bytes = …`. Looking up `bytes` from within
  `from_bytes` correctly resolved to the `from_bytes`-local binding (line 512),
  not the earlier one at line 468. The ranking at
  `workspace.rs:856` (`find_local_definition_at`) is doing its job: filter to
  candidates where `s.line <= cursor_line && cursor <= scope_end_line`, then
  `max_by_key((depth, line))` picks the deepest-and-closest.
- **Function parameters are treated as locals** (depth > 0 symbols), so
  jumping from a `&content` use to `fn foo(content: …)` works.
- **Cross-function `buffer` reference stays in scope.** Test 4 jumps from
  `vec![buffer]` at line 493 to the `let buffer = …` at line 474 in the
  same function, even though `from_bytes` at line 518 also has a `buffer`.
- **No false positives on non-identifier positions.** An earlier test with
  the cursor on `>` (of `Vec<_>`) returned a spurious match in an unrelated
  Python file via `find_definitions` fallback — worth investigating whether
  word extraction should refuse to look up symbols when the cursor is not on
  an identifier character, rather than matching something nearby.
