# Tree-sitter Integration Plan

## Problem

The hand-written tokenizer can't parse C definitions correctly. It only recognizes keywords like `struct`/`enum`/`typedef` followed by an identifier. This misses:

- **C functions**: `void foo()`, `static int bar()` — return types aren't def_keywords
- **#define constants**: `#define MAX 3` — preprocessor not handled
- **Enum values**: `STATUS_ACTIVE` inside `enum { ... }` — only the enum name is indexed
- **Typedef aliases**: `typedef unsigned int Foo` — grabs `unsigned` not `Foo`

All 4 bugs were found testing quicklsp on the Linux kernel in vim.

## Solution

Replace the hand-written tokenizer with tree-sitter for C. Tree-sitter produces a real AST — a single walk extracts both accurate definitions and all identifier occurrences. No double scan needed.

For languages without tree-sitter grammars yet, the hand-written tokenizer remains as fallback. Adding a new language = add a grammar crate + implement a trait. Once all languages have tree-sitter grammars, the hand-written tokenizer can be deleted.

## Architecture

```
C source code
    └─ tree-sitter parse → AST (single pass)
        ├─ definition nodes → Vec<Symbol>      (accurate: functions, structs, defines, etc.)
        └─ all identifier nodes → Vec<Occurrence>  (every identifier for word index)

Other languages (until tree-sitter grammar added)
    └─ hand-written tokenizer (existing, unchanged)
        ├─ tokens → Vec<Symbol>
        └─ occurrences → Vec<Occurrence>
```

### Trait-based design

```rust
// src/parsing/tree_sitter_parse.rs

pub struct ParseResult {
    pub symbols: Vec<Symbol>,
    pub occurrences: Vec<Occurrence>,
}

/// Per-language tree-sitter parser. Adding a language = implement this trait.
pub trait TsParser {
    fn parse(source: &str) -> ParseResult;
}
```

```rust
// src/parsing/tree_sitter_parse/c.rs
pub struct CParser;
impl TsParser for CParser { ... }
```

### What CParser extracts

#### Definitions (-> Symbol)

| C construct | tree-sitter node type | SymbolKind |
|-------------|----------------------|------------|
| `void foo() { }` | `function_definition` -> declarator name | Function |
| `static int bar()` | `function_definition` + `static` storage class | Function (visibility=Private) |
| `struct Config { }` | `struct_specifier` with body | Struct |
| `enum Status { A, B }` | `enum_specifier` | Enum |
| `A, B` inside enum | `enumerator` | Constant |
| `typedef int Foo` | `type_definition` -> `type_identifier` | TypeAlias |
| `#define MAX 3` | `preproc_def` | Constant |
| `int global;` | file-scope `declaration` | Variable |

#### Scope filtering

Tree-sitter AST tells us exactly what's at file scope vs inside a function body. Only index:
- File-scope definitions (structs, functions, globals, typedefs, defines)
- Enum values (inside enum specifiers)

Skip:
- Local variables inside function bodies (`compound_statement`)
- Function parameters (`parameter_list`)
- For-loop / block-scoped variables

This should significantly reduce the 2.55M definitions currently indexed for the Linux kernel — most are local variables and parameters that shouldn't appear in cross-file go-to-definition.

#### Occurrences (-> Occurrence)

Walk every `identifier` and `type_identifier` node in the AST. Each becomes an Occurrence with `(word_offset, word_len, line, col, role)`. Role = Definition if the node is part of a definition site, Reference otherwise.

### Integration point: workspace.rs `index_file_core`

```rust
let (symbols, occurrences) = match lang {
    Some(LangFamily::CLike) => {
        let result = tree_sitter_parse::c::CParser::parse(&source);
        let mut symbols = result.symbols;
        Symbol::enrich_from_source(&mut symbols, &source, lang.unwrap());
        (symbols, result.occurrences)
    }
    Some(l) => {
        // Existing hand-written tokenizer for other languages
        let (scan_result, def_contexts) = tokenizer::scan_with_contexts(&source, l);
        let symbols = Symbol::from_tokens_with_contexts(&scan_result.tokens, &def_contexts);
        (symbols, scan_result.occurrences)
    }
    None => (Vec::new(), Vec::new()),
};
```

## Files

**Create:**
- `src/parsing/tree_sitter_parse.rs` — trait definition + `pub mod c;`
- `src/parsing/tree_sitter_parse/c.rs` — CParser implementation

**Modify:**
- `Cargo.toml` — `tree-sitter` + `tree-sitter-c` dependencies
- `src/parsing/mod.rs` — `pub mod tree_sitter_parse;`
- `src/workspace.rs` — dispatch to tree-sitter for CLike

## Verification

1. 4 ignored C bug tests pass with `--ignored`:
   - `c_function_go_to_definition`
   - `c_define_go_to_definition`
   - `c_enum_values_go_to_definition`
   - `c_typedef_go_to_definition`
2. Existing tests for other languages still pass
3. Benchmark on full Linux kernel with `systemd-run --scope -p MemoryMax=7823M`
4. Vim test: `gd` on C functions/structs/defines in the kernel

## Future: adding more languages

Each language needs:
1. A grammar crate (e.g., `tree-sitter-rust`, `tree-sitter-python`)
2. An implementation of `TsParser` that maps language-specific AST nodes to `Symbol` + `Occurrence`
3. A match arm in `index_file_core` dispatching to the new parser

Once all languages have tree-sitter parsers, `src/parsing/tokenizer.rs` and `src/parsing/symbols.rs` can be deleted.
