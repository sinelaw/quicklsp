//! Tree-sitter based TypeScript parser using SCM queries.
//!
//! Extends JavaScript parsing with TypeScript-specific constructs:
//! interfaces, type aliases, enums, and type annotations.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{self, make_contained_symbol, node_text, QueryParseConfig};
use super::{ParseResult, TsParser};

const TS_IDENT_KINDS: &[&str] = &["identifier", "property_identifier", "type_identifier"];

const TS_QUERY: &str = r#"
; ── Function declarations ────────────────────────────────────────────────
(function_declaration
  name: (identifier) @name) @definition.function

; ── Generator functions ──────────────────────────────────────────────────
(generator_function_declaration
  name: (identifier) @name) @definition.function

; ── Class declarations ───────────────────────────────────────────────────
(class_declaration
  name: (type_identifier) @name) @definition.class

; ── Methods inside classes ───────────────────────────────────────────────
(class_declaration
  name: (type_identifier) @container
  body: (class_body
    (method_definition
      name: (property_identifier) @name) @definition.method))

; ── Class properties ─────────────────────────────────────────────────────
(class_declaration
  name: (type_identifier) @container
  body: (class_body
    (public_field_definition
      name: (property_identifier) @name) @definition.field))

; ── Interface declarations ───────────────────────────────────────────────
(interface_declaration
  name: (type_identifier) @name) @definition.interface

; ── Interface method signatures ──────────────────────────────────────────
(interface_declaration
  name: (type_identifier) @container
  body: (interface_body
    (method_signature
      name: (property_identifier) @name) @definition.method))

; ── Interface property signatures ────────────────────────────────────────
(interface_declaration
  name: (type_identifier) @container
  body: (interface_body
    (property_signature
      name: (property_identifier) @name) @definition.field))

; ── Type alias declarations ──────────────────────────────────────────────
(type_alias_declaration
  name: (type_identifier) @name) @definition.type

; ── Enum declarations ────────────────────────────────────────────────────
(enum_declaration
  name: (identifier) @name) @definition.enum

; ── Enum members ─────────────────────────────────────────────────────────
(enum_declaration
  name: (identifier) @container
  body: (enum_body
    (property_identifier) @name) @definition.variant)

; ── Variable declarations (const/let/var) ────────────────────────────────
(lexical_declaration
  (variable_declarator
    name: (identifier) @name) @definition.variable)

(variable_declaration
  (variable_declarator
    name: (identifier) @name) @definition.variable)

; ── Arrow function assigned to variable ──────────────────────────────────
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

; ── Exported function declarations ───────────────────────────────────────
(export_statement
  (function_declaration
    name: (identifier) @name) @definition.function)

; ── Exported class declarations ──────────────────────────────────────────
(export_statement
  (class_declaration
    name: (type_identifier) @name) @definition.class)

; ── Exported interface declarations ──────────────────────────────────────
(export_statement
  (interface_declaration
    name: (type_identifier) @name) @definition.interface)

; ── Exported type alias declarations ─────────────────────────────────────
(export_statement
  (type_alias_declaration
    name: (type_identifier) @name) @definition.type)

; ── Exported enum declarations ───────────────────────────────────────────
(export_statement
  (enum_declaration
    name: (identifier) @name) @definition.enum)

; ── Exported variable declarations ───────────────────────────────────────
(export_statement
  (lexical_declaration
    (variable_declarator
      name: (identifier) @name) @definition.variable))
"#;

pub struct TypeScriptParser;

impl TsParser for TypeScriptParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        parse_ts_common(source, lang)
    }
}

pub struct TsxParser;

impl TsParser for TsxParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TSX.into();
        parse_ts_common(source, lang)
    }
}

fn parse_ts_common(source: &str, language: tree_sitter::Language) -> ParseResult {
    let mut result = common::run_query_parse(
        source,
        &QueryParseConfig {
            language,
            query_source: TS_QUERY,
            identifier_kinds: TS_IDENT_KINDS,
            def_keyword: ts_def_keyword,
            visibility: |_node: Node, _source: &str| Visibility::Unknown,
            post_process: Some(ts_post_process),
        },
    );
    result
}

fn ts_def_keyword(_kind: SymbolKind, suffix: &str) -> &'static str {
    match suffix {
        "function" => "function",
        "method" => "method",
        "class" => "class",
        "interface" => "interface",
        "type" => "type",
        "enum" => "enum",
        "variable" | "field" => "variable",
        "variant" => "enum",
        "constant" => "const",
        _ => "function",
    }
}

/// Post-process: detect exports and const vs let/var, and extract
/// function parameters / promote in-function variable declarations
/// to locals (depth ≥ 1).
fn ts_post_process(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    walk_for_export_and_const(root, source, symbols);
    walk_ts_for_fn_scopes(root, source, symbols);
}

fn walk_ts_for_fn_scopes(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
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
            extract_ts_params(params, source, symbols, fn_name.as_deref(), body_end);
        }
        if let Some(body) = node.child_by_field_name("body") {
            mark_ts_locals_in_body(body, source, symbols, fn_name.as_deref(), 1, body_end);
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_ts_for_fn_scopes(child, source, symbols);
    }
}

fn mark_ts_locals_in_body(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
    depth: usize,
    scope_end: usize,
) {
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
            mark_ts_pattern_as_local(name_node, source, symbols, container, depth, scope_end);
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        mark_ts_locals_in_body(child, source, symbols, container, depth, scope_end);
    }
}

