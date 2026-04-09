//! Tree-sitter based JavaScript parser using SCM queries.
//!
//! Extracts: functions, classes, methods, variable declarations (const/let/var),
//! and export statements.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{self, node_text, QueryParseConfig};
use super::{ParseResult, TsParser};

const JS_IDENT_KINDS: &[&str] = &["identifier", "property_identifier"];

const JS_QUERY: &str = r#"
; ── Function declarations ────────────────────────────────────────────────
(function_declaration
  name: (identifier) @name) @definition.function

; ── Generator functions ──────────────────────────────────────────────────
(generator_function_declaration
  name: (identifier) @name) @definition.function

; ── Class declarations ───────────────────────────────────────────────────
(class_declaration
  name: (identifier) @name) @definition.class

; ── Methods inside classes ───────────────────────────────────────────────
(class_declaration
  name: (identifier) @container
  body: (class_body
    (method_definition
      name: (property_identifier) @name) @definition.method))

; ── Variable declarations (const = Constant, let/var = Variable) ─────────
(lexical_declaration
  (variable_declarator
    name: (identifier) @name) @definition.variable)

(variable_declaration
  (variable_declarator
    name: (identifier) @name) @definition.variable)

; ── Arrow function assigned to const/let ─────────────────────────────────
(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function)) @definition.function)

(variable_declaration
  (variable_declarator
    name: (identifier) @name
    value: (arrow_function)) @definition.function)

; ── Function expression assigned to variable ─────────────────────────────
(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: (function_expression)) @definition.function)

(variable_declaration
  (variable_declarator
    name: (identifier) @name
    value: (function_expression)) @definition.function)

; ── Exported function declarations ───────────────────────────────────────
(export_statement
  (function_declaration
    name: (identifier) @name) @definition.function)

; ── Exported class declarations ──────────────────────────────────────────
(export_statement
  (class_declaration
    name: (identifier) @name) @definition.class)

; ── Exported variable declarations ───────────────────────────────────────
(export_statement
  (lexical_declaration
    (variable_declarator
      name: (identifier) @name) @definition.variable))

; ── Function parameters ──────────────────────────────────────────────────
(formal_parameters
  (identifier) @name) @definition.parameter

(formal_parameters
  (assignment_pattern
    left: (identifier) @name)) @definition.parameter

(formal_parameters
  (rest_pattern
    (identifier) @name)) @definition.parameter

(arrow_function
  parameter: (identifier) @name) @definition.parameter

; ── Local variable declarations (let / const / var) ────────────────────
; These match ANYWHERE in the tree — the shared engine only promotes the
; symbol to a local when the match is inside a function scope (otherwise
; it stays as a top-level global handled by the existing captures above).
(variable_declarator
  name: (identifier) @name) @definition.local

"#;

pub struct JsParser;

/// JS nodes that introduce a new function scope. Shared with TypeScript.
pub(super) const JS_SCOPE_KINDS: &[&str] = &[
    "function_declaration",
    "function_expression",
    "function",
    "arrow_function",
    "method_definition",
    "generator_function_declaration",
    "generator_function",
];

impl TsParser for JsParser {
    fn parse(source: &str) -> ParseResult {
        common::run_query_parse(
            source,
            &QueryParseConfig {
                language: tree_sitter_javascript::LANGUAGE.into(),
                query_source: JS_QUERY,
                identifier_kinds: JS_IDENT_KINDS,
                scope_kinds: JS_SCOPE_KINDS,
                def_keyword: js_def_keyword,
                visibility: |_node: Node, _source: &str| Visibility::Unknown,
                post_process: Some(js_post_process),
            },
        )
    }
}

fn js_def_keyword(_kind: SymbolKind, suffix: &str) -> &'static str {
    match suffix {
        "function" => "function",
        "method" => "method",
        "class" => "class",
        "variable" => "variable",
        "constant" => "const",
        _ => "function",
    }
}

/// Post-process: detect exports and const vs let/var.
/// Locals / parameters are handled declaratively via `@definition.local`
/// and `@definition.parameter` SCM rules in `JS_QUERY`.
fn js_post_process(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    detect_exports_and_const(root, source, symbols);
}

fn detect_exports_and_const(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    // Walk tree to find export_statement nodes and const declarations
    walk_for_export_and_const(root, source, symbols);
}

fn walk_for_export_and_const(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    match node.kind() {
        "export_statement" => {
            // Mark all symbols defined within this export as Public
            mark_children_exported(node, symbols);
        }
        "lexical_declaration" => {
            // Check if it's a const declaration
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "const" || node_text(child, source) == "const" {
                    mark_const_declarations(node, symbols);
                    break;
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_for_export_and_const(child, source, symbols);
    }
}

fn mark_children_exported(node: Node, symbols: &mut Vec<Symbol>) {
    let start_line = node.start_position().row;
    let end_line = node.end_position().row;
    for sym in symbols.iter_mut() {
        if sym.line >= start_line && sym.line <= end_line {
            sym.visibility = Visibility::Public;
        }
    }
}

fn mark_const_declarations(node: Node, symbols: &mut Vec<Symbol>) {
    let start_line = node.start_position().row;
    let end_line = node.end_position().row;
    for sym in symbols.iter_mut() {
        if sym.line >= start_line && sym.line <= end_line && sym.kind == SymbolKind::Variable {
            sym.kind = SymbolKind::Constant;
            sym.def_keyword = "const".to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_js_parser_basic() {
        let source = r#"
function hello(name) {
    return "Hello, " + name;
}

const MAX_SIZE = 100;
let counter = 0;

class Config {
    constructor(name) {
        this.name = name;
    }

    getName() {
        return this.name;
    }
}

export function exported() {}

const handler = (x) => x + 1;
"#;
        let result = JsParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(
            names.contains(&"hello"),
            "should find function hello, got: {:?}",
            names
        );
        assert!(
            names.contains(&"MAX_SIZE"),
            "should find const MAX_SIZE, got: {:?}",
            names
        );
        assert!(
            names.contains(&"counter"),
            "should find let counter, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Config"),
            "should find class Config, got: {:?}",
            names
        );
        assert!(
            names.contains(&"getName"),
            "should find method getName, got: {:?}",
            names
        );
        assert!(
            names.contains(&"exported"),
            "should find exported function, got: {:?}",
            names
        );
        assert!(
            names.contains(&"handler"),
            "should find arrow function handler, got: {:?}",
            names
        );

        // Check exported visibility
        let exported_sym = result
            .symbols
            .iter()
            .find(|s| s.name == "exported")
            .unwrap();
        assert_eq!(exported_sym.visibility, Visibility::Public);

        // Check method container
        let get_name = result.symbols.iter().find(|s| s.name == "getName").unwrap();
        assert_eq!(get_name.container.as_deref(), Some("Config"));

        assert!(!result.occurrences.is_empty());
    }
}
