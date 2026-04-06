//! Tree-sitter based JavaScript parser.
//!
//! Extracts: functions, classes, methods, variable declarations (const/let/var),
//! and export statements.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{self, make_contained_symbol, make_symbol, node_text};
use super::{ParseResult, TsParser};

const JS_IDENT_KINDS: &[&str] = &["identifier", "property_identifier", "shorthand_property_identifier"];

pub struct JsParser;

impl TsParser for JsParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
        common::run_parse(source, &lang, JS_IDENT_KINDS, collect_definitions)
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
                // Class body is class_body
                if let Some(body) = node.child_by_field_name("body") {
                    collect_definitions_recursive(body, source, symbols, Some(&name), false);
                }
            }
        }
        "method_definition" => {
            // Method name is property_identifier accessed via "name" field
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(make_contained_symbol(
                    name, SymbolKind::Method,
                    name_node.start_position().row, name_node.start_position().column,
                    "method", Visibility::Unknown, container, 1, None, None,
                ));
            }
        }
        "lexical_declaration" | "variable_declaration" => {
            let kw = if node.kind() == "lexical_declaration" {
                // First child is "const" or "let" keyword
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
            // Export wraps declarations — use @declaration field or iterate children
            if let Some(decl) = node.child_by_field_name("declaration") {
                extract_definition(decl, source, symbols, container, true);
            } else {
                // Fallback: iterate children looking for declarations
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    match child.kind() {
                        "function_declaration" | "generator_function_declaration"
                        | "class_declaration" | "lexical_declaration" | "variable_declaration" => {
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
var legacy = true;

class Config {
    constructor(name) {
        this.name = name;
    }

    getName() {
        return this.name;
    }
}

export function exported() {}
export const API_KEY = "abc";
"#;
        let result = JsParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"hello"), "should find function hello, got: {:?}", names);
        assert!(names.contains(&"MAX_SIZE"), "should find const MAX_SIZE, got: {:?}", names);
        assert!(names.contains(&"counter"), "should find let counter, got: {:?}", names);
        assert!(names.contains(&"legacy"), "should find var legacy, got: {:?}", names);
        assert!(names.contains(&"Config"), "should find class Config, got: {:?}", names);
        assert!(names.contains(&"constructor"), "should find constructor, got: {:?}", names);
        assert!(names.contains(&"getName"), "should find method getName, got: {:?}", names);
        assert!(names.contains(&"exported"), "should find exported function, got: {:?}", names);
        assert!(names.contains(&"API_KEY"), "should find exported const, got: {:?}", names);

        // Check visibility
        let exported_sym = result.symbols.iter().find(|s| s.name == "exported").unwrap();
        assert_eq!(exported_sym.visibility, Visibility::Public);

        let hello_sym = result.symbols.iter().find(|s| s.name == "hello").unwrap();
        assert_eq!(hello_sym.visibility, Visibility::Unknown);

        // Check method container
        let get_name = result.symbols.iter().find(|s| s.name == "getName").unwrap();
        assert_eq!(get_name.container.as_deref(), Some("Config"));

        assert!(!result.occurrences.is_empty());
    }
}
