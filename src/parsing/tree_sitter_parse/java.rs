//! Tree-sitter based Java parser.
//!
//! Extracts: classes, interfaces, enums, methods, constructors, fields,
//! and enum constants.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{self, make_contained_symbol, make_symbol, node_text};
use super::{ParseResult, TsParser};

const JAVA_IDENT_KINDS: &[&str] = &["identifier", "type_identifier"];

pub struct JavaParser;

impl TsParser for JavaParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_java::LANGUAGE.into();
        common::run_parse(source, &lang, JAVA_IDENT_KINDS, collect_definitions)
    }
}

fn collect_definitions(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    collect_definitions_recursive(root, source, symbols, None);
}

fn collect_definitions_recursive(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_definition(child, source, symbols, container);
    }
}

fn extract_definition(node: Node, source: &str, symbols: &mut Vec<Symbol>, container: Option<&str>) {
    match node.kind() {
        "class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = java_visibility(node, source);
                symbols.push(make_symbol(
                    name.clone(), SymbolKind::Class,
                    name_node.start_position().row, name_node.start_position().column,
                    "class", vis,
                ));
                if let Some(body) = node.child_by_field_name("body") {
                    collect_definitions_recursive(body, source, symbols, Some(&name));
                }
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = java_visibility(node, source);
                symbols.push(make_symbol(
                    name.clone(), SymbolKind::Interface,
                    name_node.start_position().row, name_node.start_position().column,
                    "interface", vis,
                ));
                if let Some(body) = node.child_by_field_name("body") {
                    collect_definitions_recursive(body, source, symbols, Some(&name));
                }
            }
        }
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = java_visibility(node, source);
                symbols.push(make_symbol(
                    name.clone(), SymbolKind::Enum,
                    name_node.start_position().row, name_node.start_position().column,
                    "enum", vis,
                ));
                if let Some(body) = node.child_by_field_name("body") {
                    extract_enum_constants(body, source, symbols, &name);
                    collect_definitions_recursive(body, source, symbols, Some(&name));
                }
            }
        }
        "record_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = java_visibility(node, source);
                symbols.push(make_symbol(
                    name.clone(), SymbolKind::Struct,
                    name_node.start_position().row, name_node.start_position().column,
                    "record", vis,
                ));
                if let Some(body) = node.child_by_field_name("body") {
                    collect_definitions_recursive(body, source, symbols, Some(&name));
                }
            }
        }
        "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = java_visibility(node, source);
                symbols.push(make_contained_symbol(
                    name, SymbolKind::Method,
                    name_node.start_position().row, name_node.start_position().column,
                    "method", vis, container, 1, None, None,
                ));
            }
        }
        "constructor_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = java_visibility(node, source);
                symbols.push(make_contained_symbol(
                    name, SymbolKind::Function,
                    name_node.start_position().row, name_node.start_position().column,
                    "constructor", vis, container, 1, None, None,
                ));
            }
        }
        "field_declaration" => {
            let vis = java_visibility(node, source);
            let type_text = node.child_by_field_name("type")
                .map(|t| node_text(t, source).to_string());
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "variable_declarator" {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let name = node_text(name_node, source).to_string();
                        symbols.push(make_contained_symbol(
                            name, SymbolKind::Variable,
                            name_node.start_position().row, name_node.start_position().column,
                            "field", vis, container, 1, None, type_text.clone(),
                        ));
                    }
                }
            }
        }
        "annotation_type_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = java_visibility(node, source);
                symbols.push(make_symbol(
                    name, SymbolKind::Interface,
                    name_node.start_position().row, name_node.start_position().column,
                    "annotation", vis,
                ));
            }
        }
        _ => {}
    }
}

fn extract_enum_constants(body: Node, source: &str, symbols: &mut Vec<Symbol>, enum_name: &str) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == "enum_constant" {
            if let Some(name_node) = child.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(make_contained_symbol(
                    name, SymbolKind::Constant,
                    name_node.start_position().row, name_node.start_position().column,
                    "enum", Visibility::Public, Some(enum_name), 1, None, None,
                ));
            }
        }
        // enum_body_declarations contains methods/fields after the constants
        if child.kind() == "enum_body_declarations" {
            collect_definitions_recursive(child, source, symbols, Some(enum_name));
        }
    }
}

/// Detect Java visibility from modifiers.
fn java_visibility(node: Node, source: &str) -> Visibility {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let text = node_text(child, source);
            if text.contains("public") {
                return Visibility::Public;
            } else if text.contains("private") {
                return Visibility::Private;
            } else if text.contains("protected") {
                return Visibility::Unknown;
            }
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
package com.example;

public class Config {
    private String name;
    public int value;

    public Config(String name) {
        this.name = name;
    }

    public String getName() {
        return name;
    }

    private void helper() {}
}

interface Handler {
    void handle();
}

enum Status {
    ACTIVE,
    INACTIVE;

    public String label() { return name(); }
}
"#;
        let result = JavaParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"Config"), "should find class Config, got: {:?}", names);
        assert!(names.contains(&"name"), "should find field name, got: {:?}", names);
        assert!(names.contains(&"value"), "should find field value, got: {:?}", names);
        assert!(names.contains(&"getName"), "should find method getName, got: {:?}", names);
        assert!(names.contains(&"helper"), "should find method helper, got: {:?}", names);
        assert!(names.contains(&"Handler"), "should find interface Handler, got: {:?}", names);
        assert!(names.contains(&"handle"), "should find interface method handle, got: {:?}", names);
        assert!(names.contains(&"Status"), "should find enum Status, got: {:?}", names);
        assert!(names.contains(&"ACTIVE"), "should find enum constant ACTIVE, got: {:?}", names);
        assert!(names.contains(&"INACTIVE"), "should find enum constant INACTIVE, got: {:?}", names);
        assert!(names.contains(&"label"), "should find enum method label, got: {:?}", names);

        // Check visibility
        let get_name = result.symbols.iter().find(|s| s.name == "getName").unwrap();
        assert_eq!(get_name.visibility, Visibility::Public);

        let helper_sym = result.symbols.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(helper_sym.visibility, Visibility::Private);

        // Check container
        let name_field = result.symbols.iter().find(|s| s.name == "name" && s.def_keyword == "field").unwrap();
        assert_eq!(name_field.container.as_deref(), Some("Config"));

        assert!(!result.occurrences.is_empty());
    }
}
