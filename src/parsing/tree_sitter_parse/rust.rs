//! Tree-sitter based Rust parser using SCM queries.
//!
//! Extracts: functions, structs, enums, traits, type aliases, constants,
//! statics, modules, impl methods, enum variants, and struct fields.

use tree_sitter::Node;

use crate::parsing::symbols::SymbolKind;
use crate::parsing::tokenizer::Visibility;

use super::common::{self, find_child_by_kind, node_text, QueryParseConfig};
use super::{ParseResult, TsParser};

const RUST_IDENT_KINDS: &[&str] = &["identifier", "type_identifier", "field_identifier"];

/// SCM query for Rust symbol extraction.
///
/// Captures:
///   @name          — the identifier node (symbol name + position)
///   @definition.*  — the definition node (determines SymbolKind)
///   @container     — parent type/class name for contained symbols
const RUST_QUERY: &str = r#"
; ── Top-level functions ──────────────────────────────────────────────────
; Match functions NOT inside declaration_list (top-level or in mod bodies)
(function_item
  name: (identifier) @name) @definition.function

; ── Impl methods ─────────────────────────────────────────────────────────
(impl_item
  type: (_) @container
  body: (declaration_list
    (function_item
      name: (identifier) @name) @definition.method))

; ── Impl constants ───────────────────────────────────────────────────────
(impl_item
  type: (_) @container
  body: (declaration_list
    (const_item
      name: (identifier) @name) @definition.constant))

; ── Trait methods (with body) ────────────────────────────────────────────
(trait_item
  name: (type_identifier) @container
  body: (declaration_list
    (function_item
      name: (identifier) @name) @definition.method))

; ── Trait method signatures (no body) ────────────────────────────────────
(trait_item
  name: (type_identifier) @container
  body: (declaration_list
    (function_signature_item
      name: (identifier) @name) @definition.method))

; ── Structs ──────────────────────────────────────────────────────────────
(struct_item
  name: (type_identifier) @name) @definition.struct

; ── Struct fields ────────────────────────────────────────────────────────
(struct_item
  name: (type_identifier) @container
  body: (field_declaration_list
    (field_declaration
      name: (field_identifier) @name) @definition.field))

; ── Enums ────────────────────────────────────────────────────────────────
(enum_item
  name: (type_identifier) @name) @definition.enum

; ── Enum variants ────────────────────────────────────────────────────────
(enum_item
  name: (type_identifier) @container
  body: (enum_variant_list
    (enum_variant
      name: (identifier) @name) @definition.variant))

; ── Traits ───────────────────────────────────────────────────────────────
(trait_item
  name: (type_identifier) @name) @definition.trait

; ── Type aliases ─────────────────────────────────────────────────────────
(type_item
  name: (type_identifier) @name) @definition.type

; ── Top-level constants ──────────────────────────────────────────────────
(const_item
  name: (identifier) @name) @definition.constant

; ── Statics ──────────────────────────────────────────────────────────────
(static_item
  name: (identifier) @name) @definition.variable

; ── Modules ──────────────────────────────────────────────────────────────
(mod_item
  name: (identifier) @name) @definition.module

; ── Macros ───────────────────────────────────────────────────────────────
(macro_definition
  name: (identifier) @name) @definition.macro

; ── Function parameters ──────────────────────────────────────────────────
; Simple `x: Ty` pattern
(parameter
  pattern: (identifier) @name) @definition.parameter

; `mut x: Ty` pattern
(parameter
  pattern: (mut_pattern (identifier) @name)) @definition.parameter

; `&x: …` / `&mut x: …`
(parameter
  pattern: (reference_pattern (identifier) @name)) @definition.parameter

; ── let bindings ─────────────────────────────────────────────────────────
; `let x = …`
(let_declaration
  pattern: (identifier) @name) @definition.local

; `let mut x = …`
(let_declaration
  pattern: (mut_pattern (identifier) @name)) @definition.local

; `let (a, b, …) = …` — matches every identifier in the tuple pattern
(let_declaration
  pattern: (tuple_pattern (identifier) @name)) @definition.local

; `let (mut a, mut b, …) = …`
(let_declaration
  pattern: (tuple_pattern (mut_pattern (identifier) @name))) @definition.local

; `let Foo { x, y } = …` — shorthand field bindings
(let_declaration
  pattern: (struct_pattern
    (field_pattern (identifier) @name))) @definition.local

