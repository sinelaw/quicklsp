//! Tree-sitter based Java parser using SCM queries.
//!
//! Extracts: classes, interfaces, enums, methods, constructors, fields,
//! and enum constants.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{self, make_contained_symbol, node_text, QueryParseConfig};
use super::{ParseResult, TsParser};

const JAVA_IDENT_KINDS: &[&str] = &["identifier", "type_identifier"];

const JAVA_QUERY: &str = r#"
; ── Class declarations ───────────────────────────────────────────────────
(class_declaration
  name: (identifier) @name) @definition.class

; ── Interface declarations ───────────────────────────────────────────────
(interface_declaration
  name: (identifier) @name) @definition.interface

; ── Enum declarations ────────────────────────────────────────────────────
(enum_declaration
  name: (identifier) @name) @definition.enum

; ── Record declarations ──────────────────────────────────────────────────
(record_declaration
  name: (identifier) @name) @definition.struct

; ── Annotation declarations ──────────────────────────────────────────────
(annotation_type_declaration
  name: (identifier) @name) @definition.interface

; ── Methods inside classes ───────────────────────────────────────────────
(class_declaration
  name: (identifier) @container
  body: (class_body
    (method_declaration
      name: (identifier) @name) @definition.method))

; ── Constructors inside classes ──────────────────────────────────────────
(class_declaration
  name: (identifier) @container
  body: (class_body
    (constructor_declaration
      name: (identifier) @name) @definition.constructor))

; ── Fields inside classes ────────────────────────────────────────────────
(class_declaration
  name: (identifier) @container
  body: (class_body
    (field_declaration
      declarator: (variable_declarator
        name: (identifier) @name)) @definition.field))

; ── Methods inside interfaces ────────────────────────────────────────────
(interface_declaration
  name: (identifier) @container
  body: (interface_body
    (method_declaration
      name: (identifier) @name) @definition.method))

; ── Enum constants ───────────────────────────────────────────────────────
(enum_declaration
  name: (identifier) @container
  body: (enum_body
    (enum_constant
      name: (identifier) @name) @definition.constant))

; ── Methods inside enums ─────────────────────────────────────────────────
(enum_declaration
  name: (identifier) @container
  body: (enum_body
    (enum_body_declarations
      (method_declaration
        name: (identifier) @name) @definition.method)))
"#;

pub struct JavaParser;

impl TsParser for JavaParser {
    fn parse(source: &str) -> ParseResult {
        common::run_query_parse(
            source,
            &QueryParseConfig {
                language: tree_sitter_java::LANGUAGE.into(),
                query_source: JAVA_QUERY,
                identifier_kinds: JAVA_IDENT_KINDS,
                def_keyword: java_def_keyword,
                visibility: java_visibility,
                post_process: Some(java_post_process),
            },
        )
    }
}

/// Post-process: extract Java method/constructor parameters and
/// `local_variable_declaration` bindings inside method bodies.
fn java_post_process(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    walk_java(root, source, symbols);
}

fn walk_java(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    match node.kind() {
        "method_declaration" | "constructor_declaration" => {
            let name_text = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string());
            let body_end = node
                .child_by_field_name("body")
                .map(|b| b.end_position().row)
                .unwrap_or_else(|| node.end_position().row);

            if let Some(params) = node.child_by_field_name("parameters") {
                extract_java_params(params, source, symbols, name_text.as_deref(), 1, body_end);
            }
            if let Some(body) = node.child_by_field_name("body") {
                mark_java_locals_in_body(body, source, symbols, name_text.as_deref(), 1, body_end);
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_java(child, source, symbols);
    }
}

fn extract_java_params(
    params: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    fn_name: Option<&str>,
    depth: usize,
    scope_end: usize,
) {
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        if child.kind() == "formal_parameter" || child.kind() == "spread_parameter" {
            if let Some(name_node) = child.child_by_field_name("name") {
                push_java_local(
                    name_node,
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

fn mark_java_locals_in_body(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
    depth: usize,
    scope_end: usize,
) {
    // Don't recurse into nested method/constructor bodies — they have
    // their own scope handled by `walk_java`.
    if matches!(
        node.kind(),
        "method_declaration"
            | "constructor_declaration"
            | "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
    ) {
        return;
    }

    if node.kind() == "local_variable_declaration" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "variable_declarator" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    push_java_local(
                        name_node, source, symbols, container, depth, scope_end, "variable",
                    );
                }
            }
        }
    }

    // `enhanced_for_statement` binds a loop variable.
    if node.kind() == "enhanced_for_statement" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let body_end = node
                .child_by_field_name("body")
                .map(|b| b.end_position().row)
                .unwrap_or(scope_end);
            push_java_local(
                name_node,
                source,
                symbols,
                container,
                depth + 1,
                body_end,
                "variable",
            );
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        mark_java_locals_in_body(child, source, symbols, container, depth, scope_end);
    }
}

fn push_java_local(
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
        Visibility::Private,
        container,
        depth,
        Some(scope_end),
        None,
    ));
}

fn java_def_keyword(_kind: SymbolKind, suffix: &str) -> &'static str {
    match suffix {
        "function" => "function",
        "method" => "method",
        "class" => "class",
        "interface" => "interface",
        "enum" => "enum",
        "struct" => "record",
        "constant" => "enum",
        "variable" | "field" => "field",
        "constructor" => "constructor",
        _ => "class",
    }
}

fn java_visibility(node: Node, source: &str) -> Visibility {
    // Look for modifiers child
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let text = node_text(child, source);
            if text.contains("public") {
                return Visibility::Public;
            } else if text.contains("private") {
                return Visibility::Private;
            }
            // protected and package-private → Unknown
            return Visibility::Unknown;
        }
    }
    Visibility::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_java_parser_basic() {
        let source = r#"
public class Config {
    private String name;
    public int value;

    public Config(String name) {
        this.name = name;
    }

    public String getName() {
        return this.name;
    }

    private void helper() {}
}

interface Handler {
    void handle(String data);
}

enum Status {
    ACTIVE,
    INACTIVE;

    public String label() {
        return name().toLowerCase();
    }
}

record Point(int x, int y) {}
"#;
        let result = JavaParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(
            names.contains(&"Config"),
            "should find class Config, got: {:?}",
            names
        );
        assert!(
            names.contains(&"name"),
            "should find field name, got: {:?}",
            names
        );
        assert!(
            names.contains(&"getName"),
            "should find method getName, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Handler"),
            "should find interface Handler, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Status"),
            "should find enum Status, got: {:?}",
            names
        );
        assert!(
            names.contains(&"ACTIVE"),
            "should find enum constant ACTIVE, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Point"),
            "should find record Point, got: {:?}",
            names
        );

        // Check visibility
        let config_sym = result.symbols.iter().find(|s| s.name == "Config").unwrap();
        assert_eq!(config_sym.visibility, Visibility::Public);

        let helper_sym = result.symbols.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(helper_sym.visibility, Visibility::Private);

        // Check method container
        let get_name = result.symbols.iter().find(|s| s.name == "getName").unwrap();
        assert_eq!(get_name.container.as_deref(), Some("Config"));

        // Check enum constant container
        let active = result.symbols.iter().find(|s| s.name == "ACTIVE").unwrap();
        assert_eq!(active.container.as_deref(), Some("Status"));

        assert!(!result.occurrences.is_empty());
    }
}
