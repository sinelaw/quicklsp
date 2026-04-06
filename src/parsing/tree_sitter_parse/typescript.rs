//! Tree-sitter based TypeScript parser.
//!
//! Extends JavaScript parsing with TypeScript-specific constructs:
//! interfaces, type aliases, enums, and type annotations.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{self, make_contained_symbol, make_symbol, node_text};
use super::{ParseResult, TsParser};

const TS_IDENT_KINDS: &[&str] = &[
    "identifier",
    "property_identifier",
    "shorthand_property_identifier",
    "type_identifier",
];

pub struct TypeScriptParser;

impl TsParser for TypeScriptParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        common::run_parse(source, &lang, TS_IDENT_KINDS, collect_definitions)
    }
}

pub struct TsxParser;

impl TsParser for TsxParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TSX.into();
        common::run_parse(source, &lang, TS_IDENT_KINDS, collect_definitions)
    }
}

fn collect_definitions(root: Node, source: &str, symbols: &mut Vec<Symbol>) {
    collect_definitions_recursive(root, source, symbols, None, false);
}

fn collect_definitions_recursive(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
    is_exported: bool,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        extract_definition(child, source, symbols, container, is_exported);
    }
}

fn extract_definition(
    node: Node,
    source: &str,
    symbols: &mut Vec<Symbol>,
    container: Option<&str>,
    is_exported: bool,
) {
    match node.kind() {
        "function_declaration" | "generator_function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = if is_exported { Visibility::Public } else { Visibility::Unknown };
                symbols.push(make_symbol(
                    name, SymbolKind::Function,
                    name_node.start_position().row, name_node.start_position().column,
                    "function", vis,
                ));
            }
        }
        "class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = if is_exported { Visibility::Public } else { Visibility::Unknown };
                symbols.push(make_symbol(
                    name.clone(), SymbolKind::Class,
                    name_node.start_position().row, name_node.start_position().column,
                    "class", vis,
                ));
                if let Some(body) = node.child_by_field_name("body") {
                    collect_definitions_recursive(body, source, symbols, Some(&name), false);
                }
            }
        }
        "method_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(make_contained_symbol(
                    name, SymbolKind::Method,
                    name_node.start_position().row, name_node.start_position().column,
                    "method", Visibility::Unknown, container, 1, None, None,
                ));
            }
        }
        "public_field_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(make_contained_symbol(
                    name, SymbolKind::Variable,
                    name_node.start_position().row, name_node.start_position().column,
                    "field", Visibility::Unknown, container, 1, None, None,
                ));
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = if is_exported { Visibility::Public } else { Visibility::Unknown };
                symbols.push(make_symbol(
                    name.clone(), SymbolKind::Interface,
                    name_node.start_position().row, name_node.start_position().column,
                    "interface", vis,
                ));
                // Interface body is interface_body (accessed via "body" field)
                if let Some(body) = node.child_by_field_name("body") {
                    extract_interface_members(body, source, symbols, &name);
                }
            }
        }
        "type_alias_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = if is_exported { Visibility::Public } else { Visibility::Unknown };
                symbols.push(make_symbol(
                    name, SymbolKind::TypeAlias,
                    name_node.start_position().row, name_node.start_position().column,
                    "type", vis,
                ));
            }
        }
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = if is_exported { Visibility::Public } else { Visibility::Unknown };
                symbols.push(make_symbol(
                    name.clone(), SymbolKind::Enum,
                    name_node.start_position().row, name_node.start_position().column,
                    "enum", vis,
                ));
                // Enum body is enum_body (accessed via "body" field)
                if let Some(body) = node.child_by_field_name("body") {
                    extract_enum_members(body, source, symbols, &name);
                }
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            let kw = if node.kind() == "lexical_declaration" {
                node.child(0).map(|c| node_text(c, source)).unwrap_or("let")
            } else {
                "var"
            };
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "variable_declarator" {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        if name_node.kind() == "identifier" {
                            let name = node_text(name_node, source).to_string();
                            let vis = if is_exported { Visibility::Public } else { Visibility::Unknown };
                            let kind = if kw == "const" { SymbolKind::Constant } else { SymbolKind::Variable };
                            symbols.push(make_symbol(
                                name, kind,
                                name_node.start_position().row, name_node.start_position().column,
                                kw, vis,
                            ));
                        }
                    }
                }
            }
        }
        "export_statement" => {
            if let Some(decl) = node.child_by_field_name("declaration") {
                extract_definition(decl, source, symbols, container, true);
            } else {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    match child.kind() {
                        "function_declaration" | "generator_function_declaration"
                        | "class_declaration" | "lexical_declaration" | "variable_declaration"
                        | "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
                            extract_definition(child, source, symbols, container, true);
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
}

fn extract_interface_members(body: Node, source: &str, symbols: &mut Vec<Symbol>, iface_name: &str) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "method_signature" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = node_text(name_node, source).to_string();
                    symbols.push(make_contained_symbol(
                        name, SymbolKind::Method,
                        name_node.start_position().row, name_node.start_position().column,
                        "method", Visibility::Unknown, Some(iface_name), 1, None, None,
                    ));
                }
            }
            "property_signature" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = node_text(name_node, source).to_string();
                    symbols.push(make_contained_symbol(
                        name, SymbolKind::Variable,
                        name_node.start_position().row, name_node.start_position().column,
                        "field", Visibility::Unknown, Some(iface_name), 1, None, None,
                    ));
                }
            }
            _ => {}
        }
    }
}