; `let &x = …`
(let_declaration
  pattern: (reference_pattern (identifier) @name)) @definition.local

; ── for loop bindings ────────────────────────────────────────────────────
(for_expression
  pattern: (identifier) @name) @definition.local

(for_expression
  pattern: (mut_pattern (identifier) @name)) @definition.local

(for_expression
  pattern: (tuple_pattern (identifier) @name)) @definition.local

; ── closures ─────────────────────────────────────────────────────────────
(closure_parameters
  (identifier) @name) @definition.parameter

(closure_parameters
  (parameter
    pattern: (identifier) @name)) @definition.parameter
"#;

pub struct RustParser;

/// Rust nodes that introduce a new function/method scope.
const RUST_SCOPE_KINDS: &[&str] = &[
    "function_item",
    "function_signature_item",
    "closure_expression",
];

impl TsParser for RustParser {
    fn parse(source: &str) -> ParseResult {
        common::run_query_parse(
            source,
            &QueryParseConfig {
                language: tree_sitter_rust::LANGUAGE.into(),
                query_source: RUST_QUERY,
                identifier_kinds: RUST_IDENT_KINDS,
                scope_kinds: RUST_SCOPE_KINDS,
                def_keyword: rust_def_keyword,
                visibility: rust_visibility,
                post_process: None,
            },
        )
    }
}

// Locals and parameters are captured via SCM rules (`@definition.local`
// and `@definition.parameter`) in `RUST_QUERY`. The shared query engine
// in `common::run_query_parse` walks up to the nearest scope node (see
// `RUST_SCOPE_KINDS`) to compute depth / scope_end_line / container
// automatically, so no per-language walker is needed.

fn rust_def_keyword(kind: SymbolKind, suffix: &str) -> &'static str {
    match suffix {
        "function" => "fn",
        "method" => "method",
        "struct" => "struct",
        "enum" => "enum",
        "trait" => "trait",
        "type" => "type",
        "constant" => "const",
        "variable" => "static",
        "module" => "mod",
        "macro" => "macro",
        "field" => "field",
        "variant" => "variant",
        _ => common::default_def_keyword(kind, suffix),
    }
}