fn mark_ts_pattern_as_local(
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
                push_ts_local_ident(
                    node, source, symbols, container, depth, scope_end, "variable",
                );
            }
        }
        "shorthand_property_identifier_pattern" => {
            push_ts_local_ident(
                node, source, symbols, container, depth, scope_end, "variable",
            );
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                mark_ts_pattern_as_local(child, source, symbols, container, depth, scope_end);
            }
        }
    }
}

fn extract_ts_params(
    params: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    fn_name: Option<&str>,
    scope_end: usize,
) {
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        // TypeScript wraps params in `required_parameter` / `optional_parameter`
        // nodes, each containing a `pattern` field.
        match child.kind() {
            "identifier" => {
                push_ts_local_ident(child, source, symbols, fn_name, 1, scope_end, "parameter");
            }
            "required_parameter" | "optional_parameter" => {
                if let Some(pat) = child.child_by_field_name("pattern") {
                    collect_ts_idents(pat, source, symbols, fn_name, 1, scope_end, "parameter");
                } else {
                    collect_ts_idents(child, source, symbols, fn_name, 1, scope_end, "parameter");
                }
            }
            "assignment_pattern" | "rest_pattern" | "array_pattern" | "object_pattern" => {
                collect_ts_idents(child, source, symbols, fn_name, 1, scope_end, "parameter");
            }
            _ => {}
        }
    }
}

fn collect_ts_idents(
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
            push_ts_local_ident(
                node,
                source,
                symbols,
                container,
                depth,
                scope_end,
                def_keyword,
            );
        }
        // Skip type annotations — they're not bindings.
        "type_annotation" | "type_identifier" | "predefined_type" => {}
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_ts_idents(
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

fn push_ts_local_ident(
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

fn walk_for_export_and_const(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    match node.kind() {
        "export_statement" => {
            let start_line = node.start_position().row;
            let end_line = node.end_position().row;
            for sym in symbols.iter_mut() {
                if sym.line >= start_line && sym.line <= end_line {
                    sym.visibility = Visibility::Public;
                }
            }
        }
        "lexical_declaration" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if node_text(child, source) == "const" {
                    let start_line = node.start_position().row;
                    let end_line = node.end_position().row;
                    for sym in symbols.iter_mut() {
                        if sym.line >= start_line
                            && sym.line <= end_line
                            && sym.kind == SymbolKind::Variable
                        {
                            sym.kind = SymbolKind::Constant;
                            sym.def_keyword = "const".to_string();
                        }
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_typescript_parser_basic() {
        let source = r#"
function hello(name: string): string {
    return "Hello, " + name;
}

const MAX_SIZE: number = 100;

interface Handler {
    handle(data: string): void;
    name: string;
}

class Config {
    private value: number;

    constructor(public name: string) {}

    getName(): string {
        return this.name;
    }
}

type Result<T> = { ok: true; value: T } | { ok: false; error: string };

enum Status {
    Active,
    Inactive,
}

export function exported(): void {}

const handler = (x: number): number => x + 1;
"#;
        let result = TypeScriptParser::parse(source);
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
            names.contains(&"Handler"),
            "should find interface Handler, got: {:?}",
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
            names.contains(&"Result"),
            "should find type alias Result, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Status"),
            "should find enum Status, got: {:?}",
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

        // Check interface method container
        let handle_sym = result.symbols.iter().find(|s| s.name == "handle").unwrap();
        assert_eq!(handle_sym.container.as_deref(), Some("Handler"));

        assert!(!result.occurrences.is_empty());
    }

    #[test]
    fn test_typescript_parser_fixture() {
        let source = include_str!("../../../tests/fixtures/sample_typescript.ts");
        let result = TypeScriptParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        // Constants
        assert!(names.contains(&"MAX_RETRIES"), "got: {:?}", names);
        assert!(names.contains(&"DEFAULT_TIMEOUT"));

        // Interfaces
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"Handler"));

        // Type aliases
        assert!(names.contains(&"StatusCode"));
        assert!(names.contains(&"HandlerResult"));

        // Classes
        assert!(names.contains(&"Request"));
        assert!(names.contains(&"Response"));
        assert!(names.contains(&"Server"));

        // Functions
        assert!(names.contains(&"createConfig"));
        assert!(names.contains(&"processRequest"));
        assert!(names.contains(&"validateRequest"));

        // Enum
        assert!(names.contains(&"Status"));

        // Class methods with containers
        let add_handler = result.symbols.iter().find(|s| s.name == "addHandler");
        assert!(add_handler.is_some(), "should find Server.addHandler");
        if let Some(s) = add_handler {
            assert_eq!(s.container.as_deref(), Some("Server"));
        }

        // Variables
        assert!(names.contains(&"globalCounter"));
    }

    #[test]
    fn test_tsx_parser_basic() {
        let source = r#"
interface Props {
    name: string;
    onClick: () => void;
}

function Greeting(props: Props) {
    return props.name;
}

const App = () => {
    return "hello";
};

export default App;
"#;
        let result = TsxParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"Props"),
            "should find interface Props, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Greeting"),
            "should find function Greeting, got: {:?}",
            names
        );
        assert!(
            names.contains(&"App"),
            "should find const App, got: {:?}",
            names
        );
    }

    #[test]
    fn test_typescript_empty_file() {
        let result = TypeScriptParser::parse("");
        assert!(result.symbols.is_empty());
    }
}