fn extract_enum_members(body: Node, source: &str, symbols: &mut Vec<Symbol>, enum_name: &str) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        // Enum members: look for property_identifier inside enum_member or directly
        if child.kind() == "enum_member" || child.kind() == "property_identifier" {
            let name_node = child.child_by_field_name("name").unwrap_or(child);
            if name_node.kind() == "property_identifier" || name_node.kind() == "identifier" {
                let name = node_text(name_node, source).to_string();
                symbols.push(make_contained_symbol(
                    name, SymbolKind::Constant,
                    name_node.start_position().row, name_node.start_position().column,
                    "enum", Visibility::Unknown, Some(enum_name), 1, None, None,
                ));
            }
        }
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

interface Handler {
    handle(req: Request): Response;
    name: string;
}

type Result<T> = { ok: true; value: T } | { ok: false; error: Error };

enum Status {
    Active,
    Inactive,
}

const MAX_SIZE: number = 100;

class Config {
    name: string;

    constructor(name: string) {
        this.name = name;
    }

    getName(): string {
        return this.name;
    }
}

export function exported(): void {}
export interface PublicApi {
    fetch(): void;
}
export type ID = string;
"#;
        let result = TypeScriptParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"hello"), "should find function hello, got: {:?}", names);
        assert!(names.contains(&"Handler"), "should find interface Handler, got: {:?}", names);
        assert!(names.contains(&"handle"), "should find interface method handle, got: {:?}", names);
        assert!(names.contains(&"Result"), "should find type alias Result, got: {:?}", names);
        assert!(names.contains(&"Status"), "should find enum Status, got: {:?}", names);
        assert!(names.contains(&"MAX_SIZE"), "should find const MAX_SIZE, got: {:?}", names);
        assert!(names.contains(&"Config"), "should find class Config, got: {:?}", names);
        assert!(names.contains(&"getName"), "should find method getName, got: {:?}", names);
        assert!(names.contains(&"exported"), "should find exported function, got: {:?}", names);
        assert!(names.contains(&"PublicApi"), "should find exported interface, got: {:?}", names);
        assert!(names.contains(&"ID"), "should find exported type alias, got: {:?}", names);

        // Check visibility
        let exported_sym = result.symbols.iter().find(|s| s.name == "exported").unwrap();
        assert_eq!(exported_sym.visibility, Visibility::Public);

        assert!(!result.occurrences.is_empty());
    }
}