fn rust_visibility(node: Node, source: &str) -> Visibility {
    if let Some(vis) = find_child_by_kind(node, "visibility_modifier") {
        let text = node_text(vis, source);
        if text.starts_with("pub") {
            Visibility::Public
        } else {
            Visibility::Unknown
        }
    } else {
        Visibility::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsing::symbols::SymbolKind;

    #[test]
    fn test_rust_parser_basic() {
        let source = r#"
pub fn hello(x: i32) -> i32 {
    x + 1
}

pub struct Config {
    pub name: String,
    value: i32,
}

enum Status {
    Active,
    Inactive,
}

pub trait Handler {
    fn handle(&self);
}

type Result<T> = std::result::Result<T, Error>;

const MAX: usize = 100;

static GLOBAL: i32 = 42;

pub mod utils {
    pub fn helper() {}
}

impl Config {
    pub fn new() -> Self {
        Config { name: String::new(), value: 0 }
    }
}

macro_rules! my_macro {
    () => {};
}
"#;
        let result = RustParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(
            names.contains(&"hello"),
            "should find fn hello, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Config"),
            "should find struct Config, got: {:?}",
            names
        );
        assert!(
            names.contains(&"name"),
            "should find field name, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Status"),
            "should find enum Status, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Active"),
            "should find variant Active, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Handler"),
            "should find trait Handler, got: {:?}",
            names
        );
        assert!(
            names.contains(&"handle"),
            "should find trait method handle, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Result"),
            "should find type alias Result, got: {:?}",
            names
        );
        assert!(
            names.contains(&"MAX"),
            "should find const MAX, got: {:?}",
            names
        );
        assert!(
            names.contains(&"GLOBAL"),
            "should find static GLOBAL, got: {:?}",
            names
        );
        assert!(
            names.contains(&"utils"),
            "should find mod utils, got: {:?}",
            names
        );
        assert!(
            names.contains(&"helper"),
            "should find nested fn helper, got: {:?}",
            names
        );
        assert!(
            names.contains(&"new"),
            "should find impl method new, got: {:?}",
            names
        );
        assert!(
            names.contains(&"my_macro"),
            "should find macro my_macro, got: {:?}",
            names
        );

        // Check visibility
        let hello_sym = result.symbols.iter().find(|s| s.name == "hello").unwrap();
        assert_eq!(hello_sym.visibility, Visibility::Public);

        // Check impl method container
        let new_sym = result.symbols.iter().find(|s| s.name == "new").unwrap();
        assert_eq!(new_sym.container.as_deref(), Some("Config"));

        assert!(!result.occurrences.is_empty(), "should have occurrences");
    }

    #[test]
    fn test_rust_parser_fixture() {
        let source = include_str!("../../../tests/fixtures/sample_rust.rs");
        let result = RustParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        // Constants
        assert!(names.contains(&"MAX_RETRIES"));
        assert!(names.contains(&"DEFAULT_TIMEOUT"));
        assert!(names.contains(&"FINAL_STATUS"));

        // Structs
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"Request"));
        assert!(names.contains(&"Response"));
        assert!(names.contains(&"Server"));

        // Struct fields
        assert!(names.contains(&"host"));
        assert!(names.contains(&"port"));
        assert!(names.contains(&"method"));

        // Enum
        assert!(names.contains(&"Status"));
        assert!(names.contains(&"Active"));
        assert!(names.contains(&"Inactive"));

        // Trait
        assert!(names.contains(&"Handler"));

        // Functions
        assert!(names.contains(&"create_config"));
        assert!(names.contains(&"process_request"));
        assert!(names.contains(&"validate_request"));

        // Impl methods
        let new_sym = result
            .symbols
            .iter()
            .find(|s| s.name == "new" && s.container.as_deref() == Some("Server"))
            .unwrap();
        assert_eq!(new_sym.kind, SymbolKind::Method);

        let add_handler = result
            .symbols
            .iter()
            .find(|s| s.name == "add_handler")
            .unwrap();
        assert_eq!(add_handler.container.as_deref(), Some("Server"));

        // Module
        assert!(names.contains(&"utils"));

        // Type aliases
        assert!(names.contains(&"StatusCode"));
        assert!(names.contains(&"HandlerResult"));

        // Static
        assert!(names.contains(&"GLOBAL_COUNTER"));

        // Unicode identifiers
        assert!(names.contains(&"données_utilisateur"));
        assert!(names.contains(&"Über"));
    }

    #[test]
    fn test_rust_empty_file() {
        let result = RustParser::parse("");
        assert!(result.symbols.is_empty());
        assert!(result.occurrences.is_empty());
    }

    #[test]
    fn test_rust_comments_only() {
        let result = RustParser::parse("// just a comment\n/* block comment */\n");
        assert!(result.symbols.is_empty());
    }

    #[test]
    fn test_rust_nested_impl_blocks() {
        let source = r#"
struct A;
struct B;

impl A {
    fn foo(&self) {}
    fn bar(&self) {}
}

impl B {
    fn foo(&self) {}
    fn baz(&self) {}
}
"#;
        let result = RustParser::parse(source);
        let a_foo = result
            .symbols
            .iter()
            .find(|s| s.name == "foo" && s.container.as_deref() == Some("A"));
        let b_foo = result
            .symbols
            .iter()
            .find(|s| s.name == "foo" && s.container.as_deref() == Some("B"));
        assert!(a_foo.is_some(), "should find A::foo");
        assert!(b_foo.is_some(), "should find B::foo");
    }

    #[test]
    fn test_rust_visibility_variants() {
        let source = r#"
pub fn public_fn() {}
fn private_fn() {}
pub(crate) fn crate_fn() {}
pub struct PubStruct;
struct PrivStruct;
"#;
        let result = RustParser::parse(source);
        let pub_fn = result
            .symbols
            .iter()
            .find(|s| s.name == "public_fn")
            .unwrap();
        assert_eq!(pub_fn.visibility, Visibility::Public);

        let priv_fn = result
            .symbols
            .iter()
            .find(|s| s.name == "private_fn")
            .unwrap();
        assert_eq!(priv_fn.visibility, Visibility::Unknown);

        let crate_fn = result
            .symbols
            .iter()
            .find(|s| s.name == "crate_fn")
            .unwrap();
        assert_eq!(crate_fn.visibility, Visibility::Public);
    }
}
