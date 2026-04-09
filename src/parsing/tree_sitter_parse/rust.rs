//! Tree-sitter based Rust parser using SCM queries.
//!
//! Extracts: functions, structs, enums, traits, type aliases, constants,
//! statics, modules, impl methods, enum variants, and struct fields.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{self, find_child_by_kind, make_contained_symbol, node_text, QueryParseConfig};
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
"#;

pub struct RustParser;

impl TsParser for RustParser {
    fn parse(source: &str) -> ParseResult {
        common::run_query_parse(
            source,
            &QueryParseConfig {
                language: tree_sitter_rust::LANGUAGE.into(),
                query_source: RUST_QUERY,
                identifier_kinds: RUST_IDENT_KINDS,
                def_keyword: rust_def_keyword,
                visibility: rust_visibility,
                post_process: Some(rust_post_process),
            },
        )
    }
}

/// Post-process: extract Rust function parameters and `let` bindings as
/// local symbols so they can be found via `find_local_definition_at`.
///
/// The SCM query only captures item-level definitions (fn, struct, enum,
/// const, static, …) because those are scope-independent. Locals need
/// scope tracking (depth + scope_end_line) that's easier to compute by
/// walking the tree directly.
fn rust_post_process(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    walk_rust_tree(root, source, symbols, None, 0, None);
}

/// Recursively walk the Rust AST, entering function bodies and block
/// expressions to extract parameters and `let` bindings as local symbols.
///
/// * `container` – enclosing function/method name (for the Symbol's
///   `container` field).
/// * `depth` – current lexical depth. 0 = top-level, ≥1 = inside a fn.
/// * `scope_end` – end-line of the enclosing block (for scope-aware
///   resolution of shadowed names).
fn walk_rust_tree(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
    depth: usize,
    scope_end: Option<usize>,
) {
    match node.kind() {
        // New lexical scope — function or method definition.
        "function_item" | "function_signature_item" => {
            let name_text = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string());
            let body_end = node
                .child_by_field_name("body")
                .map(|b| b.end_position().row)
                .unwrap_or_else(|| node.end_position().row);

            if let Some(params) = node.child_by_field_name("parameters") {
                extract_rust_params(
                    params,
                    source,
                    symbols,
                    name_text.as_deref(),
                    depth + 1,
                    body_end,
                );
            }

            if let Some(body) = node.child_by_field_name("body") {
                let new_container = name_text.as_deref().or(container);
                // Walk the body's children directly rather than the body block
                // itself — otherwise the `block` branch below would double-
                // increment the depth for the outermost scope of the function.
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    walk_rust_tree(
                        child,
                        source,
                        symbols,
                        new_container,
                        depth + 1,
                        Some(body_end),
                    );
                }
            }
            return;
        }

        // Nested block expression — introduces a new lexical scope.
        "block" => {
            let block_end = node.end_position().row;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_rust_tree(
                    child,
                    source,
                    symbols,
                    container,
                    depth + 1,
                    Some(block_end),
                );
            }
            return;
        }

        // `let` binding — bind each identifier in the pattern.
        "let_declaration" => {
            if let Some(pat) = node.child_by_field_name("pattern") {
                // The binding is visible until the end of the enclosing scope,
                // not just the end of the let statement itself.
                let end = scope_end.unwrap_or_else(|| node.end_position().row);
                push_rust_pattern_bindings(pat, source, symbols, container, depth, end, "variable");
            }
            // Recurse into the value expression — it may contain closures or
            // nested blocks with more bindings.
            if let Some(value) = node.child_by_field_name("value") {
                walk_rust_tree(value, source, symbols, container, depth, scope_end);
            }
            return;
        }

        // `for pat in iter { body }` — pattern is scoped to the body.
        "for_expression" => {
            if let Some(val) = node.child_by_field_name("value") {
                walk_rust_tree(val, source, symbols, container, depth, scope_end);
            }
            if let Some(body) = node.child_by_field_name("body") {
                let body_end = body.end_position().row;
                if let Some(pat) = node.child_by_field_name("pattern") {
                    push_rust_pattern_bindings(
                        pat,
                        source,
                        symbols,
                        container,
                        depth + 1,
                        body_end,
                        "variable",
                    );
                }
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    walk_rust_tree(child, source, symbols, container, depth + 1, Some(body_end));
                }
            }
            return;
        }

        _ => {}
    }

    // Default: recurse into children with the same scope.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_rust_tree(child, source, symbols, container, depth, scope_end);
    }
}

/// Extract function parameter bindings from a `parameters` node.
fn extract_rust_params(
    params: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    fn_name: Option<&str>,
    depth: usize,
    scope_end: usize,
) {
    let mut cursor = params.walk();
    for param in params.children(&mut cursor) {
        // `self_parameter` and `variadic_parameter` don't introduce user
        // bindings, so we only care about `parameter`.
        if param.kind() == "parameter" {
            if let Some(pat) = param.child_by_field_name("pattern") {
                push_rust_pattern_bindings(
                    pat,
                    source,
                    symbols,
                    fn_name,
                    depth,
                    scope_end,
                    "parameter",
                );
            }
        }
    }
}

/// Walk a Rust pattern and push a local Symbol for each bound identifier.
fn push_rust_pattern_bindings(
    pat: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
    depth: usize,
    scope_end: usize,
    def_keyword: &str,
) {
    let mut idents = Vec::new();
    collect_rust_pattern_idents(pat, source, &mut idents);
    for (name, line, col) in idents {
        // Avoid duplicating a symbol the SCM query already added at the
        // same position (shouldn't happen for locals, but be safe).
        if symbols.iter().any(|s| s.line == line && s.col == col) {
            continue;
        }
        symbols.push(make_contained_symbol(
            name,
            SymbolKind::Variable,
            line,
            col,
            def_keyword,
            Visibility::Unknown,
            container,
            depth,
            Some(scope_end),
            None,
        ));
    }
}

/// Recursively collect identifier bindings from a Rust pattern.
///
/// Handles `identifier`, `mut_pattern`, `ref_pattern`, `reference_pattern`,
/// `tuple_pattern`, `tuple_struct_pattern`, `struct_pattern`, and
/// `captured_pattern` by walking into their children. `type_identifier`
/// and path-style identifiers (e.g., the `Some` in `Some(x)`) are skipped
/// because they're the matched constructor, not a binding.
fn collect_rust_pattern_idents(node: Node, source: &str, out: &mut Vec<(String, usize, usize)>) {
    match node.kind() {
        "identifier" => {
            let text = node_text(node, source);
            if !text.is_empty() && text != "_" {
                out.push((
                    text.to_string(),
                    node.start_position().row,
                    node.start_position().column,
                ));
            }
        }
        // Constructors / type names / paths inside patterns are not bindings.
        "type_identifier" | "scoped_identifier" | "scoped_type_identifier" | "field_identifier" => {
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_rust_pattern_idents(child, source, out);
            }
        }
    }
}

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
