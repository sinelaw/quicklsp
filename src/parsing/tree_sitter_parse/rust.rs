//! Tree-sitter based Rust parser.
//!
//! Extracts: functions, structs, enums, traits, type aliases, constants,
//! statics, modules, impl methods, enum variants, and struct fields.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{self, find_child_by_kind, make_contained_symbol, make_symbol, node_text};
use super::{ParseResult, TsParser};

const RUST_IDENT_KINDS: &[&str] = &["identifier", "type_identifier", "field_identifier"];

pub struct RustParser;

impl TsParser for RustParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        common::run_parse(source, &lang, RUST_IDENT_KINDS, collect_definitions)
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
        "function_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = detect_visibility(node, source);
                let kind = if container.is_some() { SymbolKind::Method } else { SymbolKind::Function };
                let kw = if container.is_some() { "method" } else { "fn" };
                if let Some(c) = container {
                    symbols.push(make_contained_symbol(
                        name, kind,
                        name_node.start_position().row, name_node.start_position().column,
                        kw, vis, Some(c), 1, None, None,
                    ));
                } else {
                    symbols.push(make_symbol(
                        name, kind,
                        name_node.start_position().row, name_node.start_position().column,
                        kw, vis,
                    ));
                }
            }
        }
        // Trait method signatures (declarations without body)
        "function_signature_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = detect_visibility(node, source);
                symbols.push(make_contained_symbol(
                    name, SymbolKind::Method,
                    name_node.start_position().row, name_node.start_position().column,
                    "method", vis, container, 1, None, None,
                ));
            }
        }
        "struct_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = detect_visibility(node, source);
                symbols.push(make_symbol(
                    name.clone(), SymbolKind::Struct,
                    name_node.start_position().row, name_node.start_position().column,
                    "struct", vis,
                ));
                // Extract fields from field_declaration_list
                if let Some(body) = find_child_by_kind(node, "field_declaration_list") {
                    extract_struct_fields(body, source, symbols, &name);
                }
            }
        }
        "enum_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = detect_visibility(node, source);
                symbols.push(make_symbol(
                    name.clone(), SymbolKind::Enum,
                    name_node.start_position().row, name_node.start_position().column,
                    "enum", vis,
                ));
                // Extract variants from enum_variant_list
                if let Some(body) = find_child_by_kind(node, "enum_variant_list") {
                    extract_enum_variants(body, source, symbols, &name);
                }
            }
        }
        "trait_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = detect_visibility(node, source);
                symbols.push(make_symbol(
                    name.clone(), SymbolKind::Trait,
                    name_node.start_position().row, name_node.start_position().column,
                    "trait", vis,
                ));
                // Recurse into declaration_list for trait methods
                if let Some(body) = find_child_by_kind(node, "declaration_list") {
                    collect_definitions_recursive(body, source, symbols, Some(&name));
                }
            }
        }
        "impl_item" => {
            // impl blocks: extract the type name as the container for methods
            let impl_name = node.child_by_field_name("type")
                .map(|t| node_text(t, source).to_string());
            if let Some(body) = find_child_by_kind(node, "declaration_list") {
                collect_definitions_recursive(body, source, symbols, impl_name.as_deref());
            }
        }
        "type_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = detect_visibility(node, source);
                symbols.push(make_symbol(
                    name, SymbolKind::TypeAlias,
                    name_node.start_position().row, name_node.start_position().column,
                    "type", vis,
                ));
            }
        }
        "const_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = detect_visibility(node, source);
                if let Some(c) = container {
                    symbols.push(make_contained_symbol(
                        name, SymbolKind::Constant,
                        name_node.start_position().row, name_node.start_position().column,
                        "const", vis, Some(c), 1, None, None,
                    ));
                } else {
                    symbols.push(make_symbol(
                        name, SymbolKind::Constant,
                        name_node.start_position().row, name_node.start_position().column,
                        "const", vis,
                    ));
                }
            }
        }
        "static_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = detect_visibility(node, source);
                symbols.push(make_symbol(
                    name, SymbolKind::Variable,
                    name_node.start_position().row, name_node.start_position().column,
                    "static", vis,
                ));
            }
        }
        "mod_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = detect_visibility(node, source);
                symbols.push(make_symbol(
                    name, SymbolKind::Module,
                    name_node.start_position().row, name_node.start_position().column,
                    "mod", vis,
                ));
                // Recurse into inline module body (declaration_list)
                if let Some(body) = find_child_by_kind(node, "declaration_list") {
                    collect_definitions_recursive(body, source, symbols, None);
                }
            }
        }
        "macro_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(make_symbol(
                    name, SymbolKind::Function,
                    name_node.start_position().row, name_node.start_position().column,
                    "macro", Visibility::Unknown,
                ));
            }
        }
        _ => {}
    }
}

fn detect_visibility(node: Node, source: &str) -> Visibility {
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

fn extract_struct_fields(body: Node, source: &str, symbols: &mut Vec<Symbol>, struct_name: &str) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == "field_declaration" {
            if let Some(name_node) = child.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = detect_visibility(child, source);
                let type_text = child.child_by_field_name("type")
                    .map(|t| node_text(t, source).to_string());
                symbols.push(make_contained_symbol(
                    name, SymbolKind::Variable,
                    name_node.start_position().row, name_node.start_position().column,
                    "field", vis, Some(struct_name), 1, None, type_text,
                ));
            }
        }
    }
}

fn extract_enum_variants(body: Node, source: &str, symbols: &mut Vec<Symbol>, enum_name: &str) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == "enum_variant" {
            if let Some(name_node) = child.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(make_contained_symbol(
                    name, SymbolKind::Constant,
                    name_node.start_position().row, name_node.start_position().column,
                    "variant", Visibility::Unknown, Some(enum_name), 1, None, None,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        assert!(names.contains(&"hello"), "should find fn hello, got: {:?}", names);
        assert!(names.contains(&"Config"), "should find struct Config, got: {:?}", names);
        assert!(names.contains(&"name"), "should find field name, got: {:?}", names);
        assert!(names.contains(&"Status"), "should find enum Status, got: {:?}", names);
        assert!(names.contains(&"Active"), "should find variant Active, got: {:?}", names);
        assert!(names.contains(&"Handler"), "should find trait Handler, got: {:?}", names);
        assert!(names.contains(&"handle"), "should find trait method handle, got: {:?}", names);
        assert!(names.contains(&"Result"), "should find type alias Result, got: {:?}", names);
        assert!(names.contains(&"MAX"), "should find const MAX, got: {:?}", names);
        assert!(names.contains(&"GLOBAL"), "should find static GLOBAL, got: {:?}", names);
        assert!(names.contains(&"utils"), "should find mod utils, got: {:?}", names);
        assert!(names.contains(&"helper"), "should find nested fn helper, got: {:?}", names);
        assert!(names.contains(&"new"), "should find impl method new, got: {:?}", names);
        assert!(names.contains(&"my_macro"), "should find macro my_macro, got: {:?}", names);

        // Check visibility
        let hello_sym = result.symbols.iter().find(|s| s.name == "hello").unwrap();
        assert_eq!(hello_sym.visibility, Visibility::Public);

        // Check impl method container
        let new_sym = result.symbols.iter().find(|s| s.name == "new").unwrap();
        assert_eq!(new_sym.container.as_deref(), Some("Config"));

        assert!(!result.occurrences.is_empty(), "should have occurrences");
    }
}
