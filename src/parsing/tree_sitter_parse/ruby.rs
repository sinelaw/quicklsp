//! Tree-sitter based Ruby parser.
//!
//! Extracts: methods, classes, modules, singleton methods, and module-level
//! constant assignments.

use tree_sitter::Node;

use crate::parsing::symbols::{Symbol, SymbolKind};
use crate::parsing::tokenizer::Visibility;

use super::common::{self, find_child_by_kind, make_contained_symbol, make_symbol, node_text};
use super::{ParseResult, TsParser};

const RUBY_IDENT_KINDS: &[&str] = &["identifier", "constant"];

pub struct RubyParser;

impl TsParser for RubyParser {
    fn parse(source: &str) -> ParseResult {
        let lang: tree_sitter::Language = tree_sitter_ruby::LANGUAGE.into();
        common::run_parse(source, &lang, RUBY_IDENT_KINDS, collect_definitions)
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
        "method" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                let vis = ruby_visibility(&name);
                let (kind, kw) = if container.is_some() {
                    (SymbolKind::Method, "method")
                } else {
                    (SymbolKind::Function, "def")
                };
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
        "singleton_method" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(make_contained_symbol(
                    name, SymbolKind::Method,
                    name_node.start_position().row, name_node.start_position().column,
                    "method", Visibility::Public, container, 1, None, None,
                ));
            }
        }
        "class" => {
            // Class name is a "constant" node accessed via "name" field
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(make_symbol(
                    name.clone(), SymbolKind::Class,
                    name_node.start_position().row, name_node.start_position().column,
                    "class", Visibility::Unknown,
                ));
                // Class body is in body_statement
                if let Some(body) = find_child_by_kind(node, "body_statement") {
                    collect_definitions_recursive(body, source, symbols, Some(&name));
                }
            }
        }
        "module" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source).to_string();
                symbols.push(make_symbol(
                    name.clone(), SymbolKind::Module,
                    name_node.start_position().row, name_node.start_position().column,
                    "module", Visibility::Unknown,
                ));
                if let Some(body) = find_child_by_kind(node, "body_statement") {
                    collect_definitions_recursive(body, source, symbols, Some(&name));
                }
            }
        }
        "assignment" => {
            // Constant assignments: `MAX = 100` where left side is a constant node
            if let Some(left) = node.child_by_field_name("left") {
                if left.kind() == "constant" {
                    let name = node_text(left, source).to_string();
                    symbols.push(make_symbol(
                        name, SymbolKind::Constant,
                        left.start_position().row, left.start_position().column,
                        "const", Visibility::Unknown,
                    ));
                }
            }
        }
        _ => {}
    }
}

fn ruby_visibility(name: &str) -> Visibility {
    if name.starts_with('_') {
        Visibility::Private
    } else {
        Visibility::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ruby_parser_basic() {
        let source = r#"
MAX_SIZE = 100

def hello(name)
  "Hello, #{name}"
end

module Utils
  def self.helper
    42
  end
end

class Config
  def initialize(name)
    @name = name
  end

  def get_name
    @name
  end

  def self.create(name)
    new(name)
  end

  def _internal
    nil
  end
end

class ChildConfig < Config
  def extra
    true
  end
end
"#;
        let result = RubyParser::parse(source);
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"MAX_SIZE"), "should find constant MAX_SIZE, got: {:?}", names);
        assert!(names.contains(&"hello"), "should find function hello, got: {:?}", names);
        assert!(names.contains(&"Utils"), "should find module Utils, got: {:?}", names);
        assert!(names.contains(&"helper"), "should find singleton method helper, got: {:?}", names);
        assert!(names.contains(&"Config"), "should find class Config, got: {:?}", names);
        assert!(names.contains(&"initialize"), "should find method initialize, got: {:?}", names);
        assert!(names.contains(&"get_name"), "should find method get_name, got: {:?}", names);
        assert!(names.contains(&"create"), "should find singleton method create, got: {:?}", names);
        assert!(names.contains(&"ChildConfig"), "should find class ChildConfig, got: {:?}", names);
        assert!(names.contains(&"extra"), "should find method extra, got: {:?}", names);

        // Check container
        let init_sym = result.symbols.iter().find(|s| s.name == "initialize").unwrap();
        assert_eq!(init_sym.container.as_deref(), Some("Config"));

        // Check visibility
        let internal_sym = result.symbols.iter().find(|s| s.name == "_internal").unwrap();
        assert_eq!(internal_sym.visibility, Visibility::Private);

        assert!(!result.occurrences.is_empty());
    }
}
