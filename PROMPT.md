# Continuation Prompt: Refactor tree-sitter parsers to use SCM queries

## Context

QuickLSP is a high-performance Rust LSP server at `/home/user/quicklsp`. We recently added tree-sitter support for all languages (C, C++, Rust, Go, Python, JavaScript, TypeScript, Java, Ruby). Currently, each language parser manually walks the AST using `node.kind()` matching and `child_by_field_name()` calls — ~3000 lines of repetitive Rust code across 9 language files.

The tree-sitter ecosystem's canonical approach is **SCM query patterns** — declarative S-expression files that match tree structure and capture named nodes. Every tree-sitter grammar crate ships with `tags.scm` files (in `queries/` directories) that already define patterns for extracting definitions and references. We should use these.

## What exists now

### Architecture
- `src/parsing/tree_sitter_parse/` — 9 language files + `common.rs` + `mod.rs`
- Each language file implements `TsParser` trait: `fn parse(source: &str) -> ParseResult`
- `common.rs` provides `run_parse()` which: parses source → calls language-specific `collect_defs` callback → collects identifier occurrences
- `mod.rs` has `try_parse(path, source)` dispatching by file extension, and `language_for_extension()`
- `src/workspace.rs` calls `try_parse()` first, falls back to hand-written tokenizer
- `src/syntax_cache.rs` uses `language_for_extension()` for AST caching at cursor positions

### Data structures produced
```rust
// src/parsing/tree_sitter_parse.rs
pub struct ParseResult {
    pub symbols: Vec<Symbol>,         // definitions
    pub occurrences: Vec<Occurrence>, // all identifier positions
}

// src/parsing/symbols.rs
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,        // Function, Method, Class, Struct, Enum, Interface, Constant, Variable, Module, TypeAlias, Trait, Unknown
    pub line: usize,
    pub col: usize,              // byte offset from line start
    pub def_keyword: String,     // "fn", "struct", "class", "define", "typedef", "field", "method", "variable", "parameter", "const", "enum", etc.
    pub doc_comment: Option<String>,
    pub signature: Option<String>,
    pub visibility: Visibility,  // Public, Private, Unknown
    pub container: Option<String>, // parent scope name (e.g. "Config" for methods)
    pub depth: usize,            // 0=top-level, >0=nested (fields, locals, params)
    pub scope_end_line: Option<usize>, // for local variable scope tracking
}

// src/parsing/tokenizer.rs
pub struct Occurrence {
    pub word_offset: u32,  // byte offset in source
    pub word_len: u16,
    pub line: usize,
    pub col: usize,
    pub role: OccurrenceRole, // Definition or Reference
}
```

### Current pattern (manual AST walking)
Each language file has ~200-600 lines of this:
```rust
fn extract_definition(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    match node.kind() {
        "function_definition" => { /* extract name, visibility, etc. */ }
        "struct_specifier" => { /* ... */ }
        "enum_specifier" => { /* ... */ }
        // ... many more arms per language
    }
}
```

### Tree-sitter grammar crates ship `tags.scm`
Each crate includes query files like:
```scheme
; C tags.scm
(struct_specifier name: (type_identifier) @name body:(_)) @definition.class
(function_declarator declarator: (identifier) @name) @definition.function
(type_definition declarator: (type_identifier) @name) @definition.type
(enum_specifier name: (type_identifier) @name) @definition.type
```

These use `@definition.function`, `@definition.class`, `@definition.type`, `@definition.method`, `@name`, `@doc` captures — a standardized convention across all tree-sitter grammars.

## What to do

Refactor the tree-sitter parsing to use SCM queries instead of manual AST walking. The goal is to replace the ~3000 lines of per-language Rust code with ~100 lines of generic query execution + per-language `.scm` files.

### Design

1. **Create a query-based parser** in `common.rs` (or a new `query_parser.rs`):
   - Load a `.scm` query string for each language
   - Use `tree_sitter::Query` + `QueryCursor` to execute it against the parsed tree
   - Map `@definition.function` → `SymbolKind::Function`, `@definition.class` → `SymbolKind::Class`, etc.
   - Extract `@name` capture text as the symbol name
   - Extract `@doc` capture as doc_comment
   - Determine `depth` from node nesting (is it inside a class/struct body?)
   - Determine `container` from enclosing named scope
   - Determine `visibility` from language-specific patterns (Rust: `pub`, Go: uppercase, Python: `_` prefix, Java: modifiers node)

2. **SCM query files** — either:
   - (a) Embed the grammar crates' `tags.scm` as starting points and extend them, or
   - (b) Write our own `.scm` files that capture exactly what we need (name, kind, container, visibility)
   
   Option (b) is likely better because `tags.scm` captures are minimal — we need extra captures for visibility modifiers, containers, fields, enum values, etc. that `tags.scm` doesn't cover.

3. **Per-language query files** stored as const strings or loaded from `queries/` dir:
   - `queries/c.scm`, `queries/rust.scm`, `queries/go.scm`, etc.
   - Each defines patterns for all definition types that language supports
   - Use custom captures like `@definition.struct`, `@definition.enum_value`, `@definition.field`, `@definition.constant`, `@visibility`, `@container`

4. **Keep occurrence collection** unchanged — the existing `collect_occurrences()` in `common.rs` walks the AST for identifier nodes. This is simple and fast. No need to use queries for this.

