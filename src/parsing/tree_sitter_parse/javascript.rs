//! Tree-sitter based JavaScript parser using SCM queries.
//!
//! Extracts: functions, classes, methods, variable declarations (const/let/var),
//! and export statements.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{self, make_contained_symbol, node_text, QueryParseConfig};
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
"#;

pub struct JsParser;

impl TsParser for JsParser {
    fn parse(source: &str) -> ParseResult {
        let mut result = common::run_query_parse(
            source,
            &QueryParseConfig {
                language: tree_sitter_javascript::LANGUAGE.into(),
                query_source: JS_QUERY,
                identifier_kinds: JS_IDENT_KINDS,
                def_keyword: js_def_keyword,
                visibility: |_node: Node, _source: &str| Visibility::Unknown,
                post_process: Some(js_post_process),
            },
        );
        result
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

/// Post-process: detect exports and const vs let/var, and extract
/// function parameters / mark in-function variable declarations as locals.
fn js_post_process(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    detect_exports_and_const(root, source, symbols);
    walk_js_for_fn_scopes(root, source, symbols);
}

/// Walk the tree to find function scopes. For each function, extract
/// its parameters and mark variable declarations in its body as locals
/// (depth ≥ 1 with a scope_end_line).
fn walk_js_for_fn_scopes(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    let is_fn = matches!(
        node.kind(),
        "function_declaration"
            | "function_expression"
            | "function"
            | "arrow_function"
            | "method_definition"
            | "generator_function_declaration"
            | "generator_function"
    );

    if is_fn {
        let fn_name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source).to_string());
        let body_end = node
            .child_by_field_name("body")
            .map(|b| b.end_position().row)
            .unwrap_or_else(|| node.end_position().row);

        if let Some(params) = node.child_by_field_name("parameters") {
            extract_js_params(params, source, symbols, fn_name.as_deref(), body_end);
        }
        if let Some(body) = node.child_by_field_name("body") {
            mark_js_locals_in_body(body, source, symbols, fn_name.as_deref(), 1, body_end);
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_js_for_fn_scopes(child, source, symbols);
    }
}

fn mark_js_locals_in_body(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
    depth: usize,
    scope_end: usize,
) {
    // Don't recurse into nested function scopes — they have their own walker pass.
    if matches!(
        node.kind(),
        "function_declaration"
            | "function_expression"
            | "function"
            | "arrow_function"
            | "method_definition"
            | "generator_function_declaration"
            | "generator_function"
    ) {
        return;
    }

    if node.kind() == "variable_declarator" {
        if let Some(name_node) = node.child_by_field_name("name") {
            // `name_node` may be a destructuring pattern; walk to find identifiers.
            mark_js_pattern_as_local(name_node, source, symbols, container, depth, scope_end);
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        mark_js_locals_in_body(child, source, symbols, container, depth, scope_end);
    }
}

/// Walk a JS binding pattern (identifier / object_pattern / array_pattern
/// / rest_pattern / assignment_pattern) and update every matching symbol
/// in `symbols` to be a local. Identifiers not yet in `symbols` are
/// appended (e.g. destructured names that the SCM query didn't catch).
fn mark_js_pattern_as_local(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
    depth: usize,
    scope_end: usize,
) {
    match node.kind() {
        "identifier" => {
            let line = node.start_position().row;
            let col = node.start_position().column;
            // Existing symbol? Promote to local in place.
            let mut found = false;
            for sym in symbols.iter_mut() {
                if sym.line == line && sym.col == col {
                    sym.depth = depth;
                    sym.container = container.map(String::from);
                    sym.scope_end_line = Some(scope_end);
                    sym.def_keyword = "variable".to_string();
                    sym.kind = SymbolKind::Variable;
                    found = true;
                    break;
                }
            }
            if !found {
                push_js_local_ident(
                    node, source, symbols, container, depth, scope_end, "variable",
                );
            }
        }
        // `property_identifier` appears in shorthand like `{ x }` — that's a binding.
        "shorthand_property_identifier_pattern" => {
            push_js_local_ident(
                node, source, symbols, container, depth, scope_end, "variable",
            );
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                mark_js_pattern_as_local(child, source, symbols, container, depth, scope_end);
            }
        }
    }
}

fn extract_js_params(
    params: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    fn_name: Option<&str>,
    scope_end: usize,
) {
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                push_js_local_ident(child, source, symbols, fn_name, 1, scope_end, "parameter");
            }
            "assignment_pattern" | "rest_pattern" | "array_pattern" | "object_pattern" => {
                collect_js_idents(child, source, symbols, fn_name, 1, scope_end, "parameter");
            }
            _ => {}
        }
    }
}

fn collect_js_idents(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
    depth: usize,
    scope_end: usize,
    def_keyword: &str,
) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            push_js_local_ident(
                node,
                source,
                symbols,
                container,
                depth,
                scope_end,
                def_keyword,
            );
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_js_idents(
                    child,
                    source,
                    symbols,
                    container,
                    depth,
                    scope_end,
                    def_keyword,
                );
            }
        }
    }
}

fn push_js_local_ident(
    ident: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
    depth: usize,
    scope_end: usize,
    def_keyword: &str,
) {
    let name = node_text(ident, source).to_string();
    if name.is_empty() {
        return;
    }
    let line = ident.start_position().row;
    let col = ident.start_position().column;
    if symbols.iter().any(|s| s.line == line && s.col == col) {
        return;
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
