//! Tree-sitter based Go parser.
//!
//! Extracts: functions, methods, type declarations (structs, interfaces,
//! type aliases), var/const declarations, struct fields, and interface methods.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{self, make_contained_symbol, make_symbol, node_text};
use super::{ParseResult, TsParser};

const GO_IDENT_KINDS: &[&str] = &["identifier", "type_identifier", "field_identifier", "package_identifier"];

pub struct GoParser;

impl TsParser for GoParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
        common::run_parse(source, &lang, GO_IDENT_KINDS, collect_definitions)
    }
}

fn collect_definitions(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        extract_definition(child, source, symbols);
    }
}

fn extract_definition(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    match node.kind() {
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = go_visibility(&name);
                symbols.push(make_symbol(
                    name, SymbolKind::Function,
                    name_node.start_position().row, name_node.start_position().column,
                    "func", vis,
                ));
            }
        }
        "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = go_visibility(&name);
                let container = node.child_by_field_name("receiver")
                    .and_then(|r| extract_receiver_type(r, source));
                symbols.push(make_contained_symbol(
                    name, SymbolKind::Method,
                    name_node.start_position().row, name_node.start_position().column,
                    "method", vis, container.as_deref(), 1, None, None,
                ));
            }
        }
        "type_declaration" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "type_spec" {
                    extract_type_spec(child, source, symbols);
                }
            }
        }
        "var_declaration" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "var_spec" {
                    extract_var_spec(child, source, symbols, "var");
                }
            }
        }
        "const_declaration" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "const_spec" {
                    extract_var_spec(child, source, symbols, "const");
                }
            }
        }
        _ => {}
    }
}

fn extract_type_spec(node: Node, source: &str, symbols: &mut Vec<Symbol>) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = node_text(name_node, source).to_string();
    let vis = go_visibility(&name);

    let type_node = node.child_by_field_name("type");
    let (kind, kw) = match type_node.map(|t| t.kind()) {
        Some("struct_type") => (SymbolKind::Struct, "struct"),
        Some("interface_type") => (SymbolKind::Interface, "interface"),
        _ => (SymbolKind::TypeAlias, "type"),
    };

    symbols.push(make_symbol(
        name.clone(), kind,
        name_node.start_position().row, name_node.start_position().column,
        kw, vis,
    ));

    if let Some(type_node) = type_node {
        match type_node.kind() {
            "struct_type" => {
                // struct fields are in field_declaration_list
                let mut cursor = type_node.walk();
                for child in type_node.children(&mut cursor) {
                    if child.kind() == "field_declaration_list" {
                        extract_struct_fields(child, source, symbols, &name);
                    }
                }
            }
            "interface_type" => {
                extract_interface_methods(type_node, source, symbols, &name);
            }
            _ => {}
        }
    }
}

fn extract_var_spec(node: Node, source: &str, symbols: &mut Vec<Symbol>, kw: &str) {
    let kind = if kw == "const" { SymbolKind::Constant } else { SymbolKind::Variable };
    // name field holds the identifiers
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            let name = node_text(child, source).to_string();
            let vis = go_visibility(&name);
            symbols.push(make_symbol(
                name, kind.clone(),
                child.start_position().row, child.start_position().column,
                kw, vis,
            ));
        }
    }
}

fn extract_struct_fields(body: Node, source: &str, symbols: &mut Vec<Symbol>, struct_name: &str) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == "field_declaration" {
            let type_text = child.child_by_field_name("type")
                .map(|t| node_text(t, source).to_string());
            let mut name_cursor = child.walk();
            for field_child in child.children(&mut name_cursor) {
                if field_child.kind() == "field_identifier" {
                    let name = node_text(field_child, source).to_string();
                    let vis = go_visibility(&name);
                    symbols.push(make_contained_symbol(
                        name, SymbolKind::Variable,
                        field_child.start_position().row, field_child.start_position().column,
                        "field", vis, Some(struct_name), 1, None, type_text.clone(),
                    ));
                }
            }
        }
    }
}

fn extract_interface_methods(iface: Node, source: &str, symbols: &mut Vec<Symbol>, iface_name: &str) {
    let mut cursor = iface.walk();
    for child in iface.children(&mut cursor) {
        // Go interface methods are `method_elem` nodes (not `method_spec`)
        if child.kind() == "method_elem" || child.kind() == "method_spec" {
            if let Some(name_node) = child.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = go_visibility(&name);
                symbols.push(make_contained_symbol(
                    name, SymbolKind::Method,
                    name_node.start_position().row, name_node.start_position().column,
                    "method", vis, Some(iface_name), 1, None, None,
                ));
            }
        }
    }
}

fn extract_receiver_type(receiver: Node, source: &str) -> Option<String> {
    let mut cursor = receiver.walk();
    for child in receiver.children(&mut cursor) {
        if child.kind() == "parameter_declaration" {
            if let Some(type_node) = child.child_by_field_name("type") {
                let text = node_text(type_node, source);
                return Some(text.trim_start_matches('*').to_string());
            }
        }
    }
    None
}

/// Go visibility: uppercase first letter = exported (Public), lowercase = unexported (Private).
fn go_visibility(name: &str) -> Visibility {
    if name.starts_with(|c: char| c.is_uppercase()) {
        Visibility::Public
    } else {
        Visibility::Private
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_go_parser_basic() {
        let source = r#"
package main

import "fmt"

func Hello(name string) string {
    return "Hello, " + name
}

func helper() {}

type Config struct {
    Name  string
    value int
}

func (c *Config) GetName() string {
    return c.Name
}

type Handler interface {
    Handle()
}

type StringAlias = string

var GlobalVar int = 42

const MaxSize = 100
"#;
        let result = GoParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"Hello"), "should find func Hello, got: {:?}", names);
        assert!(names.contains(&"helper"), "should find func helper, got: {:?}", names);
        assert!(names.contains(&"Config"), "should find struct Config, got: {:?}", names);
        assert!(names.contains(&"Name"), "should find field Name, got: {:?}", names);
        assert!(names.contains(&"GetName"), "should find method GetName, got: {:?}", names);
        assert!(names.contains(&"Handler"), "should find interface Handler, got: {:?}", names);
        assert!(names.contains(&"Handle"), "should find interface method Handle, got: {:?}", names);
        assert!(names.contains(&"GlobalVar"), "should find var GlobalVar, got: {:?}", names);
        assert!(names.contains(&"MaxSize"), "should find const MaxSize, got: {:?}", names);

        // Check visibility
        let hello_sym = result.symbols.iter().find(|s| s.name == "Hello").unwrap();
        assert_eq!(hello_sym.visibility, Visibility::Public);

        let helper_sym = result.symbols.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(helper_sym.visibility, Visibility::Private);

        // Check method container
        let get_name = result.symbols.iter().find(|s| s.name == "GetName").unwrap();
        assert_eq!(get_name.container.as_deref(), Some("Config"));

        assert!(!result.occurrences.is_empty());
    }
}