5. **Mapping captures to Symbol fields**:
   - `@name` → `symbol.name` (text of captured node)
   - `@definition.function` → `SymbolKind::Function, def_keyword: "function"`
   - `@definition.method` → `SymbolKind::Method, def_keyword: "method"`
   - `@definition.class` → `SymbolKind::Class, def_keyword: "class"`
   - `@definition.struct` → `SymbolKind::Struct, def_keyword: "struct"`
   - `@definition.enum` → `SymbolKind::Enum, def_keyword: "enum"`
   - `@definition.constant` → `SymbolKind::Constant, def_keyword: "enum"` or `"const"` or `"define"`
   - `@definition.interface` → `SymbolKind::Interface, def_keyword: "interface"`
   - `@definition.type` → `SymbolKind::TypeAlias, def_keyword: "typedef"` or `"type"`
   - `@definition.module` → `SymbolKind::Module, def_keyword: "mod"` or `"module"`
   - `@definition.trait` → `SymbolKind::Trait, def_keyword: "trait"`
   - `@definition.field` → `SymbolKind::Variable, def_keyword: "field"`, depth: 1
   - `@definition.variable` → `SymbolKind::Variable, def_keyword: "variable"`
   - `@definition.parameter` → `SymbolKind::Variable, def_keyword: "parameter"`, depth: 1

### Constraints

- **All existing tests must pass** — there are 118 lib unit tests and 42 integration tests in `tests/c_integration.rs` that validate hover, go-to-definition, find-references, and signature-help at specific cursor positions. The `@mark TAG` comments in `tests/fixtures/c_project/` anchor cursor positions.
- **The `Symbol` struct and `ParseResult` stay the same** — the query-based parser must produce the same data types.
- **Performance matters** — quicklsp indexes the Linux kernel (64K files) in ~3 seconds. Query execution should be comparable to manual walking.
- **`try_parse()` and `language_for_extension()` API stays the same** — callers in `workspace.rs` and `syntax_cache.rs` don't change.

### Files to modify
- `src/parsing/tree_sitter_parse/common.rs` — add query execution infrastructure
- `src/parsing/tree_sitter_parse/{c,cpp,rust,go,python,javascript,typescript,java,ruby}.rs` — replace manual walking with query-based extraction
- `src/parsing/tree_sitter_parse.rs` — may need updates to mod structure

### Files NOT to modify
- `src/parsing/symbols.rs` — Symbol struct stays the same
- `src/parsing/tokenizer.rs` — Occurrence struct stays the same  
- `src/workspace.rs` — dispatch logic stays the same
- `src/syntax_cache.rs` — AST caching stays the same
- `tests/c_integration.rs` — tests must pass as-is

### Example of what a query-based C parser might look like

```rust
// c.rs — after refactor
const C_QUERY: &str = r#"
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)
) @definition.function

(struct_specifier
  name: (type_identifier) @name
  body: (_)
) @definition.struct

(enum_specifier
  name: (type_identifier) @name
) @definition.enum

(enum_specifier
  body: (enumerator_list
    (enumerator name: (identifier) @name) @definition.constant))

(type_definition
  declarator: (type_identifier) @name
) @definition.type

(preproc_def
  name: (identifier) @name
) @definition.constant

(preproc_function_def
  name: (identifier) @name
) @definition.constant

; Fields inside struct/union bodies
(field_declaration
  declarator: (field_identifier) @name
) @definition.field
"#;

const C_IDENT_KINDS: &[&str] = &["identifier", "type_identifier", "field_identifier"];

pub struct CParser;

impl TsParser for CParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_c::LANGUAGE.into();
        common::run_query_parse(source, &lang, C_QUERY, C_IDENT_KINDS, c_post_process)
    }
}

// Optional per-language post-processing for visibility, container inference, etc.
fn c_post_process(symbols: &mut Vec<Symbol>, source: &str) {
    // Mark static functions as Private, etc.
}
```

And in `common.rs`:
```rust
pub fn run_query_parse<F>(
    source: &str,
    language: &tree_sitter::Language,
    query_source: &str,
    identifier_kinds: &[&str],
    post_process: F,
) -> ParseResult
where F: FnOnce(&mut Vec<Symbol>, &str)
{
    let tree = parse_source(source, language)?;
    let query = Query::new(language, query_source)?;
    let mut cursor = QueryCursor::new();
    let matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    
    let mut symbols = Vec::new();
    for m in matches {
        // Extract @name capture → symbol name
        // Extract @definition.X capture → symbol kind
        // Determine depth, container from node parents
        // Push Symbol
    }
    
    post_process(&mut symbols, source);
    let occurrences = collect_occurrences(tree.root_node(), source, &symbols, identifier_kinds);
    ParseResult { symbols, occurrences }
}
```

### What success looks like
- Each language file shrinks from 200-600 lines to ~50-100 lines (query string + optional post-processing)
- `common.rs` gains ~100-150 lines of query execution infrastructure
- Total code reduction: ~3000 lines → ~1500 lines
- All 118 unit tests pass
- All 42 integration tests pass
- Performance comparable or better (queries are compiled and optimized by tree-sitter)

### Bonus: handle the `typedef enum`/`typedef struct` issue
The C parser currently has a special case for `typedef enum { A, B } Name` to extract enum values from anonymous enums inside typedefs, and similarly for typedef struct fields. The SCM query approach handles this naturally with patterns like:
```scheme
(type_definition
  type: (enum_specifier
    body: (enumerator_list
      (enumerator name: (identifier) @name) @definition.constant)))
```
